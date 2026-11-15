//! `wsp check <task>` — a task's mechanical verification, run as a script: `wsp-324`.
//!
//! Ed, 2026-11-01. Of 405 verifier transcripts since 15 October, verifiers
//! spent 22% of their calls building and running tests, and only 23% of the
//! sessions that blocked saw a failing test or a compile error. Building and
//! running what a project already knows how to build and run needs no agent.
//!
//! # Three tiers, because the whole suite on every small change costs too much
//!
//! Ed, 2026-11-01: wsp's full verify is about nine minutes, and running a
//! pre-declared suite on every row is the cost this exists to remove. So a row
//! is checked by what its change needs, and the full suite is the barrier's.
//!
//! 1. **The member's own checks** — a `## Checks` section on its row, written
//!    at `wsp review`: the exact commands that verify its change, the targeted
//!    tests and any script whose numbers it reported. `wsp check` replays them
//!    at the landed commit, so "I ran it and it was green" is checked rather
//!    than believed (compounding-025's stale test: no leg reran the suite).
//!    `- command` is a step; `- name (limit): command` names it.
//! 2. **A minimum from the changed paths** — the project's `## Checks` holds
//!    rules, `- name (limit) on <glob>[, <glob>…]: command`, and a rule joins
//!    the run when a path the landing changed matches. The member's list cannot
//!    be too narrow, because the diff, not the member, says what is owed. A rule
//!    with no `on` runs for every landing, which is for what is cheap.
//! 3. **The full suite at the barrier**, once per group, not per row:
//!    `- name (limit) at barrier: command`. [`barrier_steps`] names them and
//!    `wsp check <project> --barrier` runs them by hand at the trunk; the
//!    barrier itself runs them with every member's checks and the stop's, in
//!    [`crate::barrier_run`] (`wsp-326`).
//!
//! A step passes when its command exits 0, and the limit is `90s`, `20m` or
//! `2h` (ten minutes without one). wsp knows nothing about any project: it runs
//! what is declared. There is no inheritance either — compound's parent is wsp,
//! and a compound row has no business running wsp's `cargo test`.
//!
//! Which paths a landing changed is read from what `wsp land` wrote on the row
//! (`since <commit>`), or, for a row landed before it did, from the commits on
//! the trunk that name the row's id.
//!
//! # Where it runs: a warm tree, at the commit that landed
//!
//! A tree of its own per repository, under the state directory
//! (`checks/<project>-<repo>/tree`), moved to the landing with `git checkout
//! --detach` and left standing so the next check's build is incremental. It is
//! not under `.worktrees/`: `checkout --sweep`, `check_tree` and `leg_tree`
//! each own what is there, and none of them should find this. One check holds
//! it at a time — a lock beside it — because two builds in one tree is how a
//! linker segfaults and a green run is bought for the wrong tree.
//!
//! **A new tree starts warm, and the record says how it started** (`wsp-566`).
//! The first check of a list built for 768s against 20-95s for the later ones,
//! and nothing said whether its `target/` had started cold or the machine was
//! busy. [`checkout_started`] clones the trunk's `target/` into a tree that is
//! new or has lost its own (`warm::clone_from`, one `clonefile(2)`, bounded at
//! 90s, as a row's tree is), and returns a [`Start`]: `cloned`, `warm`, `cold`
//! (no built target and nothing to clone) or `none` (no cargo), with the
//! one-minute load and the cores. It is written on the `task-checked` event
//! (`start`, `load_end`, `step_secs`), on the barrier's `barrier run —` log
//! line (` · start:`), and from both into the timeline phase labels that reach
//! `effort.json`, so a retro can tell a cold start from a slow machine. The
//! cold build (`cold.rs`) is cold by design and takes no seed.
//!
//! The commands run with every `WSP_*` and `herdr` variable removed. A test
//! suite that finds `WSP_HOME` set fails four store tests and reads as the
//! change under check being wrong.
//!
//! # The result, on the row
//!
//! One entry under `## Verification`, beside the verifiers' ([`crate::verification::Run`]):
//! each step, pass or fail, how long, and the tail of the one that failed. A
//! run is written `running` before anything starts, under the store lock, and
//! that entry is what keeps a second trigger from starting a second run.
//!
//! # By itself after a land
//!
//! [`gate`] sits in front of the verifier: a member that has landed and has
//! checks gets no verifier until a run for that landing has **passed**. A
//! failure sends the member back to `doing` in the same write that records it,
//! with the failing step as what is owed, so there is no instant at which a
//! failed landing reads as finished. `wsp land` starts the run too, for a row
//! that is in no list.
//!
//! A run that failed is not run again for the same landing: it has had its
//! say, and a member returned to review at the same commit has been answered
//! by a person. `wsp check <task>` by hand runs it again whenever asked.

pub(crate) use crate::model::project_of;
pub(crate) use crate::util::{captures, clean_env};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use wsp_base::calls::Command;
use std::time::{Duration, Instant};

use crate::cmd_verify::{git, git_ok};
use crate::greens::{self, Green, Reuse};
use crate::model::{Status, Task};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::verification::{self as v, Run, RunState};
use crate::Args;

/// The heading a project or a task lists its checks under.
pub(crate) const SECTION: &str = "Checks";

const DEFAULT_LIMIT: Duration = Duration::from_secs(600);

/// How long a run waits for the tree before giving it up for the next trigger.
pub(crate) const TREE_WAIT: Duration = Duration::from_secs(45 * 60);

/// The longest a tail of output is kept on the row, in lines and in characters.
const TAIL_LINES: usize = 25;
const TAIL_COLS: usize = 200;

/// Where a step came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Of {
    /// The member's own `## Checks`.
    Task,
    /// A project rule a changed path matched.
    Rule,
    /// A project rule with no paths: every landing.
    Project,
}

/// One declared check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub name: String,
    pub command: String,
    pub limit: Duration,
    pub of: Of,
    /// The globs of a project rule; none for a step that is not path-driven.
    pub on: Vec<String>,
    /// Run by the barrier, not per row.
    pub barrier: bool,
    /// Run only in the cold build (`at end`), never per row or at the barrier.
    pub end: bool,
}

// ---- declarations ----------------------------------------------------------

/// The steps listed in a section's text.
pub(crate) fn parse_steps(text: &str, of: Of) -> Vec<Step> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("- ").or_else(|| l.strip_prefix("* "))
        })
        .filter_map(|item| step_of(item, of))
        .collect()
}

/// What stands before the `: ` of a step.
pub(crate) struct Head {
    name: String,
    limit: Option<Duration>,
    on: Vec<String>,
    barrier: bool,
    end: bool,
}

/// A step's text: `label [(limit)] [on <globs>] [at barrier|end]: command`, or
/// a bare command. The label may be several words (`web-chrome ctl and
/// workspace: cd …`); only the command after it runs. Text that reads as a label
/// wsp could not take (a limit that is not one, an `at` that names nothing) is
/// kept whole as the command, and [`unparsable`] refuses to start it — it is
/// never handed to `sh` with its label as the program.
pub(crate) fn step_of(item: &str, of: Of) -> Option<Step> {
    let item = item.trim();
    let (head, command) = match line_of(item) {
        Some((h, c)) => (Some(h), c),
        None => (None, item),
    };
    let command = command_of(command);
    if command.is_empty() {
        return None;
    }
    let h = head.unwrap_or_else(|| Head { name: named_by(command), limit: None, on: Vec::new(), barrier: false, end: false });
    let of = match of {
        Of::Project if !h.on.is_empty() => Of::Rule,
        o => o,
    };
    Some(Step { name: h.name, command: command.to_string(), limit: h.limit.unwrap_or(DEFAULT_LIMIT), of, on: h.on, barrier: h.barrier, end: h.end })
}

/// The command of a step's text: what stands inside its backticks when a note
/// follows them (`` `npm test` (33 tests) ``), else the text with its backticks
/// taken off.
fn command_of(text: &str) -> &str {
    let text = text.trim();
    if text.starts_with('`') {
        let closes = text.match_indices('`').map(|(i, _)| i).filter(|&i| i > 0);
        for close in closes {
            let rest = &text[close + 1..];
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return text[1..close].trim();
            }
        }
    }
    text.trim_matches('`').trim()
}

/// Why `sh` cannot take `command` as a script, or `None` when it parses. A
/// command that still starts with a label (`slow (soon): run it`) is one: its
/// label's first word would run as the program (`wsp-609`).
pub(crate) fn unparsable(command: &str) -> Option<String> {
    if let Some(label) = label_left_in(command) {
        return Some(format!(
            "`{label}:` reads as a label but is not one — write `label (10m): command` (a limit in s, m or h) or `label: command`"
        ));
    }
    let out = Command::new("sh").arg("-n").arg("-c").arg(command).stdin(Stdio::null()).output().ok()?;
    if out.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.lines().next().unwrap_or("it does not parse").trim();
    Some(said.trim_start_matches("sh: ").trim_start_matches("-c: ").to_string())
}

/// Each step of `steps` that cannot start, with why: `name — reason`.
pub(crate) fn malformed_in(steps: &[Step]) -> Vec<String> {
    steps.iter().filter_map(|s| unparsable(&s.command).map(|why| format!("{} — {why}", s.name))).collect()
}

/// Each step of `steps` whose command runs `wsp check`, `wsp review` or `wsp
/// submit` on row `id` — by its id, or with no row named at all, which is the
/// row the check runs in. Such a line calls the very check that is running it:
/// `wsp check <id>` from inside a check of `<id>` is refused as already
/// running and exits 1 every time, so the line can never pass (compounding-520,
/// wsp-638). `name — reason`, like [`malformed_in`].
pub(crate) fn calls_itself_in(id: &str, steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .filter(|s| calls_row(&s.command, id))
        .map(|s| {
            format!(
                "{} — `{}` runs {id}'s own check from inside it, so it is always refused as already running; \
                 drop the line (the check runs the other lines) or name what it was meant to run",
                s.name,
                s.command.trim()
            )
        })
        .collect()
}

/// Whether `command` has a simple command `wsp check|review|submit` that names
/// `id`, or names no row.
fn calls_row(command: &str, id: &str) -> bool {
    // Separators are words of their own, so a simple command ends where one is.
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, words: &mut Vec<String>| {
        if !cur.is_empty() {
            words.push(std::mem::take(cur).trim_matches(['"', '\'']).to_string());
        }
    };
    for c in command.chars() {
        if matches!(c, ';' | '&' | '|' | '(' | ')' | '\n' | '`') {
            flush(&mut cur, &mut words);
            words.push(";".into());
        } else if c.is_whitespace() {
            flush(&mut cur, &mut words);
        } else {
            cur.push(c);
        }
    }
    flush(&mut cur, &mut words);
    words.split(|w| w == ";").any(|simple| {
        let Some(at) = simple.iter().position(|w| w == "wsp" || w.ends_with("/wsp")) else { return false };
        let rest = &simple[at + 1..];
        let Some(verb) = rest.first() else { return false };
        if !matches!(verb.as_str(), "check" | "review" | "submit") {
            return false;
        }
        let args: Vec<&String> = rest[1..].iter().filter(|w| !w.starts_with('-')).collect();
        args.is_empty() || args.iter().any(|w| w.as_str() == id)
    })
}

/// A word a label may be made of: it starts with a letter or digit and holds
/// nothing a shell would read as more than a word.
fn label_word(w: &str) -> bool {
    w.chars().next().is_some_and(char::is_alphanumeric) && w.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The text before a command's first `: ` when it looks like a label — label
/// words, a parenthesised limit, `on` globs, `at` — rather than shell. `None`
/// for a command with no `: `, or whose head carries quotes, `$`, `|`, `;`, `&`,
/// `=` or a redirection, which make it shell.
fn label_left_in(command: &str) -> Option<&str> {
    let (h, _) = command.split_once(": ")?;
    let first = h.split_whitespace().next()?;
    let shellish = h.chars().any(|c| !(c.is_alphanumeric() || c.is_whitespace() || matches!(c, '_' | '-' | '.' | ',' | '(' | ')' | '/' | '*' | '{' | '}')));
    (label_word(first) && !shellish).then_some(h.trim())
}

/// Why a step's command names no program wsp would run on purpose: its first
/// word (past `VAR=value` assignments) is not a shell keyword or builtin, not a
/// path, and not on `PATH`. Asked where a line is written — `wsp check --set`,
/// `wsp worklist stop-checks` — not at the run, whose tree may provide it.
pub(crate) fn unrunnable(command: &str) -> Option<String> {
    let first = command
        .split_whitespace()
        .find(|w| !(w.contains('=') && w.split('=').next().is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_'))))?;
    let first = first.trim_start_matches(['(', '{', '!']);
    if first.is_empty() || first.contains('/') || first.starts_with(['$', '`', '"', '\'', '[']) {
        return None;
    }
    let known = Command::new("sh")
        .arg("-c")
        .arg("command -v \"$1\" >/dev/null 2>&1")
        .arg("sh")
        .arg(first)
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    (!known).then(|| format!("`{first}` is not a command here (not a builtin, a path, or on PATH)"))
}

/// What a line is refused for where it is written: it cannot start
/// ([`malformed_in`]), or its first word is no program ([`unrunnable`]).
pub(crate) fn refused_in(steps: &[Step]) -> Vec<String> {
    let mut out = malformed_in(steps);
    for s in steps {
        if unparsable(&s.command).is_none() {
            if let Some(why) = unrunnable(&s.command) {
                out.push(format!("{} — {why}", s.name));
            }
        }
    }
    out
}

/// The one check-line grammar, read by `## Checks` ([`step_of`]), `wsp check
/// --set`, `--stop` lines and an exit's `checks:` / `checked:`
/// ([`timed_line`]), so the four cannot disagree (`wsp-612`, the exit plan's
/// V3): the head and the command of `label (time)[ on <globs>][ at
/// barrier|end]: command`, or of wsp-609's untimed `label words: command`.
///
/// A timed label is everything before the last ` (` that is followed by a time
/// (`Ns`, `Nm`, `Nh`) and `)`, so it may be several words of anything
/// (`web-chrome ctl & workspace (5m): …`); only a label with no `: ` and no
/// open `"` or backtick counts, so a timed pattern inside the command is not
/// mistaken for the head. Without a time the label is wsp-609's label words. `None` when no
/// head can be read: the whole text is then the command, and [`unparsable`]
/// says why it cannot start.
pub(crate) fn line_of(item: &str) -> Option<(Head, &str)> {
    let item = item.trim();
    for (i, _) in item.rmatch_indices(" (") {
        let label = item[..i].trim();
        // A label that opens a quote is shell, and its ` (time): ` is inside the command.
        let open = |q: char| label.matches(q).count() % 2 == 1;
        if label.is_empty() || label.contains(": ") || open('"') || open('`') {
            continue;
        }
        let rest = &item[i + 2..];
        let Some(close) = rest.find(')') else { continue };
        let Some(limit) = limit_of(&rest[..close]) else { continue };
        let after = &rest[close + 1..];
        let (tail, command) = if let Some(c) = after.strip_prefix(": ") {
            ("", c)
        } else if after.starts_with(" on ") || after.starts_with(" at ") {
            match after.split_once(": ") {
                Some(split) => split,
                None => continue,
            }
        } else {
            continue;
        };
        let mut words = tail.split_whitespace().peekable();
        let Some(mut head) = rules_of(&mut words) else { continue };
        head.name = label.to_string();
        head.limit = Some(limit);
        return Some((head, command));
    }
    let (h, c) = item.split_once(": ")?;
    Some((head_of(h)?, c))
}

/// A check line as an exit writes it, strictly: `label (time): command`, and
/// nothing else — no bare command, no untimed label, no `on` or `at` (those are
/// the project's rules). The label, the time as written, and the command; or
/// the shape to write instead. A line refused here is never run.
pub(crate) fn timed_line(item: &str) -> Result<(String, String, String), String> {
    const SHAPE: &str = "write `label (5m): command` (a time in s, m or h)";
    let item = item.trim();
    let Some((head, command)) = line_of(item) else {
        return Err(format!("`{item}` has no `label (time): ` — {SHAPE}"));
    };
    let Some(limit) = head.limit else {
        return Err(format!("`{}:` carries no (time) — {SHAPE}", head.name));
    };
    if !head.on.is_empty() || head.barrier || head.end {
        return Err(format!("`{}` names `on` or `at`, which belong to the project's `## Checks` — {SHAPE}", head.name));
    }
    let command = command.trim();
    if command.is_empty() {
        return Err(format!("`{}` names no command — {SHAPE}", head.name));
    }
    let time = item[..item.len() - command.len()].trim_end().trim_end_matches(':').trim_end();
    let time = time.rsplit_once(" (").map(|(_, t)| t.trim_end_matches(')')).unwrap_or_default();
    debug_assert_eq!(limit_of(time), Some(limit));
    Ok((head.name, time.to_string(), command.to_string()))
}

/// `name [(limit)] [on <globs>] [at barrier|end]`, when the text before a `: `
/// is one. The name is one or more label words, up to a `(limit)`, `on` or `at`.
fn head_of(h: &str) -> Option<Head> {
    let mut words = h.split_whitespace().peekable();
    let mut name: Vec<&str> = Vec::new();
    while let Some(w) = words.peek().copied() {
        if w.starts_with('(') || matches!(w, "on" | "at") {
            break;
        }
        if !label_word(w) {
            return None;
        }
        name.push(w);
        words.next();
    }
    if name.is_empty() {
        return None;
    }
    let mut limit = None;
    if let Some(w) = words.peek().copied() {
        if let Some(inner) = w.strip_prefix('(').and_then(|w| w.strip_suffix(')')) {
            limit = Some(limit_of(inner)?);
            words.next();
        }
    }
    let mut head = rules_of(&mut words)?;
    head.name = name.join(" ");
    head.limit = limit;
    Some(head)
}

/// The `[on <globs>] [at barrier|end]` that may follow a head's limit, and
/// nothing after them.
fn rules_of<'a>(words: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>) -> Option<Head> {
    let mut head = Head { name: String::new(), limit: None, on: Vec::new(), barrier: false, end: false };
    if words.peek() == Some(&"on") {
        words.next();
        let mut globs = String::new();
        while let Some(w) = words.peek().copied() {
            if w == "at" {
                break;
            }
            globs.push_str(w);
            globs.push(' ');
            words.next();
        }
        head.on = globs.split(',').map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect();
        if head.on.is_empty() {
            return None;
        }
    }
    if words.peek() == Some(&"at") {
        words.next();
        match words.next() {
            Some("barrier") => head.barrier = true,
            Some("end") => head.end = true,
            _ => return None,
        }
    }
    words.next().is_none().then_some(head)
}

pub(crate) fn limit_of(s: &str) -> Option<Duration> {
    let s = s.trim();
    // Split at a character, not a byte: `(✓)` or `(café)` is no limit, never a panic.
    let unit = s.chars().last()?;
    let n: u64 = s[..s.len() - unit.len_utf8()].parse().ok()?;
    let secs = match unit {
        's' => n,
        'm' => n * 60,
        'h' => n * 3600,
        _ => return None,
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// What an unnamed step is called: the command's first word, as a file name.
fn named_by(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or("check");
    let base = first.rsplit('/').next().unwrap_or(first);
    util::truncate(base, 30)
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["check"],
        run: check,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 110,
                text: r##"  wsp check --inner [<id>] [--run]  the steps this tree's changes call for,
                                    narrowest first; --run runs them here and
                                    records nothing"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 120,
                text: r##"  wsp check --affected [<id>] [--run]
                                    those, with the tests the crate graph
                                    (cargo metadata) and Vitest's module graph
                                    reach, narrowest first; --run in a tree
                                    clean at HEAD reuses and records greens by
                                    input hash, and says how many were reused"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 130,
                text: r##"  wsp check <project> --tiers       its declared steps by tier (inner, row,
                                    barrier, end) and what it leaves out"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 140,
                text: r##"  wsp check <id> [--plan] [--at <commit>]
                                    run a task's mechanical checks — its
                                    project's ## Checks, then the task's own —
                                    at the commit it landed, in a warm tree of
                                    wsp's, and record each step under ##
                                    Verification. A failure on a row at review
                                    sends it back with the step as what is owed;
                                    wsp runs it by itself on a ready or landed
                                    row, before any verifier. --plan lists the steps with
                                    their tier and any green already held, --at
                                    runs another commit and writes nothing"##,
            },
        ],
    },
];

#[cfg(test)]
fn matches(glob: &str, path: &str) -> bool {
    captures(glob, path).is_some()
}

/// Whether a capture is safe to put in a shell command: a filename is
/// somebody's text, and `$(…)` in one must not run.
fn plain(c: &str) -> bool {
    !c.is_empty() && c.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The steps a rule makes of the changed paths: itself when it names no
/// capture, and one per distinct capture otherwise.
fn expand(rule: &Step, changed: &[String]) -> Vec<Step> {
    let named = (1..=9).any(|n| rule.command.contains(&format!("{{{n}}}")));
    let mut seen: Vec<Vec<String>> = Vec::new();
    for path in changed {
        if let Some(caps) = rule.on.iter().find_map(|g| captures(g, path)) {
            if caps.iter().all(|c| plain(c)) && !seen.contains(&caps) {
                seen.push(caps);
            }
        }
    }
    if !named {
        return if seen.is_empty() { Vec::new() } else { vec![rule.clone()] };
    }
    seen.sort();
    seen.into_iter()
        .map(|caps| {
            let mut command = rule.command.clone();
            for (i, c) in caps.iter().enumerate() {
                command = command.replace(&format!("{{{}}}", i + 1), c);
            }
            Step { name: format!("{}:{}", rule.name, caps.join(":")), command, ..rule.clone() }
        })
        .collect()
}

fn project_steps(store: &Store, t: &Task) -> Vec<Step> {
    project_of(t)
        .and_then(|p| store.project(&p))
        .and_then(|p| p.section(SECTION))
        .map(|s| parse_steps(&s, Of::Project))
        .unwrap_or_default()
}

/// The member's own steps.
pub(crate) fn task_steps(t: &Task) -> Vec<Step> {
    t.section(SECTION).map(|s| parse_steps(&s, Of::Task)).unwrap_or_default()
}

/// The steps a project holds back for its barrier, whatever the row.
pub(crate) fn barrier_steps(store: &Store, project: &str) -> Vec<Step> {
    store
        .project(project)
        .and_then(|p| p.section(SECTION))
        .map(|s| parse_steps(&s, Of::Project).into_iter().filter(|s| s.barrier).collect())
        .unwrap_or_default()
}

/// The steps a project runs only in the cold build, at the end of a worklist.
pub(crate) fn end_steps(store: &Store, project: &str) -> Vec<Step> {
    store
        .project(project)
        .and_then(|p| p.section(SECTION))
        .map(|s| parse_steps(&s, Of::Project).into_iter().filter(|s| s.end).collect())
        .unwrap_or_default()
}

// ---- artefacts and the full suite (wsp submit's validator) ------------------

/// The project's `## Artefacts`: one glob per line (a `- ` bullet is allowed),
/// `#` comments and blank lines skipped, and a ` — why` after the glob is its
/// note (docs/exit-packet.md §4's shape). An absent or empty section is none:
/// nothing is restored for the member.
pub(crate) fn artefact_globs(store: &Store, project: &str) -> Vec<String> {
    store.project(project).and_then(|p| p.section("Artefacts")).map(|s| globs_in(&s)).unwrap_or_default()
}

fn globs_in(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).unwrap_or(l))
        .map(|l| l.split_once(" — ").map_or(l, |(g, _)| g).trim().trim_matches('`').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The D4 key: the project's `## Full suite` section. Command lines (a bullet
/// and backticks are allowed) name the full-suite commands; the line
/// `members: allowed` lets a member's check or a run step below the barrier
/// use them. Without that line a member's check naming one is refused.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FullSuite {
    pub commands: Vec<String>,
    pub members_allowed: bool,
}

impl FullSuite {
    /// Whether `command` is one of the project's full-suite commands.
    pub(crate) fn names(&self, command: &str) -> bool {
        let c = command.trim();
        self.commands.iter().any(|f| f == c)
    }

    /// Whether a member may run `command`: it is no full-suite command, or the
    /// project allows members the full suite.
    pub(crate) fn member_may_run(&self, command: &str) -> bool {
        !self.names(command) || self.members_allowed
    }
}

pub(crate) fn full_suite(store: &Store, project: &str) -> FullSuite {
    store.project(project).and_then(|p| p.section("Full suite")).map(|s| full_suite_in(&s)).unwrap_or_default()
}

fn full_suite_in(text: &str) -> FullSuite {
    let mut out = FullSuite::default();
    for l in globs_in(text) {
        match l.split_once(':') {
            Some((k, v)) if k.trim() == "members" => out.members_allowed = v.trim() == "allowed",
            _ => out.commands.push(l),
        }
    }
    out
}

/// What a landing is run against: the steps, in order, and the paths that
/// brought the rules in.
pub(crate) struct Plan {
    pub steps: Vec<Step>,
    pub changed: Vec<String>,
}

/// The member's steps, then the project's rules the changed paths call for,
/// then its every-landing steps; a command named twice runs once, and a barrier
/// step is not a row's.
pub(crate) fn plan(store: &Store, t: &Task, tg: &Target, at: Option<&str>) -> Plan {
    let mut steps = task_steps(t);
    // The done pieces' own lines follow the row's, labelled `<n>.<label>`.
    for (_, s) in crate::pieces::check_steps(t) {
        if !steps.iter().any(|x| x.command == s.command) {
            steps.push(s);
        }
    }
    let rules = project_steps(store, t);
    // The changed paths are read for the help snapshot too (`wsp-564`), so a
    // repository that carries `fixtures/help.txt` always has them.
    let at_sha = |path: &str| git(&tg.repo, &["show", &format!("{}:{path}", tg.sha)]);
    let wants_paths = rules.iter().any(|s| s.of == Of::Rule && !s.barrier && !s.end) || [HELP_FIXTURE, LAYERING_FILE, READONLY_FILE].iter().any(|f| at_sha(f).is_some());
    let changed = if wants_paths { changed_paths(tg, t, at.is_none()) } else { Vec::new() };
    for r in rules.into_iter().filter(|r| !r.barrier && !r.end) {
        let made = match r.of {
            Of::Rule => expand(&r, &changed),
            _ => vec![r],
        };
        for m in made {
            if !steps.iter().any(|s| s.command == m.command) {
                steps.push(m);
            }
        }
    }
    for h in structure_steps(&at_sha, &changed) {
        if !steps.iter().any(|s| s.command == h.command) {
            steps.push(h);
        }
    }
    // The base the diff was read against; a landing found by its commit
    // messages has none, and then a module's being new cannot be told.
    if let Some(base) = tg.since.clone().or_else(|| at.is_none().then(|| since_of(t)).flatten()) {
        let in_base = |path: &str| git(&tg.repo, &["cat-file", "-e", &format!("{base}:{path}")]).is_some();
        if let Some(l) = layering_step(&|p| at_sha(p).is_some(), &in_base, &changed) {
            if !steps.iter().any(|s| s.command == l.command) {
                steps.push(l);
            }
        }
    }
    Plan { steps, changed }
}

/// The module a source path belongs to, as `tests/layering.rs` counts them:
/// `src/<m>.rs` or `src/<m>/…` of the package or of a crate. Returns the path
/// of the module (`src/m` or `crates/c/src/m`), without the `.rs`.
fn module_of(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let at = match parts.as_slice() {
        ["src", ..] => 0,
        ["crates", _, "src", ..] => 2,
        _ => return None,
    };
    let rest = &parts[at + 1..];
    let first = rest.first()?;
    let name = match rest.len() {
        1 => first.strip_suffix(".rs")?,
        _ => first,
    };
    // The crate's root files are not modules of the map.
    if rest.len() == 1 && matches!(name, "main" | "lib") {
        return None;
    }
    Some(format!("{}/{name}", parts[..=at].join("/")))
}

/// The layering test, when the diff adds a module (`wsp-572`): a file under
/// `src/` or a crate's `src/` whose module the base does not have. A module
/// added without its row in `docs/components.md` reached the trunk red five
/// times and was found by the group's barrier each time, minutes later; this
/// finds it in the member's own check for about a second, and only on a row
/// that adds a module. The test's own message names the modules with no
/// component.
///
/// `has` says whether the checked tree keeps a path, `in_base` whether the
/// base the diff is read against does (a file or a directory).
pub(crate) fn layering_step(has: &dyn Fn(&str) -> bool, in_base: &dyn Fn(&str) -> bool, changed: &[String]) -> Option<Step> {
    if !has(LAYERING_FILE) {
        return None;
    }
    let mut on: Vec<String> = Vec::new();
    for p in changed.iter().filter(|p| p.ends_with(".rs") && has(p)) {
        let Some(m) = module_of(p) else { continue };
        if !in_base(&format!("{m}.rs")) && !in_base(&m) {
            on.push(p.clone());
        }
    }
    if on.is_empty() {
        return None;
    }
    Some(Step {
        name: "layering".into(),
        command: LAYERING_COMMAND.into(),
        limit: Duration::from_secs(20 * 60),
        of: Of::Rule,
        on,
        barrier: false,
        end: false,
    })
}

/// The help snapshot a repository keeps: everything `wsp help` prints after
/// its version line.
pub(crate) const HELP_FIXTURE: &str = "fixtures/help.txt";

/// The test that compares the help to [`HELP_FIXTURE`].
pub(crate) const HELP_TEST: &str = "the_help_is_what_it_was_before_the_registry";

/// Whether a changed file feeds `wsp help`: the verb registry and its entries,
/// or a module that declares a `VERBS` table or the closing notes.
fn feeds_help(path: &str, content: &str) -> bool {
    path.ends_with(".rs")
        && (matches!(path, "src/main.rs" | "src/registry.rs" | "src/verbs.rs") || content.contains("const VERBS") || content.contains("HELP_NOTES"))
}

/// The help-snapshot test, when a path the landing changed feeds the help and
/// the repository keeps a snapshot (`wsp-564`). A help line changed without
/// `fixtures/help.txt` reached the trunk red and was found by the group's
/// barrier twenty minutes later, three times in a day; this finds it in the
/// member's own check, for the cost of one test and only on a row that touches
/// a help source. The failure message of the test names the verb that
/// regenerates the snapshot (`wsp help --fixture`).
///
/// `read` gives a path's content at the commit under check.
pub(crate) fn help_step(read: &dyn Fn(&str) -> Option<String>, changed: &[String]) -> Option<Step> {
    read(HELP_FIXTURE)?;
    let on: Vec<String> = changed.iter().filter(|p| feeds_help(p, &read(p).unwrap_or_default())).cloned().collect();
    if on.is_empty() {
        return None;
    }
    Some(Step {
        name: "help".into(),
        command: format!("cargo test {HELP_TEST}"),
        limit: Duration::from_secs(20 * 60),
        of: Of::Rule,
        on,
        barrier: false,
        end: false,
    })
}

/// The layering test a repository keeps, and the readonly classification.
pub(crate) const LAYERING_FILE: &str = "tests/layering.rs";
pub(crate) const LAYERING_COMMAND: &str = "cargo test --test layering";
pub(crate) const READONLY_FILE: &str = "src/readonly.rs";
pub(crate) const READONLY_COMMAND: &str = "cargo test readonly::tests";

/// Whether a changed file can add a module or a verb: the module list, the
/// registry, or a file that declares a `VERBS` table.
fn adds_module_or_verb(path: &str, content: &str) -> bool {
    path.ends_with(".rs") && (matches!(path, "src/main.rs" | "src/registry.rs" | "src/verbs.rs") || content.contains("const VERBS"))
}

/// The layering and readonly tests, when a path the landing changed can add a
/// module or a verb (`wsp-569`). Both ran only at the group's barrier, which
/// stops at the first failure and holds the whole group; a row that adds a
/// module (an upward `use`) or a verb (unclassified in `src/readonly.rs`) finds
/// out in its own check, for the cost of two small tests, and only when it
/// touches those sources. Each needs its file in the repository.
///
/// `read` gives a path's content at the commit under check.
pub(crate) fn structure_steps(read: &dyn Fn(&str) -> Option<String>, changed: &[String]) -> Vec<Step> {
    let mut out: Vec<Step> = help_step(read, changed).into_iter().collect();
    let on: Vec<String> = changed.iter().filter(|p| adds_module_or_verb(p, &read(p).unwrap_or_default())).cloned().collect();
    if on.is_empty() {
        return out;
    }
    for (file, name, command) in [(LAYERING_FILE, "layering", LAYERING_COMMAND), (READONLY_FILE, "readonly", READONLY_COMMAND)] {
        if read(file).is_some() {
            out.push(Step {
                name: name.into(),
                command: command.into(),
                limit: Duration::from_secs(20 * 60),
                of: Of::Rule,
                on: on.clone(),
                barrier: false,
                end: false,
            });
        }
    }
    out
}

/// The steps of a re-plan after the trunk moved that the rebase can have
/// affected, given `pulled_in` (what the rebase brought from the trunk) and
/// `ours` (what the branch changes): a project rule whose glob matches a pulled-in
/// path; a member's own step when the two sets overlap; an every-landing step
/// when anything was pulled in. A rule expanded from a capture is kept by its
/// glob, so a sibling capture may be rerun too — more checks, never fewer.
pub(crate) fn affected(steps: &[Step], pulled_in: &[String], ours: &[String]) -> Vec<Step> {
    let overlap = pulled_in.iter().any(|d| ours.contains(d));
    steps
        .iter()
        .filter(|s| match s.of {
            Of::Rule => s.on.iter().any(|g| pulled_in.iter().any(|d| captures(g, d).is_some())),
            Of::Task => overlap,
            Of::Project => !pulled_in.is_empty(),
        })
        .cloned()
        .collect()
}

/// The paths the landing changed: since the commit `wsp land` wrote on the row,
/// and for a row landed before it did, in the commits up to `sha` that name the
/// row's id — in front, as wsp's own do now, or at the end, as they once did.
pub(crate) fn changed_paths(tg: &Target, t: &Task, use_since: bool) -> Vec<String> {
    let since = tg.since.clone().or_else(|| use_since.then(|| since_of(t)).flatten());
    let out = match since {
        Some(base) => git(&tg.repo, &["diff", "--name-only", &base, &tg.sha]),
        None => git(
            &tg.repo,
            &["log", &tg.sha, "-E", &format!("--grep=(^|[^[:alnum:]-]){}([^0-9]|$)", t.id), "--name-only", "--format="],
        ),
    };
    let mut paths: Vec<String> = out.unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The `since <commit>` clause on the row's newest landing line.
fn since_of(t: &Task) -> Option<String> {
    let log = t.section("Log")?;
    let line = log.lines().filter(|l| l.contains(crate::repair::LANDED)).last()?;
    let after = line.split(" since ").nth(1)?;
    let sha = after.split_whitespace().next()?;
    (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha.to_string())
}

// ---- where, and at what ----------------------------------------------------

/// The commit to run at, and the repository that holds it.
pub(crate) struct Target {
    /// A base named on the command line for reading which paths changed.
    pub since: Option<String>,
    pub sha: String,
    pub repo: PathBuf,
    pub project: String,
}

/// The landing of `t` — or `at`, when a commit is named — in the first of its
/// project's roots that has the commit.
pub(crate) fn target(store: &Store, t: &Task, at: Option<&str>) -> Result<Target, String> {
    let sha = match at {
        Some(a) => a.to_string(),
        None => crate::repair::commit_of(t).ok_or_else(|| format!("{} has no landing recorded — `wsp check {} --at <commit>` names one", t.id, t.id))?,
    };
    let project = project_of(t).ok_or_else(|| format!("{} belongs to no project", t.id))?;
    let mut roots = Vec::new();
    let mut at_project = Some(project.clone());
    while let Some(p) = at_project.take() {
        let Some(found) = store.project(&p) else { break };
        if !found.roots.is_empty() {
            roots = found.roots.clone();
            break;
        }
        at_project = found.parent.clone();
    }
    if roots.is_empty() {
        return Err(format!("project {project} has no roots to find {sha} in"));
    }
    for root in &roots {
        let Some(repo) = crate::cmd_verify::toplevel(&util::expand(root)) else { continue };
        if let Some(full) = git(&repo, &["rev-parse", "--verify", "--quiet", &format!("{sha}^{{commit}}")]) {
            return Ok(Target { sha: full.trim().to_string(), repo, project, since: None });
        }
    }
    Err(format!("{sha} is in none of {project}'s roots ({})", roots.join(", ")))
}

/// The member's own tree, when the commit to check is the ready commit it
/// stands at: the checks run there, in its warm `target/`, and not in a tree of
/// wsp's own. A named commit (`--at`) is a look elsewhere and is never run here.
fn member_tree_at(store: &Store, t: &Task, sha: &str, named: bool) -> Option<PathBuf> {
    if named || !crate::repair::ready(t).is_some_and(|r| v::same_commit(&r, sha)) {
        return None;
    }
    let tree = crate::cmd_checkout::member_tree(store, &t.id)?;
    let head = git(&tree, &["rev-parse", "HEAD"])?;
    v::same_commit(head.trim(), sha).then_some(tree)
}

pub(crate) fn dir_of(store: &Store, tg: &Target) -> PathBuf {
    let repo = tg.repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    store.state_file("checks").join(format!("{}-{repo}", tg.project))
}

/// The barrier's own warm tree and lock for a repository, apart from the
/// per-row `checks/` trees so a row's check never moves the barrier's `target/`.
pub(crate) fn barrier_dir(store: &Store, project: &str, repo: &Path) -> PathBuf {
    let repo = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    store.state_file("barrier").join(format!("{project}-{repo}"))
}

/// Where the barrier tree stands: its path and the commit it is at, if it exists.
pub(crate) fn barrier_tree_line(store: &Store, project: &str, repo: &Path) -> String {
    let tree = barrier_dir(store, project, repo).join("tree");
    match git(&tree, &["rev-parse", "HEAD"]) {
        Some(head) if tree.join(".git").exists() => format!("barrier tree {} at {}", util::contract(&tree), &head.trim()[..head.trim().len().min(12)]),
        _ => format!("barrier tree {} not made yet", util::contract(&tree)),
    }
}

/// Move the warm tree to `sha`, making it on the first use.
pub(crate) fn checkout(repo: &Path, tree: &Path, sha: &str) -> Result<(), String> {
    if tree.join(".git").exists() {
        git_ok(tree, &["checkout", "--detach", "--quiet", "--force", sha])?;
        // What a build left that git would track is cleaned; what it ignores
        // (`target/`, which is the warmth) is not.
        return git_ok(tree, &["clean", "-fdq"]);
    }
    if let Some(parent) = tree.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", util::contract(parent)))?;
    }
    // A tree whose directory was removed leaves a registration that refuses
    // the path.
    let _ = git_ok(repo, &["worktree", "prune"]);
    git_ok(repo, &["worktree", "add", "--detach", "--quiet", &tree.display().to_string(), sha])
}

/// How a check tree stood when its steps began (`wsp-566`): was there a built
/// `target/` to start from, and how busy was the machine. A first check that
/// builds for twelve minutes against twenty seconds for the later ones is a cold
/// target or a loaded machine, and the record said neither; this is what says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Start {
    /// `cold` (no built `target/`), `warm` (one was there), `cloned` (made warm
    /// from the trunk's just now), or `none` (a tree with no cargo target to be
    /// warm or cold).
    pub target: &'static str,
    /// What seeding said, one line each, when it was tried.
    pub seed: Vec<String>,
    /// The one-minute load average at the start, and the cores to read it against.
    pub load: Option<f64>,
    pub cores: usize,
}

impl Start {
    /// The tree as it stands now; `cloned` says a seed was just made into it.
    pub(crate) fn of(tree: &Path, seed: Vec<String>) -> Start {
        let cloned = seed.iter().any(|l| l.contains("cloned from the parent tree"));
        let built = std::fs::read_dir(tree.join("target")).is_ok_and(|mut d| d.next().is_some());
        let target = match (built, cloned) {
            (true, true) => "cloned",
            (true, false) => "warm",
            (false, _) if tree.join("Cargo.lock").is_file() => "cold",
            (false, _) => "none",
        };
        Start { target, seed, load: crate::throttle::load1(), cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) }
    }

    /// `target cold, load 3.2 of 8 cores`, for a line of the log.
    pub(crate) fn line(&self) -> String {
        let load = self.load.map(|l| format!(", load {l:.1} of {} cores", self.cores)).unwrap_or_default();
        format!("target {}{load}", self.target)
    }

    /// The same as the figures an event carries.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({ "target": self.target, "load": self.load, "cores": self.cores, "seed": self.seed })
    }
}

/// [`checkout`], then — for a tree that is new or has lost its `target/` —
/// seed it from the trunk's by clone, as a row's tree is (`wsp-566`), and say
/// how it stands. Only the row-check and barrier trees come through here: the
/// cold build is cold by design and calls [`checkout`] alone.
pub(crate) fn checkout_started(repo: &Path, tree: &Path, sha: &str) -> Result<Start, String> {
    let made = !tree.join(".git").exists();
    checkout(repo, tree, sha)?;
    let seed = if made || !tree.join("target").exists() {
        crate::cmd_checkout::trunk(repo).filter(|t| t != tree).map(|t| crate::warm::clone_from(&t, tree)).unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(Start::of(tree, seed))
}

/// One check at a time in a tree: a file beside it holding the pid that has it.
pub(crate) struct TreeLock(PathBuf);

impl TreeLock {
    pub(crate) fn take(path: PathBuf, wait: Duration) -> Result<TreeLock, String> {
        Self::take_saying(path, wait, |_| {})
    }

    /// [`take`](Self::take), saying once — to `said`, with who has it and since
    /// when — if the lock is held, so a run that waits behind another names it.
    pub(crate) fn take_saying(path: PathBuf, wait: Duration, mut said: impl FnMut(Option<(u32, String)>)) -> Result<TreeLock, String> {
        let start = Instant::now();
        let mut told = false;
        loop {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "{} {}", std::process::id(), util::now_iso());
                    return Ok(TreeLock(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::dead(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if !told {
                        told = true;
                        said(Self::holder(&path));
                    }
                    if start.elapsed() > wait {
                        return Err(format!("the tree has been in use for {}s", wait.as_secs()));
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
                Err(e) => return Err(format!("{}: {e}", util::contract(&path))),
            }
        }
    }

    /// Who holds the lock and since when, as the file says; `None` where it is
    /// not there or does not read.
    pub(crate) fn holder(path: &Path) -> Option<(u32, String)> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut it = text.split_whitespace();
        let pid = it.next()?.parse().ok()?;
        Some((pid, it.next().unwrap_or_default().to_string()))
    }

    /// Whether the lock at `path` is held by a process that is alive: `kill -0`
    /// is a better witness of a live process than a `ps` that can fail empty.
    pub(crate) fn live(path: &Path) -> bool {
        Self::holder(path).is_some() && !Self::dead(path)
    }

    /// Whether the pid in the lock is gone, which is the only reason to break one.
    fn dead(path: &Path) -> bool {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let Some(pid) = text.split_whitespace().next().filter(|p| p.chars().all(|c| c.is_ascii_digit())) else {
            // Being written this instant, or garbage: garbage ages out.
            return std::fs::metadata(path).and_then(|m| m.modified()).map(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(60)).unwrap_or(false);
        };
        !Command::new("kill").args(["-0", pid]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(true)
    }
}

impl Drop for TreeLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---- running ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Done {
    Passed,
    Failed { exit: Option<i32>, timed_out: bool },
    /// The command could not start — it does not parse, or names nothing to run.
    /// The declaration is wrong; nothing the work did.
    Malformed(String),
    /// The step wrote nothing and used no CPU for [`hung_after`], and was ended
    /// there rather than left to its limit (`wsp-609`): the process tree it had,
    /// one `pid command` per line.
    Hung(String),
    /// An earlier step failed, so this one was not run.
    Skipped,
}

impl Done {
    /// Anything that stops a run: a red step or one that could not start.
    pub(crate) fn is_red(&self) -> bool {
        matches!(self, Done::Failed { .. } | Done::Malformed(_) | Done::Hung(_))
    }
}

/// How long a step may sit with no output and no CPU before it is called hung:
/// five minutes, or `WSP_CHECK_HUNG_SECS`.
pub(crate) fn hung_after() -> Duration {
    #[cfg(test)]
    if let Some(d) = HUNG_AFTER.with(|h| h.get()) {
        return d;
    }
    let secs = std::env::var("WSP_CHECK_HUNG_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    Duration::from_secs(secs)
}

#[cfg(test)]
thread_local! {
    /// A test's own hung threshold, kept to its thread.
    static HUNG_AFTER: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

/// The processes of group `pgid`: (`pid command` lines, their CPU time summed in
/// hundredths of a second). `None` where `ps` does not answer.
fn group_of(pgid: u32) -> Option<(Vec<String>, u64)> {
    let out = Command::new("ps").args(["-A", "-o", "pid=,pgid=,time=,command="]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut lines, mut cpu) = (Vec::new(), 0u64);
    for l in text.lines() {
        let mut w = l.split_whitespace();
        let (Some(pid), Some(g), Some(time)) = (w.next(), w.next(), w.next()) else { continue };
        if g.parse::<u32>().ok() != Some(pgid) {
            continue;
        }
        cpu += cpu_of(time);
        lines.push(format!("{pid} {}", util::truncate(&w.collect::<Vec<_>>().join(" "), 160)));
    }
    Some((lines, cpu))
}

/// `ps`'s `time` (`[[dd-]hh:]mm:ss[.cc]`) in hundredths of a second.
fn cpu_of(time: &str) -> u64 {
    let (days, rest) = match time.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap_or(0), r),
        None => (0, time),
    };
    let (whole, frac) = rest.split_once('.').unwrap_or((rest, "0"));
    let secs = whole.split(':').fold(0u64, |acc, p| acc * 60 + p.parse::<u64>().unwrap_or(0));
    let cc: u64 = format!("{frac:0<2}")[..2].parse().unwrap_or(0);
    (days * 86_400 + secs) * 100 + cc
}

#[derive(Clone, Debug)]
pub(crate) struct Ran {
    pub step: Step,
    pub done: Done,
    pub took: Duration,
    pub tail: String,
    /// Why the first run failed and its tail, when the step passed on a second:
    /// a flake is passed and kept in view, not swallowed.
    pub first: Option<(String, String)>,
    /// The green this step was answered by, when it did not run.
    pub reused: Option<Green>,
}

/// Run one step in `dir`, its output to `log`, killed with its process group
/// at the limit.
pub(crate) fn run_step(dir: &Path, step: &Step, log: &Path) -> Ran {
    let start = Instant::now();
    let failed = |why: String| Ran {
        step: step.clone(),
        done: Done::Failed { exit: None, timed_out: false },
        took: start.elapsed(),
        tail: why,
        first: None,
        reused: None,
    };
    if let Some(why) = unparsable(&step.command) {
        return Ran { done: Done::Malformed(why.clone()), tail: why, ..failed(String::new()) };
    }
    let Ok(out) = std::fs::File::create(log) else { return failed(format!("could not write {}", util::contract(log))) };
    let Ok(err) = out.try_clone() else { return failed("could not share the log".into()) };
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&step.command).current_dir(dir).stdin(Stdio::null()).stdout(out).stderr(err).process_group(0);
    clean_env(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return failed(format!("sh: {e}")),
    };
    let mut timed_out = false;
    // `wsp-609`: a step that writes nothing and uses no CPU for `hung_after` is
    // ended there and named with its process tree, not left to its limit.
    let quiet_for = hung_after();
    let probe = (quiet_for / 4).clamp(Duration::from_millis(200), Duration::from_secs(30));
    let (mut seen, mut quiet_since, mut probed) = ((0u64, 0u64), Instant::now(), Instant::now());
    let mut hung: Option<String> = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() > step.limit => {
                timed_out = true;
                let _ = Command::new("kill").args(["-s", "KILL", "--", &format!("-{}", child.id())]).status();
                break child.wait().ok();
            }
            Ok(None) if probed.elapsed() >= probe => {
                probed = Instant::now();
                let wrote = std::fs::metadata(log).map(|m| m.len()).unwrap_or(0);
                if let Some((pids, cpu)) = group_of(child.id()) {
                    if (wrote, cpu) != seen {
                        seen = (wrote, cpu);
                        quiet_since = Instant::now();
                    } else if quiet_since.elapsed() >= quiet_for {
                        hung = Some(pids.join("\n"));
                        let _ = Command::new("kill").args(["-s", "KILL", "--", &format!("-{}", child.id())]).status();
                        break child.wait().ok();
                    }
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break None,
        }
    };
    // `wsp-453`: whatever the step started and left in its group (a Vite
    // server, a Vitest watcher) goes with it, however it ended.
    let _ = Command::new("kill").args(["-s", "KILL", "--", &format!("-{}", child.id())]).stderr(Stdio::null()).status();
    if let Some(pids) = hung {
        let tail = format!("hung — no output and no CPU for {}s; its processes:\n{pids}", secs(quiet_for));
        return Ran { step: step.clone(), done: Done::Hung(pids), took: start.elapsed(), tail, first: None, reused: None };
    }
    let ok = !timed_out && status.is_some_and(|s| s.success());
    let tail = tail_of(log);
    let done = if ok {
        Done::Passed
    } else if let Some(why) = failed_to_start(status.and_then(|s| s.code()), &tail) {
        Done::Malformed(why)
    } else {
        Done::Failed { exit: status.and_then(|s| s.code()), timed_out }
    };
    Ran { step: step.clone(), done, took: start.elapsed(), tail, first: None, reused: None }
}

/// Whether an exit says the command never ran: not found (127) or not
/// executable (126), as `sh` words it on the last line of the output.
fn failed_to_start(exit: Option<i32>, tail: &str) -> Option<String> {
    let last = tail.lines().last().unwrap_or_default().trim();
    match exit {
        Some(127) if last.contains("not found") => Some(last.to_string()),
        Some(126) if last.contains("Permission denied") || last.contains("cannot execute") => Some(last.to_string()),
        _ => None,
    }
}

/// The last lines of a log, each cut to a width a row can hold.
fn tail_of(log: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(log) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(16 * 1024)));
    let mut bytes = Vec::new();
    let _ = f.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..]
        .iter()
        .map(|l| util::truncate(l, TAIL_COLS))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run `steps` in order, stopping at the first that fails.
pub(crate) fn run_all(dir: &Path, steps: &[Step], log: &Path, reuse: Option<&Reuse>, mut each: impl FnMut(&Ran)) -> Vec<Ran> {
    let mut out: Vec<Ran> = Vec::new();
    for s in steps {
        let stop = out.last().is_some_and(|r| r.done.is_red() || r.done == Done::Skipped);
        let ran = if stop {
            Ran { step: s.clone(), done: Done::Skipped, took: Duration::ZERO, tail: String::new(), first: None, reused: None }
        } else {
            run_again(dir, s, log, reuse)
        };
        each(&ran);
        out.push(ran);
    }
    out
}

/// Run every step whatever the one before did. A barrier wants all of its
/// answers — a red suite does not excuse a member's own check from being read —
/// where a row's run stops at the first.
pub(crate) fn run_every(dir: &Path, steps: &[Step], log: &Path, reuse: Option<&Reuse>, mut each: impl FnMut(&Ran)) -> Vec<Ran> {
    steps
        .iter()
        .map(|s| {
            let ran = run_again(dir, s, log, reuse);
            each(&ran);
            ran
        })
        .collect()
}

/// A step, and once more if it failed on its own account: a suite that fails
/// intermittently (the handbook counts five red-then-greens on unmodified trees)
/// would otherwise send a row back for nothing it did. The second pass is what
/// counts, and the first is kept on the entry — `wsp verify` makes the same
/// rule for the same reason: a flake swallowed stops being observable.
/// A step that timed out is not run twice.
///
/// With a `reuse`, a step already green at that commit is not run (it costs 0s
/// and is not written again), and a pass is recorded when the tree was clean at
/// that commit before the step ran.
pub(crate) fn run_again(dir: &Path, step: &Step, log: &Path, reuse: Option<&Reuse>) -> Ran {
    let inputs = reuse.and_then(|r| crate::affected::key(dir, r.sha, &step.command));
    if let Some(r) = reuse {
        if let Some(green) = r.greens.lookup_by(r.sha, inputs.as_deref(), &step.command, r.env) {
            return Ran { step: step.clone(), done: Done::Passed, took: Duration::ZERO, tail: String::new(), first: None, reused: Some(green) };
        }
    }
    let clean = reuse.is_some_and(|r| greens::clean_at(dir, r.sha));
    let first = run_step(dir, step, log);
    let ran = if matches!(first.done, Done::Failed { timed_out: false, .. }) {
        let second = run_step(dir, step, log);
        Ran { took: first.took + second.took, first: Some((why_failed(&first), first.tail.clone())), ..second }
    } else {
        first
    };
    if let (Some(r), true, Done::Passed) = (reuse, clean, &ran.done) {
        r.greens.record_by(r.sha, inputs.as_deref(), &step.command, r.env, secs(ran.took), r.who, ran.first.is_some());
    }
    ran
}

/// How many steps of a run were reused from the ledger and how many ran.
pub(crate) fn tally(ran: &[Ran]) -> (usize, usize) {
    let reused = ran.iter().filter(|r| r.reused.is_some()).count();
    let skipped = ran.iter().filter(|r| r.done == Done::Skipped).count();
    (reused, ran.len() - reused - skipped)
}

/// `N reused, M ran`, said on every run.
pub(crate) fn tally_line(ran: &[Ran]) -> String {
    let (reused, ran) = tally(ran);
    format!("{reused} reused, {ran} ran")
}

pub(crate) fn passed(ran: &[Ran]) -> bool {
    ran.iter().all(|r| r.done == Done::Passed)
}

pub(crate) fn secs(d: Duration) -> u64 {
    d.as_secs().max(u64::from(d > Duration::ZERO))
}

pub(crate) fn why_failed(r: &Ran) -> String {
    match &r.done {
        Done::Failed { timed_out: true, .. } => format!("timed out after {}s", r.step.limit.as_secs()),
        Done::Failed { exit: Some(n), .. } => format!("exit {n}"),
        Done::Failed { exit: None, .. } => "no exit status".into(),
        Done::Malformed(why) => format!("malformed: {why}"),
        Done::Hung(pids) => format!("hung (no output, no CPU) — pids {}", pids.lines().filter_map(|l| l.split_whitespace().next()).collect::<Vec<_>>().join(" ")),
        _ => String::new(),
    }
}

/// The step lines an entry carries.
pub(crate) fn account(ran: &[Ran]) -> String {
    let mut out = Vec::new();
    for r in ran {
        let (mark, rest) = match (&r.done, &r.first) {
            (Done::Passed, _) if r.reused.is_some() => {
                let g = r.reused.as_ref().unwrap();
                let flake = if g.flaky { ", a flake that passed on a second run" } else { "" };
                let at = if !g.carried { "this commit".to_string() } else { format!("{} with the same inputs", &g.sha[..12]) };
                ("✓", format!("reused (green at {at}, {}s, by {}, {}{flake})", g.took, g.who, g.when))
            }
            (Done::Passed, None) => ("✓", format!("{}s", secs(r.took))),
            (Done::Passed, Some((why, _))) => ("✓", format!("{}s · passed on a second run, the first was {why}", secs(r.took))),
            (Done::Malformed(why), _) => ("✗", format!("malformed — the line cannot start: {why}")),
            (Done::Hung(_), _) => ("✗", format!("{}s · {}", secs(r.took), why_failed(r))),
            (Done::Failed { .. }, first) => {
                let twice = if first.is_some() { " both times" } else { "" };
                ("✗", format!("{}s · {}{twice}", secs(r.took), why_failed(r)))
            }
            (Done::Skipped, _) => ("–", "skipped".to_string()),
        };
        out.push(format!("{mark} {} · {} · {rest} · {}", r.step.name, r.step.command.lines().next().unwrap_or_default(), why_in(&r.step)));
        if r.done.is_red() {
            out.extend(r.tail.lines().map(|l| format!("  | {l}")));
        } else if let Some((_, tail)) = &r.first {
            out.extend(tail.lines().map(|l| format!("  ~ {l}")));
        }
    }
    out.join("\n")
}

/// What a failed run owes, for the row's log and the pane: the first failing
/// step, its command and the end of its output.
pub(crate) fn owed(ran: &[Ran], sha: &str) -> String {
    let Some(r) = ran.iter().find(|r| r.done.is_red()) else { return String::new() };
    if let Done::Malformed(why) = &r.done {
        return format!(
            "wsp check could not start `{}` at {}: {why}. The line is malformed, not the work — nothing was sent back. \
             Fix it with `wsp check <id> --set -` (one `- name: command` item per check, the command alone in backticks if a note follows).",
            r.step.name,
            &sha[..sha.len().min(12)],
        );
    }
    let tail: Vec<&str> = r.tail.lines().collect();
    let tail = tail[tail.len().saturating_sub(12)..].join("\n");
    format!(
        "wsp check failed at {}: `{}` ({}) — {}, {}s. Run it yourself and fix what it reports; the whole run is under ## Verification.\n\n{tail}",
        &sha[..sha.len().min(12)],
        r.step.name,
        r.step.command.lines().next().unwrap_or_default(),
        why_failed(r),
        secs(r.took),
    )
}

// ---- the record --------------------------------------------------------------

/// How long a `running` entry is believed: its steps' limits, the wait for the
/// tree, and a margin.
pub(crate) fn stale_after(steps: &[Step]) -> i64 {
    (steps.iter().map(|s| s.limit).sum::<Duration>() + TREE_WAIT + Duration::from_secs(300)).as_secs() as i64
}

/// Write a `running` entry for the landing, unless one is owed to nobody.
///
/// `Some(at)` is an entry written and waiting for its run. `None` is a run
/// already under way or finished for this commit — unless `again`, which a
/// person asks for and which ignores a finished one.
pub(crate) fn begin(store: &Store, id: &str, sha: &str, stale: i64, again: bool) -> Option<String> {
    let read: String = sha.chars().take(12).collect();
    let made = store.locked(|| {
        let mut t = store.find_task(id)?;
        let mut runs = v::runs(&t);
        let mut mine = runs.iter().enumerate().rev().filter(|(_, r)| r.read.as_deref().is_some_and(|x| v::same_commit(x, sha)));
        if let Some((i, r)) = mine.next() {
            let fresh = util::epoch_secs() - util::epoch_of(&r.at) <= stale;
            match r.state {
                RunState::Running if fresh => return None,
                RunState::Running => {
                    runs.remove(i);
                }
                RunState::Waiting => {}
                _ if !again => return None,
                _ => {}
            }
        }
        // An entry is found again by its stamp, and a run answered wholly by the ledger ends within the second it began.
        let mut secs = util::epoch_secs();
        while runs.iter().any(|r| util::epoch_of(&r.at) >= secs) {
            secs += 1;
        }
        let at = util::iso_at(secs);
        runs.push(Run { at: at.clone(), state: RunState::Running, read: Some(read), took: None, text: String::new() });
        v::save_runs(store, &mut t, &runs).ok()?;
        Some(at)
    })?;
    store.git_commit(&format!("wsp: checking {id}"));
    Some(made)
}

/// Take an entry back when its run could not start: nothing was run, so
/// nothing is recorded, and the next trigger starts it again.
fn abandon(store: &Store, id: &str, at: &str) {
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return };
        let runs: Vec<Run> = v::runs(&t).into_iter().filter(|r| r.at != at).collect();
        let _ = v::save_runs(store, &mut t, &runs);
    });
}

/// What [`settle`] did to the row.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Settled {
    Passed,
    /// Failed, and the row was at review: it is back at `doing`.
    SentBack,
    /// Failed, and the row was not at review: recorded, nobody moved.
    Failed,
    /// A step could not start: the line is wrong, not the work, so the row is
    /// recorded and left where it is, and its owner is asked to fix the line.
    Malformed,
    /// Timed out while the machine was over its load line: not a verdict, the
    /// row stays at review, and the gate runs the check again once the load falls.
    Waiting,
    /// The row or its entry is gone.
    Lost,
}

/// Write the outcome over its `running` entry — and, when it failed on a row at
/// review, send the row back in the same write, so no reader ever sees a failed
/// landing as finished.
pub(crate) fn settle(store: &Store, id: &str, at: &str, ran: &[Ran], took: Duration) -> Settled {
    settle_started(store, id, at, ran, took, None)
}

/// [`settle`], with how the tree stood when the steps began, so the event says
/// whether a long step began on a cold `target/` or a busy machine.
pub(crate) fn settle_started(store: &Store, id: &str, at: &str, ran: &[Ran], took: Duration, start: Option<&Start>) -> Settled {
    settle_loaded(store, id, at, ran, took, start, crate::throttle::load1())
}

/// [`settle_started`], with the load at the end given.
fn settle_loaded(store: &Store, id: &str, at: &str, ran: &[Ran], took: Duration, start: Option<&Start>, load_end: Option<f64>) -> Settled {
    let busy = start.and_then(|s| timed_out_under_load(ran, s, load_end));
    let state = if passed(ran) {
        RunState::Passed
    } else if busy.is_some() {
        RunState::Waiting
    } else {
        RunState::Failed
    };
    let settled = store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return Settled::Lost };
        let mut runs = v::runs(&t);
        let Some(i) = runs.iter().position(|r| r.at == at) else { return Settled::Lost };
        let read = runs[i].read.clone();
        // An earlier attempt at this commit that timed out under load: if this one
        // timed out at normal load, the reason carries both loads.
        let earlier: Vec<String> = runs[..i]
            .iter()
            .filter(|r| r.state == RunState::Waiting && r.read == read)
            .filter_map(|r| r.text.lines().find_map(|l| l.strip_prefix(WAITING)))
            .map(|l| l.trim_end_matches(", rerunning once the load falls").to_string())
            .collect();
        let run = &mut runs[i];
        run.state = state;
        run.took = Some(secs(took));
        run.text = account(ran);
        if let Some(why) = &busy {
            run.text.push_str(&format!("\n{WAITING}{why}, rerunning once the load falls"));
        }
        let mut out = match state {
            RunState::Passed => Settled::Passed,
            RunState::Waiting => Settled::Waiting,
            _ => Settled::Failed,
        };
        if ran.iter().any(|r| matches!(r.done, Done::Malformed(_))) {
            out = Settled::Malformed;
        } else if state == RunState::Waiting {
            t.updated = util::now_iso();
            t.log(&format!("wsp: check {WAITING}{} — the row stays where it is", busy.as_deref().unwrap_or_default()));
        } else if state == RunState::Failed && t.status() == Status::Review {
            let mut owes = owed(ran, read.as_deref().unwrap_or_default());
            if !earlier.is_empty() && ran.iter().any(|r| matches!(r.done, Done::Failed { timed_out: true, .. })) {
                let now = start.map(|s| format!(", then at normal {}", s.line())).unwrap_or_default();
                owes = format!("Timed out twice: under load ({}){now}.\n\n{owes}", earlier.join("; "));
            }
            v::send_back(&mut t, util::now_iso(), "wsp check failed", read.as_deref(), &owes, Some(CHECK_HEAD));
            out = Settled::SentBack;
        }
        if v::save_runs(store, &mut t, &runs).is_err() {
            return Settled::Lost;
        }
        out
    });
    let word = state.as_str();
    let steps: Vec<_> = ran.iter().map(|r| serde_json::json!({ "name": r.step.name, "secs": secs(r.took), "reused": r.reused.is_some() })).collect();
    let mut event = serde_json::json!({ "id": id, "state": word, "steps": ran.len(), "took": took.as_secs(), "step_secs": steps });
    if let Some(start) = start {
        event["start"] = start.json();
        event["load_end"] = serde_json::json!(load_end);
    }
    store.log_event("task-checked", event);
    store.git_commit(&format!("wsp: checked {id} {word}"));
    settled
}

/// What an entry's line says when it timed out under load.
pub(crate) const WAITING: &str = "timed out under load, ";

/// The run timed out (a step stopped by its limit) while the one-minute load,
/// at the start or at the end, stood over the line at which new work is held
/// (`wsp-595`): a timeout on a machine nobody can work on is not a verdict on
/// the change. Says which loads, for the line on the row.
pub(crate) fn timed_out_under_load(ran: &[Ran], start: &Start, load_end: Option<f64>) -> Option<String> {
    ran.iter().find(|r| matches!(r.done, Done::Failed { timed_out: true, .. }))?;
    let over = |l: Option<f64>| l.filter(|l| crate::throttle::load_over_hold(*l, start.cores));
    over(start.load).or(over(load_end))?;
    let f = |l: Option<f64>| l.map(|l| format!("{l:.1}")).unwrap_or_else(|| "?".into());
    Some(format!("load {} at the start, {} at the end, of {} cores", f(start.load), f(load_end), start.cores))
}

const CHECK_HEAD: &str = "`wsp check` failed on this row's landing. What is owed:\n\n";

// ---- the verb -----------------------------------------------------------------

const USAGE: &str = "wsp check <task> [--plan] [--at <commit>] [--since <commit>] [--json]\n       wsp check --inner [<task>] [--run]   the steps this tree's changes call for\n       wsp check --affected [<task>] [--run]   those and the targets the crate and module graphs reach\n       wsp check <project> --barrier [--plan|--barrier-tree] [--at <commit>]\n       wsp check <project> --tiers   its declared steps by tier\n       wsp check <task|project> --set --from FILE   (or `-`) declare its ## Checks";

pub fn check(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    if args.has("inner") || args.has("affected") {
        return inner(store, args);
    }
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    if args.has("set") {
        return set(store, args, &needle, USAGE);
    }
    if args.has("barrier") || args.has("barrier-tree") {
        return barrier(store, args, &needle);
    }
    if args.has("tiers") {
        return tiers(store, &needle);
    }
    let auto = args.has("auto");
    let t = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let at = args.get("at");
    let tg = match target(store, &t, at.as_deref()) {
        Ok(tg) => Target { since: args.get("since"), ..tg },
        Err(why) => {
            if auto {
                crate::store::log_line(store, &format!("check {}: not run — {why}", t.id));
            } else {
                eprintln!("wsp: {why}");
            }
            return if auto { 0 } else { 1 };
        }
    };
    let Plan { steps, changed } = plan(store, &t, &tg, at.as_deref());
    if steps.is_empty() {
        if !auto {
            println!(
                "{} {}",
                p.dim("nothing to run —"),
                p.dim(&format!("{} has no ## Checks, and no rule of {} matches what it changed", t.id, project_of(&t).unwrap_or_default()))
            );
        }
        return 0;
    }
    if args.has("plan") || args.has("dry-run") {
        let ledger = greens::Greens::open(store, &tg.project, &tg.repo);
        let green = |s: &Step| ledger.lookup(&tg.sha, &s.command, greens::Env::Warm);
        if args.json() {
            let rows: Vec<_> = steps
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name, "command": s.command, "limit": s.limit.as_secs(), "tier": tier(s), "why": why_in(s),
                        "green": green(s).map(|g| serde_json::json!({ "env": g.env.word(), "took": g.took, "who": g.who, "when": g.when })),
                    })
                })
                .collect();
            println!("{}", serde_json::json!({ "task": t.id, "commit": tg.sha, "steps": rows }));
            return 0;
        }
        println!("{} at {}", p.bold(&t.id), &tg.sha[..12]);
        for s in &steps {
            println!("  {}", plan_line(s, green(s).as_ref()));
        }
        if !changed.is_empty() {
            println!("  {}", p.dim(&format!("{} changed path(s) read for the rules", changed.len())));
        }
        println!("  {}", p.dim(&format!("in {}", util::contract(&dir_of(store, &tg).join("tree")))));
        return 0;
    }

    // A named commit is a look at another commit — a replay — and writes
    // nothing on the row. The landing is the one that is recorded.
    let record = at.is_none();
    let entry = match (record, args.get("run")) {
        (true, Some(at)) => Some(at),
        (true, None) => match begin(store, &t.id, &tg.sha, stale_after(&steps), true) {
            Some(at) => Some(at),
            None => {
                eprintln!("wsp: a check of {} at {} is already running", t.id, &tg.sha[..12]);
                return 1;
            }
        },
        (false, _) => None,
    };

    let own = member_tree_at(store, &t, &tg.sha, at.is_some());
    let (dir, tree) = match &own {
        Some(own) => (store.state_file("checks").join(format!("{}-tree", t.id)), own.clone()),
        None => {
            let dir = dir_of(store, &tg);
            let tree = dir.join("tree");
            (dir, tree)
        }
    };
    let started = Instant::now();
    let lock = match TreeLock::take(dir.join("lock"), TREE_WAIT) {
        Ok(l) => l,
        Err(why) => return gave_up(store, &t.id, entry.as_deref(), &why),
    };
    let start = if own.is_none() {
        match checkout_started(&tg.repo, &tree, &tg.sha) {
            Ok(start) => start,
            Err(why) => {
                drop(lock);
                return gave_up(store, &t.id, entry.as_deref(), &format!("could not put {} in {}: {why}", &tg.sha[..12], util::contract(&tree)));
            }
        }
    } else {
        Start::of(&tree, Vec::new())
    };
    if !auto && !args.json() {
        println!("{} {} at {} in {} · {}", p.dim("checking"), p.bold(&t.id), &tg.sha[..12], util::contract(&tree), start.line());
    }
    let greens = greens::Greens::open(store, &tg.project, &tg.repo);
    let ledger = (at.is_none() && !args.has("again") && !args.has("force"))
        .then(|| Reuse { greens: &greens, sha: &tg.sha, env: greens::Env::Warm, who: &t.id });
    let ran = run_all(&tree, &steps, &dir.join("last.log"), ledger.as_ref(), |r| {
        if !auto && !args.json() {
            let (mark, tail) = match &r.done {
                Done::Passed if r.reused.is_some() => (p.green("✓"), " · reused".to_string()),
                Done::Passed => (p.green("✓"), String::new()),
                Done::Failed { .. } | Done::Malformed(_) | Done::Hung(_) => (p.red("✗"), format!(" · {}", why_failed(r))),
                Done::Skipped => (p.dim("–"), " · skipped".to_string()),
            };
            println!("  {mark} {} · {}s{tail}", r.step.name, secs(r.took));
        }
    });
    drop(lock);
    let took = started.elapsed();
    let ok = passed(&ran);

    let settled = match &entry {
        Some(at) => settle_started(store, &t.id, at, &ran, took, Some(&start)),
        None => Settled::Lost,
    };
    if args.json() {
        println!(
            "{}",
            serde_json::json!({ "task": t.id, "commit": tg.sha, "passed": ok, "took": took.as_secs(), "reused": tally(&ran).0, "ran": tally(&ran).1, "steps": account(&ran) })
        );
    } else if !auto {
        if !ok {
            let skip = if settled == Settled::Malformed { 0 } else { 2 };
            for l in owed(&ran, &tg.sha).lines().skip(skip) {
                println!("    {}", p.dim(l));
            }
        }
        let tail = match (&settled, record) {
            (Settled::SentBack, _) => format!(" · {} is back at doing with this as what is owed", t.id),
            (Settled::Waiting, _) => format!(" · timed out under load, rerunning once the load falls; {} stays where it is", t.id),
            (_, false) => " · nothing written on the row".to_string(),
            _ => String::new(),
        };
        println!(
            "{} {} {}  {}s · {}{tail}",
            if ok { p.green("✓") } else { p.red("✗") },
            p.bold(&t.id),
            if ok { "passed" } else { "failed" },
            took.as_secs(),
            tally_line(&ran)
        );
    }
    if settled == Settled::SentBack {
        if let Some(t) = store.find_task(&t.id) {
            crate::cmd_task::deliver_reason(store, &t.id, &owed(&ran, &tg.sha));
        }
    }
    if settled == Settled::Malformed {
        let ask = owed(&ran, &tg.sha).replace("<id>", &t.id);
        crate::store::log_line(store, &format!("check {}: malformed — {ask}", t.id));
        crate::cmd_task::deliver_notice(store, &t.id, &ask);
    }
    if record {
        crate::cycle::poke_task(store, &t.id, "checked");
    }
    i32::from(!ok)
}

/// The tier a step belongs to: `inner` (a rule that names a capture, so it is
/// one module's tests), `row` (before review, verify and land), `barrier`, `end`.
pub(crate) fn tier(s: &Step) -> &'static str {
    if s.end {
        "end"
    } else if s.barrier {
        "barrier"
    } else if s.of == Of::Rule && (s.name.contains(':') || (1..=9).any(|n| s.command.contains(&format!("{{{n}}}")))) {
        "inner"
    } else {
        "row"
    }
}

/// `wsp check <row> --plan` as text, and the greens ledger beside it, for a
/// verifier's packet (`compounding-104`): what the row's checks are and which
/// of them the ledger already holds green at the commit.
pub(crate) fn plan_text(store: &Store, t: &Task, tg: &Target) -> String {
    let Plan { steps, .. } = plan(store, t, tg, None);
    if steps.is_empty() {
        return format!(
            "{} has no ## Checks, and no rule of {} matches what it changed",
            t.id,
            project_of(t).unwrap_or_default()
        );
    }
    let ledger = greens::Greens::open(store, &tg.project, &tg.repo);
    let mut out = format!("{} at {}\n", t.id, &tg.sha[..12.min(tg.sha.len())]);
    let mut held = 0;
    for s in &steps {
        let g = ledger.lookup(&tg.sha, &s.command, greens::Env::Warm);
        held += g.is_some() as usize;
        out.push_str(&format!("  {}\n", plan_line(s, g.as_ref())));
    }
    out.push_str(&format!("greens ledger: {held} of {} step(s) green at this commit\n", steps.len()));
    out
}

/// The row's own green at the commit a verifier reads (`wsp-425`): every step
/// of its plan with the ledger's green for it, at that commit or carried to it by
/// the same input hash. `None` when the row has no landing, no plan, or any step
/// without a green — a red or half-run check keeps the verifier's order as it was.
pub(crate) fn greens_held(store: &Store, t: &Task) -> Option<(String, Vec<(String, Green)>)> {
    let tg = target(store, t, None).ok()?;
    let Plan { steps, .. } = plan(store, t, &tg, None);
    if steps.is_empty() {
        return None;
    }
    let ledger = greens::Greens::open(store, &tg.project, &tg.repo);
    let mut held = Vec::new();
    for s in &steps {
        let inputs = crate::affected::key(&tg.repo, &tg.sha, &s.command);
        let g = ledger.lookup_by(&tg.sha, inputs.as_deref(), &s.command, greens::Env::Warm)?;
        held.push((s.command.clone(), g));
    }
    Some((tg.sha, held))
}

/// What a verifier is told about each done piece's `check:` lines: the result
/// of each on the row's tip, from the greens ledger. `None` for a row with no
/// pieces' lines.
pub(crate) fn pieces_results(store: &Store, t: &Task) -> Vec<(usize, Vec<String>)> {
    let steps = crate::pieces::check_steps(t);
    if steps.is_empty() {
        return Vec::new();
    }
    let tg = target(store, t, None).ok();
    let ledger = tg.as_ref().map(|tg| greens::Greens::open(store, &tg.project, &tg.repo));
    let mut out: Vec<(usize, Vec<String>)> = Vec::new();
    for (n, s) in steps {
        let green = tg.as_ref().zip(ledger.as_ref()).and_then(|(tg, l)| {
            let inputs = crate::affected::key(&tg.repo, &tg.sha, &s.command);
            l.lookup_by(&tg.sha, inputs.as_deref(), &s.command, greens::Env::Warm)
        });
        let said = match green {
            Some(g) => format!("{} · green · {}s", s.name, g.took),
            None => format!("{} · not green at this commit (no green in the ledger)", s.name),
        };
        match out.iter_mut().find(|(m, _)| *m == n) {
            Some((_, v)) => v.push(said),
            None => out.push((n, vec![said])),
        }
    }
    out
}

fn plan_line(s: &Step, green: Option<&Green>) -> String {
    let reused = green.map(|g| format!(" · green · reused, {}s", g.took)).unwrap_or_default();
    format!("{} · {} · up to {}s · {} · {}{reused}", s.name, s.command, s.limit.as_secs(), tier(s), why_in(s))
}

/// The steps the paths a tree changed call for, narrowest first: the project's
/// rules with captures, its other rules, the member's own, then every-landing
/// steps. Barrier and end steps are never an inner loop's.
pub(crate) fn inner_steps(store: &Store, t: &Task, changed: &[String]) -> Vec<Step> {
    let mut steps = task_steps(t);
    for r in project_steps(store, t).into_iter().filter(|r| !r.barrier && !r.end) {
        steps.extend(match r.of {
            Of::Rule => expand(&r, changed),
            _ => vec![r],
        });
    }
    steps.retain(|s| !s.barrier && !s.end);
    let rank = |s: &Step| match (tier(s), s.of) {
        ("inner", _) => 0,
        (_, Of::Rule) => 1,
        (_, Of::Task) => 2,
        _ => 3,
    };
    steps.sort_by_key(rank);
    let mut seen = Vec::new();
    steps.retain(|s| {
        let fresh = !seen.contains(&s.command);
        seen.push(s.command.clone());
        fresh
    });
    steps
}

/// What a tree changed against where it left the trunk: tracked changes,
/// committed or not, and files git has not seen.
fn changed_in_tree(tree: &Path) -> Option<Vec<String>> {
    let trunk = crate::cmd_checkout::trunk(tree)?;
    let branch = crate::cmd_checkout::trunk_branch(&trunk)?;
    let base = git(tree, &["merge-base", "HEAD", &branch])?;
    let mut paths: Vec<String> = git(tree, &["diff", "--name-only", base.trim()])?.lines().map(str::to_string).collect();
    paths.extend(git(tree, &["ls-files", "--others", "--exclude-standard"])?.lines().map(str::to_string));
    paths.retain(|l| !l.trim().is_empty());
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// The steps a tree's changes reach, narrowest first (`wsp-361`): the rules'
/// captures, the crates the paths changed in and Vitest's related tests, the
/// project's other rules, the crates that depend on a changed one by distance,
/// then the member's own and every-landing steps. A derived target a declared
/// step already runs is not added again.
pub(crate) fn affected_steps(store: &Store, t: &Task, tree: &Path, changed: &[String]) -> Vec<Step> {
    let mut ranked: Vec<(usize, Step)> = inner_steps(store, t, changed)
        .into_iter()
        .map(|s| {
            let r = match (tier(&s), s.of) {
                ("inner", _) => 0,
                (_, Of::Rule) => 2,
                (_, Of::Task) => 100,
                _ => 101,
            };
            (r, s)
        })
        .collect();
    for (s, d) in crate::affected::derived(tree, changed) {
        if !ranked.iter().any(|(_, r)| r.command == s.command) {
            ranked.push((if d == 0 { 1 } else { 2 + d }, s));
        }
    }
    ranked.sort_by_key(|(r, _)| *r);
    ranked.into_iter().map(|(_, s)| s).collect()
}

/// `wsp check --inner|--affected [<task>] [--run]` — the steps this tree's
/// changes call for. `--inner` reads the declared rules; `--affected` adds the
/// targets the crate graph and Vitest's module graph reach. Named, it reads the
/// member's own tree. A run in a tree with changes records nothing: a dirty
/// tree proves no commit; `--affected --run` in a tree clean at its ready
/// commit reuses and records greens by input hash.
fn inner(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    let run = args.has("run");
    let wide = args.has("affected");
    // `--run` is valued elsewhere, so a task id typed after it arrives as its value.
    let named = args
        .rest
        .first()
        .cloned()
        .or_else(|| args.get("run").filter(|v| v != "true"))
        .or_else(|| args.get("affected").filter(|v| v != "true"));
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(here) = crate::cmd_verify::toplevel(&cwd) else {
        eprintln!("wsp: {} is in no git tree", util::contract(&cwd));
        return 1;
    };
    let dir_name = here.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let found = match &named {
        Some(id) => store.task_or_why(id),
        None => store.task_or_why(&dir_name),
    };
    let t = match found {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    // Named from elsewhere, the member's own tree is read when it has one.
    let tree = match named.is_some() && dir_name != t.id {
        true => crate::cmd_checkout::member_tree(store, &t.id).unwrap_or(here),
        false => here,
    };
    let Some(changed) = changed_in_tree(&tree) else {
        eprintln!("wsp: cannot read what {} changed against the trunk", util::contract(&tree));
        return 1;
    };
    let mut steps = if wide { affected_steps(store, &t, &tree, &changed) } else { inner_steps(store, &t, &changed) };
    let in_tree = |path: &str| std::fs::read_to_string(tree.join(path)).ok();
    for h in structure_steps(&in_tree, &changed) {
        if !steps.iter().any(|s| s.command == h.command) {
            steps.push(h);
        }
    }
    let base = crate::cmd_checkout::trunk(&tree)
        .and_then(|trunk| crate::cmd_checkout::trunk_branch(&trunk))
        .and_then(|branch| git(&tree, &["merge-base", "HEAD", &branch]));
    if let Some(base) = base {
        let base = base.trim().to_string();
        let in_base = |path: &str| git(&tree, &["cat-file", "-e", &format!("{base}:{path}")]).is_some();
        if let Some(l) = layering_step(&|p| tree.join(p).exists(), &in_base, &changed) {
            if !steps.iter().any(|s| s.command == l.command) {
                steps.push(l);
            }
        }
    }
    if steps.is_empty() {
        println!("{} {}", p.dim("nothing to run —"), p.dim(&format!("{} changed path(s) call for no check of {}", changed.len(), t.id)));
        return 0;
    }
    if !run {
        println!("{} · {} changed path(s)", p.bold(&t.id), changed.len());
        for s in &steps {
            println!("  {} · {} · up to {}s · {}", s.name, s.command, s.limit.as_secs(), why_in(s));
        }
        return 0;
    }
    let log = store.state_file("checks").join(format!("{}-inner.log", t.id));
    let _ = std::fs::create_dir_all(store.state_file("checks"));
    let head = git(&tree, &["rev-parse", "HEAD"]).map(|h| h.trim().to_string()).unwrap_or_default();
    let clean = wide && greens::clean_at(&tree, &head);
    let ledger = clean
        .then(|| {
            let project = project_of(&t)?;
            let trunk = crate::cmd_checkout::trunk(&tree)?;
            Some(greens::Greens::open(store, &project, &trunk))
        })
        .flatten();
    let reuse = ledger.as_ref().map(|g| Reuse { greens: g, sha: &head, env: greens::Env::Warm, who: &t.id });
    let ran = run_all(&tree, &steps, &log, reuse.as_ref(), |r| match &r.done {
        Done::Passed if r.reused.is_some() => println!("  {} {} · reused", p.green("✓"), r.step.name),
        Done::Passed => println!("  {} {} · {}s", p.green("✓"), r.step.name, secs(r.took)),
        Done::Skipped => println!("  {} {} · skipped", p.dim("–"), r.step.name),
        _ => {
            println!("  {} {} · {}s · {}", p.red("✗"), r.step.name, secs(r.took), why_failed(r));
            for l in r.tail.lines() {
                println!("    {}", p.dim(l));
            }
        }
    });
    let said = if reuse.is_some() { "greens recorded at the commit" } else { "nothing recorded: a tree with changes proves no commit" };
    println!("{}", p.dim(&format!("{} · {said}", tally_line(&ran))));
    i32::from(!passed(&ran))
}

/// `wsp check <project> --tiers` — the project's own declared steps by tier,
/// and what it leaves undeclared.
fn tiers(store: &Store, project: &str) -> i32 {
    let p = Paint::new();
    let Some(found) = store.project(project) else {
        eprintln!("wsp: no project {project}");
        return 1;
    };
    let steps: Vec<Step> = found.section(SECTION).map(|s| parse_steps(&s, Of::Project)).unwrap_or_default();
    if steps.is_empty() {
        println!("{} {}", p.dim("nothing declared —"), p.dim(&format!("{} has no ## Checks", found.id)));
        return 0;
    }
    println!("{} declares {} step(s)", p.bold(&found.id), steps.len());
    for word in ["inner", "row", "barrier", "end"] {
        for s in steps.iter().filter(|s| tier(s) == word) {
            let on = if s.on.is_empty() { String::new() } else { format!(" on {}", s.on.join(", ")) };
            println!("  {word:<7} {} (up to {}s){on}: {}", s.name, s.limit.as_secs(), s.command);
        }
    }
    if !steps.iter().any(|s| s.barrier) {
        println!("  {}", p.dim("no `at barrier` step: no full suite runs when a group ends"));
    }
    if !steps.iter().any(|s| s.end) {
        println!("  {}", p.dim("no `at end` step: a worklist's cold build runs only the barrier steps"));
    }
    0
}

/// Which tier brought a step in, for a plan and for a row's account.
fn why_in(s: &Step) -> String {
    if s.barrier {
        return "at the barrier".into();
    }
    if s.end {
        return "in the cold build".into();
    }
    match s.of {
        Of::Task => "the member's".into(),
        Of::Rule if s.on.is_empty() => "reached by the changed paths (crate or module graph)".into(),
        Of::Rule => format!("rule on {}", s.on.join(", ")),
        Of::Project => "every landing".into(),
    }
}

/// `wsp check <project> --barrier [--at <commit>]` — the project's `at barrier`
/// steps, run at the trunk's tip (or the named commit), once for a group: the
/// by-hand form of what [`crate::barrier_run`] does for a group. It writes
/// nothing.
fn barrier(store: &Store, args: &Args, project: &str) -> i32 {
    let p = Paint::new();
    let Some(found) = store.project(project) else {
        eprintln!("wsp: no project {project}");
        return 1;
    };
    let steps = barrier_steps(store, &found.id);
    if steps.is_empty() {
        println!("{} {}", p.dim("nothing to run —"), p.dim(&format!("{} declares no `at barrier` check", found.id)));
        return 0;
    }
    let mut repo = None;
    for root in &found.roots {
        repo = crate::cmd_verify::toplevel(&util::expand(root));
        if repo.is_some() {
            break;
        }
    }
    let Some(repo) = repo else {
        eprintln!("wsp: {} has no root to run in", found.id);
        return 1;
    };
    let Some(sha) = args.get("at").map(|a| git(&repo, &["rev-parse", "--verify", "--quiet", &format!("{a}^{{commit}}")])).unwrap_or_else(|| {
        let trunk = crate::cmd_checkout::trunk(&repo)?;
        let branch = crate::cmd_checkout::trunk_branch(&trunk)?;
        git(&trunk, &["rev-parse", &branch])
    }) else {
        eprintln!("wsp: cannot read the commit to run at");
        return 1;
    };
    let tg = Target { sha: sha.trim().to_string(), repo, project: found.id.clone(), since: None };
    if args.has("plan") || args.has("dry-run") || args.has("barrier-tree") {
        println!("{} barrier at {}", p.bold(&found.id), &tg.sha[..12]);
        for s in &steps {
            println!("  {} · {} · up to {}s", s.name, s.command, s.limit.as_secs());
        }
        println!("  {}", p.dim(&barrier_tree_line(store, &tg.project, &tg.repo)));
        return 0;
    }
    let dir = barrier_dir(store, &tg.project, &tg.repo);
    let tree = dir.join("tree");
    let started = Instant::now();
    let lock = match TreeLock::take(dir.join("lock"), TREE_WAIT) {
        Ok(l) => l,
        Err(why) => return gave_up(store, &found.id, None, &why),
    };
    let start = match checkout_started(&tg.repo, &tree, &tg.sha) {
        Ok(start) => start,
        Err(why) => {
            drop(lock);
            return gave_up(store, &found.id, None, &format!("could not put {} in {}: {why}", &tg.sha[..12], util::contract(&tree)));
        }
    };
    println!("{} {} barrier at {} · {}", p.dim("checking"), p.bold(&found.id), &tg.sha[..12], start.line());
    let ran = run_all(&tree, &steps, &dir.join("last.log"), None, |r| {
        let (mark, tail) = match &r.done {
            Done::Passed => (p.green("✓"), String::new()),
            Done::Failed { .. } | Done::Malformed(_) | Done::Hung(_) => (p.red("✗"), format!(" · {}", why_failed(r))),
            Done::Skipped => (p.dim("–"), " · skipped".to_string()),
        };
        println!("  {mark} {} · {}s{tail}", r.step.name, secs(r.took));
    });
    drop(lock);
    let ok = passed(&ran);
    if !ok {
        for l in owed(&ran, &tg.sha).lines().skip(2) {
            println!("    {}", p.dim(l));
        }
    }
    println!("{} {} {}  {}s", if ok { p.green("✓") } else { p.red("✗") }, p.bold(&found.id), if ok { "passed" } else { "failed" }, started.elapsed().as_secs());
    i32::from(!ok)
}

/// `wsp check <task|project> --set --from FILE` — write the `## Checks` section
/// of a project (when the name is one) or of a task. The list is the whole
/// section: it replaces what was there.
fn set(store: &Store, args: &Args, needle: &str, usage: &str) -> i32 {
    // `--from FILE`, or `-` for the stream: the list keeps its lines, which is
    // the whole of it, so it is not folded the way a log entry is.
    let src = args.get("from").or_else(|| args.rest.get(1).cloned().filter(|a| a == "-"));
    let text = match src.as_deref() {
        Some("-") if util::stdin_is_tty() => {
            eprintln!("wsp: nothing is piped in — `-` reads the list from a stream");
            return 2;
        }
        Some("-") => {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
            buf
        }
        Some(path) => match std::fs::read_to_string(util::expand(path)) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("wsp: cannot read {path}: {e}");
                return 1;
            }
        },
        None => {
            eprintln!("usage: {usage}");
            return 2;
        }
    };
    let text = text.trim().to_string();
    let steps = parse_steps(&text, Of::Project);
    if steps.is_empty() {
        eprintln!("wsp: no list item in that text declares a check — `- name (limit): command`");
        return 2;
    }
    let bad = refused_in(&steps);
    if !bad.is_empty() {
        eprintln!("wsp: not written — {} check line(s) cannot start:", bad.len());
        for b in &bad {
            eprintln!("  - {b}");
        }
        eprintln!("  one item per check, `- name: command`; a note after the command goes outside its backticks");
        return 2;
    }
    let what = if let Some(mut proj) = store.project(needle) {
        if steps.iter().any(|s| s.end) && !steps.iter().any(|s| s.barrier) {
            eprintln!("wsp: warning: an `at end` step but no `at barrier` step — the cold build runs the barrier steps with it, and a group's end runs none");
        }
        crate::model::set_section_in(&mut proj.body, SECTION, &text);
        if let Err(e) = store.save_project(&proj) {
            eprintln!("wsp: {e}");
            return 1;
        }
        format!("project {}", proj.id)
    } else {
        let mut t = match store.task_or_why(needle) {
            Ok(t) => t,
            Err(why) => {
                eprintln!("{why}");
                return 1;
            }
        };
        let calls = calls_itself_in(&t.id, &steps);
        if !calls.is_empty() {
            eprintln!("wsp: not written — {} check line(s) call {}'s own check:", calls.len(), t.id);
            for b in &calls {
                eprintln!("  - {b}");
            }
            return 2;
        }
        crate::model::set_section_in(&mut t.body, SECTION, &text);
        if let Err(e) = store.save_task(&t) {
            eprintln!("wsp: {e}");
            return 1;
        }
        t.id
    };
    store.git_commit(&format!("wsp: {what} declares {} check(s)", steps.len()));
    println!("{what}: {} check(s) — {}", steps.len(), steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "));
    0
}

/// A run that could not start: its entry is taken back and the reason said
/// where somebody looks.
fn gave_up(store: &Store, id: &str, entry: Option<&str>, why: &str) -> i32 {
    if let Some(at) = entry {
        abandon(store, id, at);
    }
    crate::store::log_line(store, &format!("check {id}: not run — {why}"));
    eprintln!("wsp: check {id}: not run — {why}");
    1
}

// ---- the triggers ---------------------------------------------------------------

/// Whether the verifier may start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    Clear,
    /// A run is owed or under way for this landing.
    Wait,
}

/// The gate in front of a member's verifier, starting the run it is waiting on
/// when none is.
///
/// Clear for a member with nothing to run, no landing, or no root to run in —
/// none of those is a reason to hold work — and for a landing whose run has
/// finished, either way: a pass opens the way, and a failure has already sent
/// the member back, so one at review again was answered by a person.
pub(crate) fn gate(store: &Store, member: &Task) -> Gate {
    let Some(sha) = crate::repair::commit_of(member) else { return Gate::Clear };
    let same = |read: &Option<String>| read.as_deref().is_some_and(|x| v::same_commit(x, &sha));
    let mine = v::runs(member).into_iter().rev().find(|r| same(&r.read));
    if mine.as_ref().is_some_and(|r| r.state == RunState::Waiting) {
        // Timed out under load: owed a rerun, and the rerun waits for the load.
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        if !crate::throttle::load1().is_none_or(|l| crate::throttle::load_released(l, cores)) {
            return Gate::Wait;
        }
    } else if mine.as_ref().is_some_and(|r| r.state != RunState::Running) {
        return Gate::Clear;
    }
    // A pass already opened at this landing is no answer for a commit with no
    // run: its spawn may have failed (`compound-sup` not found, `wsp-602`) and
    // the verifier gate then owed nothing for ever. The run is owed, and the
    // entry `begin` writes is what keeps it from being asked again.
    let Ok(tg) = target(store, member, None) else { return Gate::Clear };
    let steps = plan(store, member, &tg, None).steps;
    if steps.is_empty() {
        return Gate::Clear;
    }
    if let Some(at) = begin(store, &member.id, &sha, stale_after(&steps), false) {
        if mine.is_none() {
            let short: String = sha.chars().take(12).collect();
            crate::store::log_line(store, &format!("check {}: check owed at {short}, starting it", member.id));
        }
        crate::cycle::launch_out(store, &["check", &member.id, "--auto", "--run", &at]);
    }
    Gate::Wait
}

/// Is a check of this commit under way for the member, within its budget? The
/// cycle's rebase waits on it: a rebase mid-run moves the tip, and the gate
/// would then start a second check at the new one while the first still runs
/// (`wsp-392`: wsp-379 ran three, two overlapping).
pub(crate) fn running_at(store: &Store, member: &Task, sha: &str) -> bool {
    let Some(entry) = v::runs(member).into_iter().rev().find(|r| r.read.as_deref().is_some_and(|x| v::same_commit(x, sha))) else { return false };
    if entry.state != RunState::Running {
        return false;
    }
    let Ok(tg) = target(store, member, None) else { return false };
    let stale = stale_after(&plan(store, member, &tg, None).steps);
    util::epoch_secs() - util::epoch_of(&entry.at) <= stale
}

pub(crate) use crate::verification::carry_pass;

/// What `wsp review` says to a member that has recorded no checks: tier 1 is the
/// member's to write, and a warning rather than a refusal, because design work
/// has nothing to run. Silent for a project with no repository to run in.
pub(crate) fn review_hint(store: &Store, t: &Task) -> Option<String> {
    if !task_steps(t).is_empty() {
        return None;
    }
    let has_root = project_of(t).and_then(|p| store.project(&p)).is_some_and(|p| !p.roots.is_empty());
    has_root.then(|| {
        format!(
            "nothing under ## Checks on {id} — if code changed, record the exact commands that verify it (the targeted tests, any \
             script behind a number you reported) with `wsp check {id} --set -`; wsp replays them at the landing",
            id = t.id
        )
    })
}

/// The checks a rebase onto a moved trunk can have affected. A rebase pulled the
/// trunk in, so what it can have broken is owed a check: a check that cannot be
/// planned is a land that checked nothing, and only an empty answer — planned,
/// and none of it affected — may skip the run.
pub(crate) fn rerun_steps(
    store: &Store,
    task: &str,
    new_head: &str,
    old_base: &str,
    pulled_in: &[String],
    ours: &[String],
) -> Result<Vec<Step>, String> {
    let t = store.find_task(task).ok_or_else(|| format!("{task} is not in the store"))?;
    let tg = target(store, &t, Some(new_head))?;
    let tg = Target { since: Some(old_base.to_string()), ..tg };
    Ok(affected(&plan(store, &t, &tg, Some(new_head)).steps, pulled_in, ours))
}

/// The rerun a land performs in `dir` after its rebase onto a moved trunk: the
/// checks [`rerun_steps`] names, under the tree lock, reusing greens.
pub(crate) fn rerun(
    store: &Store,
    task: &str,
    dir: &Path,
    new_head: &str,
    old_base: &str,
    pulled_in: &[String],
    ours: &[String],
) -> crate::cmd_checkout::Rerun {
    use crate::cmd_checkout::Rerun;
    let steps = match rerun_steps(store, task, new_head, old_base, pulled_in, ours) {
        Ok(steps) => steps,
        Err(why) => return Rerun::Unplanned(why),
    };
    if steps.is_empty() {
        return Rerun::Nothing;
    }
    let lockdir = store.state_file("checks").join(format!("{task}-tree"));
    let _ = std::fs::create_dir_all(&lockdir);
    let lock = match TreeLock::take(lockdir.join("lock"), TREE_WAIT) {
        Ok(l) => l,
        Err(why) => return Rerun::NoLock(why.to_string()),
    };
    let greens = store
        .find_task(task)
        .and_then(|t| target(store, &t, Some(new_head)).ok())
        .map(|tg| crate::greens::Greens::open(store, &tg.project, &tg.repo));
    let reuse = greens.as_ref().map(|g| crate::greens::Reuse { greens: g, sha: new_head, env: crate::greens::Env::Warm, who: task });
    let ran = run_all(dir, &steps, &lockdir.join("land.log"), reuse.as_ref(), |_| {});
    drop(lock);
    if !passed(&ran) {
        return Rerun::Failed(owed(&ran, new_head));
    }
    Rerun::Ran(steps.iter().map(|s| s.name.clone()).collect())
}

/// After `wsp land`: start the run for a row that no list will gate.
pub(crate) fn after_land(store: &Store, id: &str) {
    if let Some(t) = store.find_task(id) {
        let _ = gate(store, &t);
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_line_that_runs_the_rows_own_check_is_read_as_calling_itself() {
        let calls = |id: &str, cmd: &str| !calls_itself_in(id, &parse_steps(&format!("- t (2s): {cmd}"), Of::Task)).is_empty();
        assert!(calls("compounding-520", "wsp check compounding-520"));
        assert!(calls("wsp-638", "cd x && wsp check wsp-638 --json | tail"));
        assert!(calls("wsp-638", "wsp review wsp-638 \"done\""));
        assert!(calls("wsp-638", "wsp submit wsp-638 --dry-run"));
        assert!(calls("wsp-638", "wsp check"), "no row named is the row it runs in");
        assert!(!calls("wsp-638", "wsp check wsp-639"), "another row's check is another row's");
        assert!(!calls("wsp-638", "cargo test wsp_check_wsp-638"));
        assert!(!calls("wsp-638", "wsp show wsp-638"));
    }
    use super::*;

    fn step(text: &str) -> Step {
        parse_steps(text, Of::Project).remove(0)
    }

    #[test]
    fn a_tree_lock_names_its_holder_and_a_wait_behind_it_says_so_once() {
        let env = util::isolated("check-lock-said");
        let path = env.state().join("lock");
        let held = TreeLock::take(path.clone(), Duration::from_secs(1)).unwrap();
        assert!(TreeLock::live(&path));
        let mut said = Vec::new();
        let r = TreeLock::take_saying(path.clone(), Duration::from_secs(0), |by| said.push(by));
        assert!(r.is_err());
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].as_ref().map(|(p, _)| *p), Some(std::process::id()));
        drop(held);
        assert!(!TreeLock::live(&path));
    }

    #[test]
    fn a_step_is_a_name_a_limit_and_a_command() {
        let s = step("- tests (20m): cargo test --release");
        assert_eq!((s.name.as_str(), s.command.as_str(), s.limit), ("tests", "cargo test --release", Duration::from_secs(1200)));
        let s = step("- build: cargo build");
        assert_eq!((s.name.as_str(), s.limit), ("build", DEFAULT_LIMIT));
        let s = step("- quick (90s): ./check.sh");
        assert_eq!(s.limit, Duration::from_secs(90));
    }

    #[test]
    fn a_bare_command_is_a_step_named_for_its_program_and_a_colon_inside_it_is_not_a_name() {
        let s = step("- `test -f docs/numbers.md`");
        assert_eq!((s.name.as_str(), s.command.as_str()), ("test", "test -f docs/numbers.md"));
        let s = step(r#"- grep -q "a: b" notes.txt"#);
        assert_eq!(s.command, r#"grep -q "a: b" notes.txt"#, "the text before `: ` is not a name");
        assert_eq!(s.name, "grep");
        let s = step("- scripts/lanes.sh gate");
        assert_eq!(s.name, "lanes.sh");
    }

    #[test]
    fn prose_and_empty_items_declare_nothing() {
        let text = "Run these in order.\n\n- tests: cargo test\n-\n- \nnot a list item\n* lint (1m): cargo clippy";
        let names: Vec<_> = parse_steps(text, Of::Task).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["tests", "lint"]);
    }

    #[test]
    fn a_limit_that_is_not_one_keeps_the_head_in_the_command_and_the_line_cannot_start() {
        let s = step("- slow (soon): run it");
        assert_eq!(s.command, "slow (soon): run it");
        assert!(unparsable(&s.command).is_some_and(|w| w.contains("label")), "a label left at the front is refused");
        assert!(unparsable(r#"grep -q "a: b" notes.txt"#).is_none(), "shell before a `: ` is not a label");
    }

    #[test]
    fn a_label_of_several_words_runs_only_its_command_with_the_labels_limit() {
        // wsp-609: `web-chrome ctl and workspace: cd …` ran `web-chrome` as the program.
        let s = step("- web-chrome ctl and workspace: cd spikes/web-chrome && npx tsc --noEmit");
        assert_eq!((s.name.as_str(), s.command.as_str()), ("web-chrome ctl and workspace", "cd spikes/web-chrome && npx tsc --noEmit"));
        let s = step("- intents and more (2s): echo only-the-command; sleep 10");
        assert_eq!(s.limit, Duration::from_secs(2));
        let (dir, log) = scratch_log("labelled");
        let r = run_step(&dir, &s, &log);
        assert!(matches!(r.done, Done::Failed { timed_out: true, .. }), "the label's limit is the step's: {:?}", r.done);
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "only-the-command");
        let s = step("- say it: echo said");
        let r = run_step(&dir, &s, &log);
        assert_eq!(r.done, Done::Passed);
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "said", "the label is not run");
    }

    #[test]
    fn a_timed_label_is_everything_before_the_last_time_and_the_exits_grammar_is_the_same_function() {
        // wsp-612: the label is all before the last ` (` that a time and `)` follow.
        let s = step("- lint & types (front) (90s): npm run lint");
        assert_eq!((s.name.as_str(), s.command.as_str(), s.limit), ("lint & types (front)", "npm run lint", Duration::from_secs(90)));
        let s = step("- web suite (20m) on web/**, docs/*.md at barrier: npm test");
        assert_eq!((s.name.as_str(), s.on.len(), s.barrier, s.command.as_str()), ("web suite", 2, true, "npm test"));
        let s = step(r#"- grep -q "x (5m): y" notes.txt"#);
        assert_eq!(s.command, r#"grep -q "x (5m): y" notes.txt"#, "a time inside a quote is the command's");
        let s = step("- unit (5m): echo 'a (1s): b'");
        assert_eq!((s.name.as_str(), s.command.as_str()), ("unit", "echo 'a (1s): b'"), "a label never holds a `: `");

        let ok = timed_line("- web-chrome ctl and workspace (5m): cd spikes/web-chrome && npx tsc --noEmit".trim_start_matches("- "));
        assert_eq!(ok, Ok(("web-chrome ctl and workspace".into(), "5m".into(), "cd spikes/web-chrome && npx tsc --noEmit".into())));
        // A limit of multi-byte characters is no limit, and never a panic (cpd-4944).
        let s = step("- echo done (✓)");
        assert_eq!((s.name.as_str(), s.command.as_str()), ("echo", "echo done (✓)"), "a plain command, as on master");
        assert_eq!(step("- tests (café): cargo test").command, "tests (café): cargo test");
        assert_eq!(limit_of("né"), None);
        for bad in ["tests: cargo test", "cargo test", "slow (soon): true", "t (5m) at barrier: true", "t (5m): ", "tests (café): cargo test", "t (✓): true"] {
            assert!(timed_line(bad).is_err_and(|w| w.contains("label (5m): command")), "{bad}");
        }
    }

    #[test]
    fn a_label_wsp_cannot_read_is_reported_at_the_run_and_nothing_runs() {
        // `echo at nowhere: …` would run `echo`; an `at` naming nothing leaves the label unread.
        let s = step("- echo at nowhere: touch marker");
        assert_eq!(s.command, "echo at nowhere: touch marker");
        let (dir, log) = scratch_log("unread-label");
        let r = run_step(&dir, &s, &log);
        assert!(matches!(&r.done, Done::Malformed(w) if w.contains("label")), "{:?}", r.done);
        assert!(!dir.join("marker").exists() && std::fs::read_to_string(&log).unwrap_or_default().is_empty(), "nothing ran");
        assert!(r.done.is_red());
    }

    #[test]
    fn set_refuses_a_label_it_cannot_read_and_a_first_word_that_is_no_command() {
        let (env, store, _) = world("setlabel", "- old: true", "exit 0\n");
        let file = env.path("bad.md");
        let args = Args::synth("check", &["pj-001"], &[("set", "true"), ("from", &file.display().to_string())]);
        for bad in ["- slow (5 min): true\n", "- tests: wsp-no-such-program-anywhere --run\n"] {
            std::fs::write(&file, bad).unwrap();
            assert_eq!(check(&store, &args), 2, "{bad}");
            assert!(store.find_task("pj-001").unwrap().section("Checks").is_none(), "{bad} was written");
        }
        std::fs::write(&file, "- two words (1m): true\n- path: ./scripts/x.sh\n- env: FOO=1 true\n").unwrap();
        assert_eq!(check(&store, &args), 0);
    }

    #[test]
    fn a_step_with_no_output_and_no_cpu_is_reported_hung_with_its_pids() {
        HUNG_AFTER.with(|h| h.set(Some(Duration::from_secs(1))));
        let (dir, log) = scratch_log("hung");
        let start = Instant::now();
        let r = run_step(&dir, &sh("sleep 30", 60), &log);
        HUNG_AFTER.with(|h| h.set(None));
        assert!(start.elapsed() < Duration::from_secs(20), "ended when hung, not at its limit");
        let Done::Hung(pids) = &r.done else { panic!("{:?}", r.done) };
        assert!(pids.contains("sleep 30"), "{pids}");
        assert!(pids.lines().all(|l| l.split_whitespace().next().is_some_and(|p| p.parse::<u32>().is_ok())), "{pids}");
        assert!(why_failed(&r).starts_with("hung") && r.tail.contains("sleep 30"), "{}", r.tail);
        assert!(r.done.is_red());
        assert!(account(std::slice::from_ref(&r)).contains("hung"));
    }

    fn scratch_log(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("wsp-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("log"))
    }

    fn sh(command: &str, limit: u64) -> Step {
        Step { name: "s".into(), command: command.into(), limit: Duration::from_secs(limit), of: Of::Project, on: Vec::new(), barrier: false, end: false }
    }

    #[test]
    fn exit_zero_passes_and_a_failure_keeps_the_end_of_its_output() {
        let (dir, log) = scratch_log("exit");
        let ok = run_step(&dir, &sh("echo fine", 10), &log);
        assert_eq!(ok.done, Done::Passed);
        let bad = run_step(&dir, &sh("echo one; echo two >&2; exit 3", 10), &log);
        assert_eq!(bad.done, Done::Failed { exit: Some(3), timed_out: false });
        assert!(bad.tail.contains("one") && bad.tail.contains("two"), "both streams: {}", bad.tail);
    }

    #[test]
    fn a_step_past_its_limit_is_killed_with_everything_it_started() {
        let (dir, log) = scratch_log("limit");
        let started = Instant::now();
        let r = run_step(&dir, &sh("sleep 30 & sleep 30", 1), &log);
        assert!(started.elapsed() < Duration::from_secs(10), "waited on the sleep");
        assert_eq!(r.done, Done::Failed { exit: None, timed_out: true });
        assert!(why_failed(&r).contains("timed out after 1s"));
    }

    /// `wsp-453`: a step that passes still has its group killed — the Vite and
    /// Vitest it started stood for hours after the box check.
    #[test]
    fn a_step_that_passes_leaves_nothing_it_started_running() {
        let (dir, log) = scratch_log("leaves");
        let pidfile = dir.join("server.pid");
        let r = run_step(&dir, &sh(&format!("sleep 60 > /dev/null 2>&1 & echo $! > {}", pidfile.display()), 20), &log);
        assert_eq!(r.done, Done::Passed);
        let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
        let alive = || {
            Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
        };
        let until = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "the step's background process {pid} is still running");
    }

    #[test]
    fn a_step_that_fails_once_and_passes_is_passed_and_the_flake_is_kept_on_the_entry() {
        let (dir, log) = scratch_log("flake");
        let flaky = sh("test -f seen || { touch seen; echo first-time >&2; exit 1; }", 10);
        let ran = run_all(&dir, &[flaky], &log, None, |_| {});
        assert!(passed(&ran), "the second run counts");
        let text = account(&ran);
        assert!(text.contains("passed on a second run, the first was exit 1") && text.contains("~ first-time"), "{text}");
        let broken = sh("echo no; exit 2", 10);
        let ran = run_all(&dir, &[broken], &log, None, |_| {});
        assert!(!passed(&ran));
        assert!(account(&ran).contains("exit 2 both times"), "{}", account(&ran));
    }

    #[test]
    fn steps_stop_at_the_first_failure_and_say_which_were_not_run() {
        let (dir, log) = scratch_log("stop");
        let steps = vec![sh("true", 10), sh("exit 1", 10), sh("true", 10)];
        let ran = run_all(&dir, &steps, &log, None, |_| {});
        let done: Vec<_> = ran.iter().map(|r| r.done.clone()).collect();
        assert_eq!(done[0], Done::Passed);
        assert!(matches!(done[1], Done::Failed { .. }));
        assert_eq!(done[2], Done::Skipped);
        assert!(!passed(&ran));
        let text = account(&ran);
        assert!(text.contains("✓ s") && text.contains("✗ s") && text.contains("– s · true · skipped"), "{text}");
    }

    #[test]
    fn a_check_runs_without_wsps_own_environment() {
        let (dir, log) = scratch_log("env");
        std::env::set_var("WSP_CHECK_PROBE", "set");
        let r = run_step(&dir, &sh(r#"test -z "$WSP_CHECK_PROBE""#, 10), &log);
        std::env::remove_var("WSP_CHECK_PROBE");
        assert_eq!(r.done, Done::Passed, "WSP_* reached the command");
    }

    // ---- the row ----

    use crate::model::Project;

    fn git_run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
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
    }

    /// A store with a project rooted at a repository whose one commit holds
    /// `check.sh`, and a task landed on that commit.
    fn world(name: &str, project_checks: &str, script: &str) -> (util::Isolated, Store, String) {
        let env = util::isolated(&format!("check-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        std::fs::write(repo.join("check.sh"), script).unwrap();
        git_run(&repo, &["add", "check.sh"]);
        git_run(&repo, &["commit", "--quiet", "-m", "pj-001: first"]);
        let sha = git(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let store = Store::open();
        let mut p = Project::new("pj");
        p.roots = vec![repo.display().to_string()];
        p.body = format!("## Checks\n{project_checks}\n");
        store.save_project(&p).unwrap();
        let mut t = Task::new("a row", "pj-001");
        t.project = Some("pj".into());
        t.set_status(Status::Review);
        crate::log::note(&store, &mut t, &format!("{} {sha} on pj-001", crate::repair::LANDED)).unwrap();
        (env, store, sha)
    }

    #[test]
    fn a_projects_artefact_globs_are_read_and_an_empty_section_reads_as_none() {
        let env = util::isolated("check-artefacts");
        let _ = &env;
        let store = Store::open();
        let mut p = Project::new("pj");
        p.body = "## Artefacts\n# generated\nCargo.lock\n- docs/*.html\n\n## Checks\n- a: true\n".into();
        store.save_project(&p).unwrap();
        assert_eq!(artefact_globs(&store, "pj"), vec!["Cargo.lock", "docs/*.html"]);
        p.body = "## Artefacts\n- `docs/shots/*.png` — rewritten by `npm run check`\n".into();
        store.save_project(&p).unwrap();
        assert_eq!(artefact_globs(&store, "pj"), vec!["docs/shots/*.png"], "the note after ` — ` is not the glob");
        p.body = "## Artefacts\n# none yet\n\n## Checks\n- a: true\n".into();
        store.save_project(&p).unwrap();
        assert!(artefact_globs(&store, "pj").is_empty());
        p.body = "## Artefacts\n".into();
        store.save_project(&p).unwrap();
        assert!(artefact_globs(&store, "pj").is_empty());
        p.body = "## Checks\n- a: true\n".into();
        store.save_project(&p).unwrap();
        assert!(artefact_globs(&store, "pj").is_empty());
    }

    #[test]
    fn the_d4_key_absent_refuses_a_members_full_suite_and_present_allows_the_named_command() {
        let env = util::isolated("check-d4");
        let _ = &env;
        let store = Store::open();
        let mut p = Project::new("pj");
        p.body = "## Full suite\n- `cargo nextest run --workspace`\n".into();
        store.save_project(&p).unwrap();
        let fs = full_suite(&store, "pj");
        assert!(fs.names("cargo nextest run --workspace"));
        assert!(!fs.member_may_run("cargo nextest run --workspace"));
        assert!(fs.member_may_run("cargo test check::"));
        p.body = "## Full suite\ncargo nextest run --workspace\nmembers: allowed\n".into();
        store.save_project(&p).unwrap();
        let fs = full_suite(&store, "pj");
        assert!(fs.member_may_run("cargo nextest run --workspace"));
        assert!(!fs.names("cargo build"));
        p.body = "## Checks\n- a: true\n".into();
        store.save_project(&p).unwrap();
        assert_eq!(full_suite(&store, "pj"), FullSuite::default());
        assert!(FullSuite::default().member_may_run("anything"));
    }

    #[test]
    fn a_verifiers_order_gives_the_rows_green_at_the_commit_a_carried_green_and_keeps_its_old_text_for_a_red() {
        let (_env, store, sha) = world("order", "- ok: sh check.sh", "exit 0\n");
        let t = store.find_task("pj-001").unwrap();
        let tg = target(&store, &t, None).unwrap();
        let ledger = greens::Greens::open(&store, &tg.project, &tg.repo);
        let before = crate::cycle::verifier_order(&store, &t);
        assert!(!before.contains("do not run it again"), "no green: {before}");
        assert!(greens_held(&store, &t).is_none());

        // A green carried to this commit: it passed at another one with the same inputs.
        let from = "0123456789abcdef0123456789abcdef01234567";
        let key = crate::affected::key(&tg.repo, &sha, "sh check.sh");
        ledger.record_by(from, key.as_deref(), "sh check.sh", greens::Env::Warm, 12, "pj-001", false);
        let carried = crate::cycle::verifier_order(&store, &t);
        assert!(carried.contains("do not run it again"), "{carried}");
        assert!(carried.contains("`sh check.sh` · 12s · warm · carried from 0123456789ab (same inputs, rebased since)"), "{carried}");

        // A green at this very commit is preferred.
        ledger.record(&sha, "sh check.sh", greens::Env::Warm, 41, "pj-001", false);
        let order = crate::cycle::verifier_order(&store, &t);
        assert!(order.contains(&format!("already green at {}: do not run it again", &sha[..12])), "{order}");
        assert!(order.contains("`sh check.sh` · 41s · warm · at this commit · by pj-001"), "{order}");
        assert!(order.contains("Run only what the packet or the row's done-when names that these commands do not cover"), "{order}");
        let line = crate::cycle::verifier_order_line(&store, &t);
        assert!(line.contains("do not run it again") && !line.contains('\n'), "{line}");
    }

    #[test]
    fn a_run_is_recorded_beside_the_verifiers_passes_and_leaves_them_alone() {
        let (_env, store, sha) = world("beside", "- ok: sh check.sh", "exit 0\n");
        v::record(&store, "pj-001", v::State::Holds, "it holds", None, Some("held")).unwrap();
        let t = store.find_task("pj-001").unwrap();
        let tg = target(&store, &t, None).unwrap();
        let at = begin(&store, "pj-001", &tg.sha, 3600, false).unwrap();
        let ran = vec![Ran { step: sh("sh check.sh", 10), done: Done::Passed, took: Duration::from_secs(4), tail: String::new(), first: None, reused: None }];
        assert_eq!(settle(&store, "pj-001", &at, &ran, Duration::from_secs(4)), Settled::Passed);

        let t = store.find_task("pj-001").unwrap();
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].state, runs[0].took), (RunState::Passed, Some(4)));
        assert!(v::same_commit(runs[0].read.as_deref().unwrap(), &sha));
        let passes = v::passes(&t);
        assert_eq!(passes.len(), 1, "{}", t.body);
        assert_eq!(passes[0].text, "it holds", "the run's lines are not read as the pass's verdict");
        // A write of the passes keeps the run, and a write of the run keeps the pass.
        let mut t2 = t.clone();
        v::write(&mut t2, &passes);
        assert_eq!(v::runs(&t2).len(), 1);
        assert!(v::verified(&[t2], "pj-001"), "a passing run is not a verdict, and does not unseat one");
    }

    #[test]
    fn a_run_is_started_once_per_landing_and_again_only_when_asked() {
        let (_env, store, sha) = world("once", "- ok: true", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        assert!(begin(&store, "pj-001", &sha, 3600, false).is_none(), "a second trigger found the first one running");
        let ran = vec![Ran { step: sh("true", 10), done: Done::Passed, took: Duration::ZERO, tail: String::new(), first: None, reused: None }];
        settle(&store, "pj-001", &at, &ran, Duration::ZERO);
        assert!(begin(&store, "pj-001", &sha, 3600, false).is_none(), "a landing already checked is not checked again");
        assert!(begin(&store, "pj-001", &sha, 3600, true).is_some(), "but a person can ask");
        assert_eq!(v::runs(&store.find_task("pj-001").unwrap()).len(), 2);
    }

    #[test]
    fn a_rebase_waits_for_the_check_under_way_and_a_pass_is_carried_to_the_rebased_tip() {
        let (_env, store, sha) = world("carry", "- ok: true", "exit 0\n");
        let other = "0123456789abcdef0123456789abcdef01234567";
        let t = store.find_task("pj-001").unwrap();
        assert!(!running_at(&store, &t, &sha), "no entry, nothing running");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let t = store.find_task("pj-001").unwrap();
        assert!(running_at(&store, &t, &sha), "the rebase must wait for it");
        assert!(!running_at(&store, &t, other), "another tip has no run");
        let mut t2 = t.clone();
        assert!(!carry_pass(&mut t2, &sha, other), "a run still going carries nothing");
        let ran = vec![Ran { step: sh("true", 10), done: Done::Passed, took: Duration::ZERO, tail: String::new(), first: None, reused: None }];
        settle(&store, "pj-001", &at, &ran, Duration::ZERO);
        let mut t = store.find_task("pj-001").unwrap();
        assert!(!running_at(&store, &t, &sha));
        assert!(carry_pass(&mut t, &sha, other));
        assert!(!carry_pass(&mut t, &sha, other), "once");
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 2);
        assert!(runs[1].state == RunState::Passed && v::same_commit(runs[1].read.as_deref().unwrap(), other));
        assert!(!carry_pass(&mut t, "fedcba9876543210fedcba9876543210fedcba98", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "nothing to carry from");
    }

    #[test]
    fn a_running_entry_nobody_finished_is_taken_again_after_its_budget() {
        let (_env, store, sha) = world("stale", "- ok: true", "exit 0\n");
        let mut t = store.find_task("pj-001").unwrap();
        v::save_runs(
            &store,
            &mut t,
            &[Run { at: "2026-01-01T00:00:00Z".into(), state: RunState::Running, read: Some(sha[..12].into()), took: None, text: String::new() }],
        )
        .unwrap();
        assert!(begin(&store, "pj-001", &sha, 3600, false).is_some());
        assert_eq!(v::runs(&store.find_task("pj-001").unwrap()).len(), 1, "the dead entry was replaced, not kept");
    }

    #[test]
    fn a_failure_on_a_row_at_review_sends_it_back_in_the_same_write_with_the_step_as_what_is_owed() {
        let (_env, store, sha) = world("sendback", "- tests: sh check.sh", "echo boom >&2; exit 7\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let ran = vec![Ran {
            step: Step { name: "tests".into(), command: "sh check.sh".into(), limit: DEFAULT_LIMIT, of: Of::Project, on: Vec::new(), barrier: false, end: false },
            done: Done::Failed { exit: Some(7), timed_out: false },
            took: Duration::from_secs(2),
            tail: "boom".into(),
            first: None,
            reused: None,
        }];
        assert_eq!(settle(&store, "pj-001", &at, &ran, Duration::from_secs(2)), Settled::SentBack);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.status(), Status::Doing);
        let log = t.section("Log").unwrap();
        assert!(log.contains("sent back: wsp check failed at") && log.contains("`tests`") && log.contains("exit 7"), "{log}");
        let run = &v::runs(&t)[0];
        assert_eq!(run.state, RunState::Failed);
        assert!(run.text.contains("✗ tests · sh check.sh · 2s · exit 7") && run.text.contains("  | boom"), "{}", run.text);
    }

    fn timed_out() -> Vec<Ran> {
        vec![Ran {
            step: Step { name: "build".into(), command: "cargo build".into(), limit: Duration::from_secs(900), of: Of::Project, on: Vec::new(), barrier: false, end: false },
            done: Done::Failed { exit: None, timed_out: true },
            took: Duration::from_secs(900),
            tail: String::new(),
            first: None,
            reused: None,
        }]
    }

    fn at_load(load: f64) -> Start {
        Start { target: "warm", seed: Vec::new(), load: Some(load), cores: 8 }
    }

    #[test]
    fn a_timeout_under_load_leaves_the_row_at_review_and_is_run_again_not_sent_back() {
        let (_env, store, sha) = world("busy", "- build: cargo build", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let ran = timed_out();
        assert_eq!(settle_loaded(&store, "pj-001", &at, &ran, Duration::from_secs(900), Some(&at_load(19.0)), Some(19.0)), Settled::Waiting);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.status(), Status::Review, "not sent back");
        let run = &v::runs(&t)[0];
        assert_eq!(run.state, RunState::Waiting);
        assert!(run.text.contains("timed out under load, load 19.0 at the start") && run.text.contains("rerunning"), "{}", run.text);
        assert!(!t.section("Log").unwrap().contains("sent back"));
        assert!(begin(&store, "pj-001", &sha, 3600, false).is_some(), "the rerun may start");
        assert_eq!(v::runs(&store.find_task("pj-001").unwrap()).len(), 2, "the first attempt stays on the row");
    }

    #[test]
    fn a_timeout_at_normal_load_is_a_red_check() {
        let (_env, store, sha) = world("calm", "- build: cargo build", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        assert_eq!(settle_loaded(&store, "pj-001", &at, &timed_out(), Duration::from_secs(900), Some(&at_load(2.0)), Some(2.0)), Settled::SentBack);
    }

    #[test]
    fn a_second_timeout_at_normal_load_is_sent_back_with_both_loads_in_the_reason() {
        let (_env, store, sha) = world("twice", "- build: cargo build", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        settle_loaded(&store, "pj-001", &at, &timed_out(), Duration::from_secs(900), Some(&at_load(30.0)), Some(30.0));
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        assert_eq!(settle_loaded(&store, "pj-001", &at, &timed_out(), Duration::from_secs(900), Some(&at_load(2.0)), Some(2.0)), Settled::SentBack);
        let log = store.find_task("pj-001").unwrap().section("Log").unwrap();
        assert!(log.contains("Timed out twice") && log.contains("load 30.0") && log.contains("load 2.0"), "{log}");
    }

    #[test]
    fn a_note_after_the_backticked_command_is_not_part_of_it() {
        let s = step("- `cd spikes/web-chrome && npm run check` (tsc, 33 Vitest, 11 Playwright on WebKit)");
        assert_eq!(s.command, "cd spikes/web-chrome && npm run check");
        assert!(unparsable(&s.command).is_none());
        assert_eq!(step("- web: `npm test` — 33 tests").command, "npm test");
        assert_eq!(step("- `echo `date``").command, "echo `date`");
        assert_eq!(step("- `make test`").command, "make test");
    }

    #[test]
    fn set_refuses_a_line_that_does_not_parse_and_writes_nothing() {
        let (env, store, _) = world("setbad", "- old: true", "exit 0\n");
        let file = env.path("bad.md");
        std::fs::write(&file, "- ok: true\n- broken: cd x &&\n").unwrap();
        let args = Args::synth("check", &["pj-001"], &[("set", "true"), ("from", &file.display().to_string())]);
        assert_eq!(check(&store, &args), 2);
        assert!(store.find_task("pj-001").unwrap().section("Checks").is_none(), "refused before it was written");
        std::fs::write(&file, "- ok: true\n").unwrap();
        assert_eq!(check(&store, &args), 0);
    }

    #[test]
    fn a_step_that_cannot_start_is_malformed_and_never_run() {
        let dir = std::env::temp_dir();
        let log = dir.join(format!("wsp-malformed-{}.log", std::process::id()));
        let broken = Ran { ..run_step(&dir, &sh("cd x &&", 30), &log) };
        assert!(matches!(broken.done, Done::Malformed(_)), "{:?}", broken.done);
        let missing = run_step(&dir, &sh("wsp-no-such-command-anywhere", 30), &log);
        assert!(matches!(missing.done, Done::Malformed(_)), "a command that is not found is not a red test: {:?}", missing.done);
        let red = run_step(&dir, &sh("exit 3", 30), &log);
        assert!(matches!(red.done, Done::Failed { exit: Some(3), .. }), "and a real failure still is one");
    }

    #[test]
    fn a_malformed_step_on_a_row_at_review_records_the_run_and_does_not_send_the_row_back() {
        let (_env, store, sha) = world("malformed", "- t: true", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let ran = vec![Ran {
            step: Step { name: "t".into(), command: "cd x &&".into(), limit: DEFAULT_LIMIT, of: Of::Project, on: Vec::new(), barrier: false, end: false },
            done: Done::Malformed("syntax error: unexpected end of file".into()),
            took: Duration::from_secs(1),
            tail: "syntax error: unexpected end of file".into(),
            first: None,
            reused: None,
        }];
        assert_eq!(settle(&store, "pj-001", &at, &ran, Duration::from_secs(1)), Settled::Malformed);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.status(), Status::Review, "the row stays where it was");
        assert!(!t.section("Log").unwrap().contains("sent back"));
        let run = &v::runs(&t)[0];
        assert_eq!(run.state, RunState::Failed);
        assert!(run.text.contains("malformed"), "{}", run.text);
        assert!(owed(&ran, &sha).contains("--set"), "and what it asks is for the line to be fixed");
        assert_eq!(gate(&store, &t), Gate::Clear, "the verifier is not held for a declaration");
    }

    #[test]
    fn a_failure_on_a_row_still_at_doing_is_recorded_and_nobody_is_moved() {
        let (_env, store, sha) = world("doing", "- t: false", "exit 1\n");
        let mut t = store.find_task("pj-001").unwrap();
        t.set_status(Status::Doing);
        store.save_task(&t).unwrap();
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let ran = vec![Ran { step: sh("false", 10), done: Done::Failed { exit: Some(1), timed_out: false }, took: Duration::ZERO, tail: String::new(), first: None, reused: None }];
        assert_eq!(settle(&store, "pj-001", &at, &ran, Duration::ZERO), Settled::Failed);
        assert_eq!(store.find_task("pj-001").unwrap().status(), Status::Doing);
    }

    #[test]
    fn the_gate_holds_a_landed_member_with_checks_until_the_run_has_passed() {
        let (_env, store, sha) = world("gate", "- ok: true", "exit 0\n");
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(gate(&store, &t), Gate::Wait, "no run for this landing yet");
        let t = store.find_task("pj-001").unwrap();
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 1, "the gate wrote the entry that keeps a second trigger from starting another");
        assert_eq!(runs[0].state, RunState::Running);
        assert_eq!(gate(&store, &t), Gate::Wait, "and it is still running");
        assert_eq!(v::runs(&store.find_task("pj-001").unwrap()).len(), 1);

        let ran = vec![Ran { step: sh("true", 10), done: Done::Passed, took: Duration::ZERO, tail: String::new(), first: None, reused: None }];
        settle(&store, "pj-001", &runs[0].at, &ran, Duration::ZERO);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(gate(&store, &t), Gate::Clear, "passed for {sha}");
    }

    /// `wsp-602`: a pass opened at the landing whose spawn failed is no run of
    /// the check; the gate still owes one, and says so.
    #[test]
    fn a_pass_with_no_run_at_its_commit_does_not_clear_the_gate() {
        let (_env, store, sha) = world("owed", "- ok: true", "exit 0\n");
        let mut t = store.find_task("pj-001").unwrap();
        let mut passes = v::passes(&t);
        passes.push(v::Pass::opened(Some(sha.clone()), None));
        v::write(&mut t, &passes);
        store.save_task(&t).unwrap();
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(gate(&store, &t), Gate::Wait, "the check is owed at {sha}");
        let t = store.find_task("pj-001").unwrap();
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 1, "and its entry is written");
        assert_eq!(runs[0].state, RunState::Running);
        let short: String = sha.chars().take(12).collect();
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert!(log.contains(&format!("check owed at {short}, starting it")), "{log}");
    }

    #[test]
    fn the_gate_is_clear_where_nothing_is_declared_nothing_landed_or_nothing_can_run() {
        let (_env, store, _) = world("clear", "", "exit 0\n");
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(gate(&store, &t), Gate::Clear, "a project that declares nothing");

        let mut t = store.find_task("pj-001").unwrap();
        t.body = "## Checks\n- ok: true\n".into();
        store.save_task(&t).unwrap();
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(task_steps(&t).len(), 1, "a task's own checks count on their own");
        assert_eq!(gate(&store, &t), Gate::Clear, "no landing recorded");
    }

    #[test]
    fn the_members_steps_come_first_then_the_rules_its_paths_call_for_then_every_landing_ones() {
        let rules = "- always: true\n- docs on docs/**: test -f docs/a\n- src on src/**,lib/*.rs: test -f src/b\n- full (30m) at barrier: false";
        let (_env, store, _) = world("tiers", rules, "exit 0\n");
        let mut t = store.find_task("pj-001").unwrap();
        t.body.push_str("\n## Checks\n- `test -f check.sh`\n- always-too: true\n");
        let tg = target(&store, &t, None).unwrap();
        let got = plan(&store, &t, &tg, None);
        let names: Vec<_> = got.steps.iter().map(|s| (s.name.as_str(), s.of)).collect();
        // The commit is `pj-001: first` and changed only check.sh, so neither path rule matches,
        // the barrier step is nobody's per row, and `true` named twice runs once.
        assert_eq!(names, [("test", Of::Task), ("always-too", Of::Task)], "{names:?}");
        assert_eq!(got.changed, Vec::<String>::new().into_iter().chain(["check.sh".to_string()]).collect::<Vec<_>>());
    }

    #[test]
    fn a_rows_plan_lists_every_done_pieces_check_lines_after_its_own_labelled_by_number() {
        let (_env, store, _) = world("pieces", "", "exit 0\n");
        let mut t = store.find_task("pj-001").unwrap();
        t.body.push_str("\n## Checks\n- own: true own\n");
        let mut rec = crate::pieces::Record::default();
        for (n, label) in [(1, "types"), (2, "board"), (3, "rail")] {
            let mut p = crate::pieces::Piece::pending(n, label, 1);
            p.state = if n == 3 { crate::pieces::State::Pending } else { crate::pieces::State::Done };
            p.checks = vec![format!("{label} (1m): true {label}")];
            rec.pieces.push(p);
        }
        let mut p = crate::pieces::Piece::pending(4, "again", 2);
        p.state = crate::pieces::State::Done;
        p.checks = vec!["again: true own".into()];
        rec.pieces.push(p);
        crate::pieces::write(&mut t, &rec);
        let tg = target(&store, &t, None).unwrap();
        let names: Vec<_> = plan(&store, &t, &tg, None).steps.into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["own", "1.types", "2.board"], "a pending piece's lines and a command the row ran already are not added");
    }

    /// A commit on the test repo that writes `files` and returns its sha.
    fn commit_files(repo: &Path, files: &[(&str, &str)]) -> String {
        for (path, text) in files {
            let full = repo.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, text).unwrap();
            git_run(repo, &["add", path]);
        }
        git_run(repo, &["commit", "--quiet", "-m", "pj-001: more"]);
        git(repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
    }

    #[test]
    fn a_help_source_in_the_diff_adds_the_help_test_and_a_row_that_touches_none_does_not() {
        let (env, store, base) = world("help-step", "- ok: true", "exit 0\n");
        let repo = env.path("repo");
        let t = store.find_task("pj-001").unwrap();
        let names = |tg: &Target| plan(&store, &t, tg, Some(&tg.sha)).steps;
        let at = |sha: &str| Target { since: Some(base.clone()), ..target(&store, &t, Some(sha)).unwrap() };

        // A repository with a snapshot, and a change that leaves help alone.
        let with_fixture = commit_files(&repo, &[(HELP_FIXTURE, "help\n")]);
        let docs = commit_files(&repo, &[("docs/x.md", "x\n")]);
        let steps = names(&at(&docs));
        assert!(steps.iter().all(|s| s.name != "help"), "{steps:?}");
        let _ = with_fixture;

        // A help line added to a module's VERBS, and the snapshot not touched.
        let help_line = commit_files(&repo, &[("src/cmd_new.rs", "pub const VERBS: &[crate::verbs::Verb] = &[];\n")]);
        let steps = names(&at(&help_line));
        let help = steps.iter().find(|s| s.name == "help").unwrap_or_else(|| panic!("no help step in {steps:?}"));
        assert_eq!(help.command, format!("cargo test {HELP_TEST}"));
        assert_eq!(help.on, ["src/cmd_new.rs"]);
        assert_eq!(help.of, Of::Rule);

        // The snapshot updated beside it: the step still runs, and its test passes.
        let both = commit_files(&repo, &[("src/cmd_new.rs", "pub const VERBS: &[crate::verbs::Verb] = &[];\n// more\n"), (HELP_FIXTURE, "help 2\n")]);
        assert!(names(&at(&both)).iter().any(|s| s.name == "help"));
    }

    #[test]
    fn a_module_or_verb_in_the_diff_adds_the_layering_and_readonly_tests() {
        let kept = |p: &str| matches!(p, LAYERING_FILE | READONLY_FILE).then(|| "x".to_string());
        let changed = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let cmds = |p: &[&str]| structure_steps(&kept, &changed(p)).into_iter().map(|s| s.command).collect::<Vec<_>>();
        assert_eq!(cmds(&["src/main.rs"]), [LAYERING_COMMAND, READONLY_COMMAND]);
        assert!(cmds(&["src/util.rs", "README.md"]).is_empty());
        let none = |_: &str| None;
        assert!(structure_steps(&none, &changed(&["src/main.rs"])).is_empty());
    }

    #[test]
    fn the_help_step_needs_a_snapshot_in_the_repository_and_a_source_that_feeds_the_help() {
        let reads = |files: &'static [(&'static str, &'static str)]| {
            move |p: &str| files.iter().find(|(n, _)| *n == p).map(|(_, c)| c.to_string())
        };
        let changed = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // No snapshot: not this repository's business.
        let none = reads(&[("src/main.rs", "fn main() {}")]);
        assert!(help_step(&none, &changed(&["src/main.rs"])).is_none());
        // A snapshot, and the entry point, the registry or a table of verbs changed.
        let kept = reads(&[(HELP_FIXTURE, "h"), ("src/main.rs", "fn main() {}"), ("src/x.rs", "pub const VERBS: &[V] = &[];"), ("src/y.rs", "fn y() {}"), ("docs/z.md", "const VERBS")]);
        for p in [&["src/main.rs"][..], &["src/x.rs"], &["src/y.rs", "src/x.rs"]] {
            assert!(help_step(&kept, &changed(p)).is_some(), "{p:?}");
        }
        for p in [&["src/y.rs"][..], &["docs/z.md"], &[]] {
            assert!(help_step(&kept, &changed(p)).is_none(), "{p:?}");
        }
    }

    #[test]
    fn the_layering_step_runs_for_a_row_that_adds_a_module_and_for_no_other() {
        let tree = ["tests/layering.rs", "src/old.rs", "src/dir/a.rs", "src/new.rs", "src/dir/b.rs", "src/fresh/mod.rs", "crates/store/src/more.rs", "crates/store/src/lib.rs", "docs/x.rs"];
        let has = |p: &str| tree.contains(&p);
        let base = |p: &str| ["src/old.rs", "src/dir", "src/dir/a.rs", "crates/store/src/lib.rs"].contains(&p);
        let changed = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A file of a module the base has, or a root file, adds no module.
        for p in [&["src/old.rs"][..], &["src/dir/b.rs"], &["crates/store/src/lib.rs"], &["docs/x.rs"], &[]] {
            assert!(layering_step(&has, &base, &changed(p)).is_none(), "{p:?}");
        }
        // A new file, a new directory, a new file of a crate.
        for (p, on) in [("src/new.rs", "src/new.rs"), ("src/fresh/mod.rs", "src/fresh/mod.rs"), ("crates/store/src/more.rs", "crates/store/src/more.rs")] {
            let s = layering_step(&has, &base, &changed(&["src/old.rs", p])).unwrap_or_else(|| panic!("no step for {p}"));
            assert_eq!((s.command.as_str(), s.on.as_slice()), (LAYERING_COMMAND, &[on.to_string()][..]));
        }
        // A repository with no layering test has nothing to run.
        assert!(layering_step(&|p| p != LAYERING_FILE && has(p), &base, &changed(&["src/new.rs"])).is_none());
    }

    #[test]
    fn a_new_module_in_the_diff_adds_the_layering_test_to_the_plan() {
        let (env, store, _base) = world("layering-step", "- ok: true", "exit 0\n");
        let repo = env.path("repo");
        let t = store.find_task("pj-001").unwrap();
        commit_files(&repo, &[(LAYERING_FILE, "// test\n"), ("src/old.rs", "pub fn f() {}\n")]);
        let since = git(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let steps = |sha: &str| {
            let tg = Target { since: Some(since.clone()), ..target(&store, &t, Some(sha)).unwrap() };
            plan(&store, &t, &tg, Some(&tg.sha)).steps
        };
        let edit = commit_files(&repo, &[("src/old.rs", "pub fn f() { }\n")]);
        assert!(steps(&edit).iter().all(|s| s.name != "layering"));
        let add = commit_files(&repo, &[("src/brand_new.rs", "pub fn g() {}\n")]);
        let got = steps(&add);
        let l = got.iter().find(|s| s.name == "layering").unwrap_or_else(|| panic!("no layering step in {got:?}"));
        assert_eq!(l.command, LAYERING_COMMAND);
    }

    #[test]
    fn a_rule_joins_when_a_changed_path_matches_and_not_before() {
        let rules = "- top on **/*.sh: test -f check.sh\n- other on docs/: test -d docs";
        let (_env, store, _) = world("rule", rules, "exit 0\n");
        let t = store.find_task("pj-001").unwrap();
        let tg = target(&store, &t, None).unwrap();
        let steps = plan(&store, &t, &tg, None).steps;
        assert_eq!(steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["top"], "check.sh matches **/*.sh, and docs/ is not touched");
        assert_eq!(steps[0].of, Of::Rule);
    }

    #[test]
    fn the_since_clause_a_land_wrote_bounds_the_paths_to_that_landing() {
        let (env, store, sha) = world("since", "- src on src/**: true", "exit 0\n");
        let repo = env.path("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "x").unwrap();
        git_run(&repo, &["add", "src"]);
        git_run(&repo, &["commit", "--quiet", "-m", "someone else's work"]);
        let tip = git(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let mut t = store.find_task("pj-001").unwrap();
        crate::log::note(&store, &mut t, &format!("{} {tip} on master · since {sha}", crate::repair::LANDED)).unwrap();
        let t = store.find_task("pj-001").unwrap();
        let tg = target(&store, &t, None).unwrap();
        assert_eq!(since_of(&t).as_deref(), Some(sha.as_str()));
        assert_eq!(plan(&store, &t, &tg, None).changed, ["src/a.rs"], "the diff since, whatever the subject said");
        // Named at the older commit, the diff is the row's own commits up to it.
        let tg = target(&store, &t, Some(&sha)).unwrap();
        assert_eq!(plan(&store, &t, &tg, Some(&sha)).changed, ["check.sh"]);
    }

    #[test]
    fn globs_stay_in_a_segment_unless_they_are_double_starred() {
        assert!(matches("crates/host-ui/**", "crates/host-ui/src/lib.rs"));
        assert!(matches("crates/host-ui/", "crates/host-ui/src/lib.rs"), "a trailing slash is the directory");
        assert!(matches("**/*.rs", "src/a/b.rs") && matches("**/*.rs", "b.rs"));
        assert!(matches("src/*.rs", "src/a.rs") && !matches("src/*.rs", "src/a/b.rs"));
        assert!(matches("src/?.rs", "src/a.rs") && !matches("src/?.rs", "src/ab.rs"));
        assert!(!matches("crates/host-ui/**", "crates/host-views/src/lib.rs"));
        assert!(matches("Cargo.toml", "Cargo.toml") && !matches("Cargo.toml", "x/Cargo.toml"));
    }

    #[test]
    fn a_rule_runs_once_for_each_thing_it_names_in_the_paths_that_changed() {
        let rule = step("- crate on crates/*/**: cargo test -p {1}");
        let changed: Vec<String> = ["crates/host-ui/src/a.rs", "crates/host-ui/src/b.rs", "crates/sup/x.rs", "docs/a.md"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let made = expand(&rule, &changed);
        let cmds: Vec<_> = made.iter().map(|s| s.command.as_str()).collect();
        assert_eq!(cmds, ["cargo test -p host-ui", "cargo test -p sup"]);
        assert_eq!(made[0].name, "crate:host-ui");
        assert_eq!(captures("src/*.rs", "src/cmd_task.rs"), Some(vec!["cmd_task".to_string()]));
        assert_eq!(captures("crates/*/**", "crates/host-ui/src/a.rs"), Some(vec!["host-ui".to_string()]));
        let plain_rule = step("- all on src/**: cargo build");
        assert_eq!(expand(&plain_rule, &changed).len(), 0, "no path of src/ changed");
        assert_eq!(expand(&plain_rule, &["src/x.rs".to_string(), "src/y.rs".to_string()]).len(), 1, "named nothing: once");
    }

    #[test]
    fn a_capture_that_is_not_a_plain_name_makes_no_step() {
        let rule = step("- m on src/*.rs: cargo test {1}");
        let changed = vec!["src/$(touch pwned).rs".to_string(), "src/ok.rs".to_string()];
        let cmds: Vec<_> = expand(&rule, &changed).into_iter().map(|s| s.command).collect();
        assert_eq!(cmds, ["cargo test ok"], "a filename is text, and it does not reach a shell");
    }

    #[test]
    fn a_rule_head_reads_paths_and_a_barrier_and_a_command_with_a_colon_is_still_a_command() {
        let s = step("- ui (15m) on crates/host-ui/**, goldens/: cargo test -p host-ui");
        assert_eq!((s.name.as_str(), s.of, s.limit.as_secs()), ("ui", Of::Rule, 900));
        assert_eq!(s.on, ["crates/host-ui/**", "goldens/"]);
        let s = step("- full (45m) at barrier: cargo test --workspace");
        assert!(s.barrier && s.on.is_empty() && s.of == Of::Project);
        let s = step("- ui on x/** at barrier: y");
        assert!(s.barrier && s.on == ["x/**"]);
        let s = step("- on x: y");
        assert_eq!(s.command, "on x: y", "`on x` has no name before it");
        let s = step("- ui on : y");
        assert_eq!(s.command, "ui on : y", "an `on` with no paths is not a head");
    }

    fn repo_at_head(name: &str) -> (PathBuf, PathBuf, String) {
        let (dir, log) = scratch_log(name);
        crate::testkit::repo(&dir);
        std::fs::write(dir.join("f"), "1").unwrap();
        git_run(&dir, &["add", "f"]);
        git_run(&dir, &["commit", "--quiet", "-m", "one"]);
        let sha = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        (dir, log, sha)
    }

    fn counting(dir: &Path, file: &str) -> Step {
        sh(&format!("echo x >> {}/{file}", dir.display()), 10)
    }

    fn runs_of(dir: &Path, file: &str) -> usize {
        std::fs::read_to_string(dir.join(file)).map(|t| t.lines().count()).unwrap_or(0)
    }

    #[test]
    fn a_pass_is_recorded_at_the_commit_and_reused_by_the_next_run_of_the_same_command() {
        let (_env, store, _) = world("reuse", "- x: true", "exit 0\n");
        let (dir, log, sha) = repo_at_head("reuse");
        let g = greens::Greens::open(&store, "pj", &dir);
        let reuse = Reuse { greens: &g, sha: &sha, env: greens::Env::Warm, who: "pj-001" };
        let step = counting(&dir, "count");
        let first = run_all(&dir, &[step.clone()], &log, Some(&reuse), |_| {});
        assert!(passed(&first) && first[0].reused.is_none());
        assert!(g.lookup(&sha, &step.command, greens::Env::Warm).is_some());
        let second = run_all(&dir, &[step], &log, Some(&reuse), |_| {});
        assert!(passed(&second));
        assert_eq!(runs_of(&dir, "count"), 1, "the second run did not run the command");
        assert_eq!(second[0].took, Duration::ZERO);
    }

    #[test]
    fn a_green_is_reused_at_a_new_commit_whose_inputs_are_unchanged_and_the_run_says_so() {
        let (_env, store, _) = world("reuse-inputs", "- x: true", "exit 0\n");
        let (dir, log, sha) = repo_at_head("reuse-inputs");
        let g = greens::Greens::open(&store, "pj", &dir);
        let step = counting(&dir, "count");
        let reuse = Reuse { greens: &g, sha: &sha, env: greens::Env::Warm, who: "a" };
        assert_eq!(tally_line(&run_all(&dir, &[step.clone()], &log, Some(&reuse), |_| {})), "0 reused, 1 ran");
        git_run(&dir, &["commit", "--quiet", "--allow-empty", "-m", "a rebase that changed nothing"]);
        let sha2 = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let reuse = Reuse { greens: &g, sha: &sha2, env: greens::Env::Warm, who: "a" };
        let again = run_all(&dir, &[step], &log, Some(&reuse), |_| {});
        assert_eq!(runs_of(&dir, "count"), 1, "the same inputs at another commit are not run again");
        assert_eq!(tally_line(&again), "1 reused, 0 ran");
        assert!(account(&again).contains("with the same inputs"));
    }

    #[test]
    fn a_different_command_or_commit_is_not_reused() {
        let (_env, store, _) = world("reuse-other", "- x: true", "exit 0\n");
        let (dir, log, sha) = repo_at_head("reuse-other");
        let g = greens::Greens::open(&store, "pj", &dir);
        let reuse = Reuse { greens: &g, sha: &sha, env: greens::Env::Warm, who: "a" };
        run_all(&dir, &[counting(&dir, "one")], &log, Some(&reuse), |_| {});
        run_all(&dir, &[counting(&dir, "two")], &log, Some(&reuse), |_| {});
        assert_eq!((runs_of(&dir, "one"), runs_of(&dir, "two")), (1, 1));
        std::fs::write(dir.join("f"), "2").unwrap();
        git_run(&dir, &["commit", "--quiet", "-am", "two"]);
        let sha2 = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let reuse = Reuse { greens: &g, sha: &sha2, env: greens::Env::Warm, who: "a" };
        run_all(&dir, &[counting(&dir, "one")], &log, Some(&reuse), |_| {});
        assert_eq!(runs_of(&dir, "one"), 2, "a green at the old commit says nothing of the new one");
    }

    #[test]
    fn a_dirty_tree_records_nothing_and_a_failing_step_is_never_recorded() {
        let (_env, store, _) = world("reuse-dirty", "- x: true", "exit 0\n");
        let (dir, log, sha) = repo_at_head("reuse-dirty");
        let g = greens::Greens::open(&store, "pj", &dir);
        let reuse = Reuse { greens: &g, sha: &sha, env: greens::Env::Warm, who: "a" };
        let bad = sh("exit 3", 10);
        assert!(!passed(&run_all(&dir, &[bad.clone()], &log, Some(&reuse), |_| {})));
        assert!(g.lookup(&sha, &bad.command, greens::Env::Warm).is_none());
        std::fs::write(dir.join("untracked"), "x").unwrap();
        let step = counting(&dir, "count");
        run_all(&dir, &[step.clone()], &log, Some(&reuse), |_| {});
        assert!(g.lookup(&sha, &step.command, greens::Env::Warm).is_none(), "a dirty tree proved the diff, not the commit");
        std::fs::remove_file(dir.join("untracked")).unwrap();
        std::fs::remove_file(dir.join("count")).unwrap();
        std::fs::write(dir.join(".gitignore"), "count\nlog\n").unwrap();
        git_run(&dir, &["add", ".gitignore"]);
        git_run(&dir, &["commit", "--quiet", "-m", "ignore"]);
        let sha3 = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let reuse = Reuse { greens: &g, sha: &sha3, env: greens::Env::Warm, who: "a" };
        run_all(&dir, &[step.clone()], &log, Some(&reuse), |_| {});
        assert!(g.lookup(&sha3, &step.command, greens::Env::Warm).is_some(), "ignored files do not make a tree dirty");
    }

    #[test]
    fn a_cold_green_satisfies_a_warm_run_and_a_warm_green_does_not_satisfy_a_cold_one() {
        let (_env, store, _) = world("reuse-env", "- x: true", "exit 0\n");
        let (dir, log, sha) = repo_at_head("reuse-env");
        let g = greens::Greens::open(&store, "pj", &dir);
        let cold = Reuse { greens: &g, sha: &sha, env: greens::Env::Cold, who: "cold" };
        let warm = Reuse { greens: &g, sha: &sha, env: greens::Env::Warm, who: "warm" };
        run_all(&dir, &[counting(&dir, "a")], &log, Some(&cold), |_| {});
        run_all(&dir, &[counting(&dir, "a")], &log, Some(&warm), |_| {});
        assert_eq!(runs_of(&dir, "a"), 1, "the cold green answered the warm ask");
        run_all(&dir, &[counting(&dir, "b")], &log, Some(&warm), |_| {});
        run_all(&dir, &[counting(&dir, "b")], &log, Some(&cold), |_| {});
        assert_eq!(runs_of(&dir, "b"), 2, "the warm green did not answer the cold ask");
    }

    /// A trunk with a lockfile, an ignored built `target/`, and one commit.
    fn trunk_with_target(name: &str, built: bool) -> (PathBuf, PathBuf, String) {
        let (dir, tree, _) = {
            let (dir, log) = scratch_log(name);
            (dir.clone(), dir.with_file_name(format!("{}-tree", dir.file_name().unwrap().to_string_lossy())), log)
        };
        crate::testkit::repo(&dir);
        std::fs::write(dir.join("Cargo.lock"), "lock").unwrap();
        std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
        git_run(&dir, &["add", "Cargo.lock", ".gitignore"]);
        git_run(&dir, &["commit", "--quiet", "-m", "one"]);
        if built {
            std::fs::create_dir_all(dir.join("target/debug")).unwrap();
            std::fs::write(dir.join("target/debug/x"), "bin").unwrap();
        }
        let sha = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let _ = std::fs::remove_dir_all(&tree);
        (dir, tree, sha)
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_new_check_tree_starts_from_the_trunks_target_and_the_record_says_cloned() {
        let (repo, tree, sha) = trunk_with_target("seed", true);
        let first = checkout_started(&repo, &tree, &sha).unwrap();
        assert_eq!(first.target, "cloned", "{:?}", first.seed);
        assert_eq!(std::fs::read_to_string(tree.join("target/debug/x")).unwrap(), "bin");
        // The same tree at the next check began warm, and nothing was cloned again.
        let again = checkout_started(&repo, &tree, &sha).unwrap();
        assert_eq!((again.target, again.seed.is_empty()), ("warm", true));
        assert!(again.line().starts_with("target warm"), "{}", again.line());
    }

    #[test]
    fn a_new_check_tree_with_no_target_to_clone_says_cold_and_a_tree_with_no_cargo_says_none() {
        let (repo, tree, sha) = trunk_with_target("cold", false);
        let start = checkout_started(&repo, &tree, &sha).unwrap();
        assert_eq!(start.target, "cold", "{:?}", start.seed);
        assert!(!tree.join("target").exists());
        let json = start.json();
        assert_eq!((json["target"].as_str(), json["cores"].as_u64().is_some_and(|c| c >= 1)), (Some("cold"), true));
        let bare = scratch_log("bare").0;
        assert_eq!(Start::of(&bare, Vec::new()).target, "none");
    }

    #[test]
    fn a_tree_whose_target_was_removed_is_seeded_again() {
        let (repo, tree, sha) = trunk_with_target("reseed", true);
        checkout_started(&repo, &tree, &sha).unwrap();
        std::fs::remove_dir_all(tree.join("target")).unwrap();
        let start = checkout_started(&repo, &tree, &sha).unwrap();
        // macOS clones it; elsewhere the seed says why not and the tree is honestly cold.
        assert_eq!(start.target, if cfg!(target_os = "macos") { "cloned" } else { "cold" }, "{:?}", start.seed);
    }

    #[test]
    fn the_cold_build_still_starts_without_a_target() {
        let (repo, tree, sha) = trunk_with_target("coldbuild", true);
        checkout(&repo, &tree, &sha).unwrap();
        assert!(!tree.join("target").exists(), "only the row-check and barrier trees are seeded");
    }

    #[test]
    fn a_reused_step_shows_reused_on_the_entry_with_who_and_how_long() {
        let (_env, store, _) = world("reuse-account", "- x: true", "exit 0\n");
        let (dir, log, sha) = repo_at_head("reuse-account");
        let g = greens::Greens::open(&store, "pj", &dir);
        g.record(&sha, "true", greens::Env::Warm, 101, "wsp-9", false);
        let reuse = Reuse { greens: &g, sha: &sha, env: greens::Env::Warm, who: "x" };
        let ran = run_all(&dir, &[sh("true", 10)], &log, Some(&reuse), |_| {});
        let text = account(&ran);
        assert!(text.contains("reused (green at this commit, 101s, by wsp-9,"), "{text}");
        assert!(passed(&ran));
    }

    #[test]
    fn the_verb_reuses_a_green_at_the_landing_and_again_ignores_the_ledger() {
        let (env, store, sha) = world("verb-reuse", "- n: true", "exit 0\n");
        let counter = env.path("count");
        let cmd = format!("echo x >> {}", counter.display());
        let t = store.find_task("pj-001").unwrap();
        let mut t = t;
        t.body = format!("{}\n## Checks\n- n: {cmd}\n", t.body);
        store.save_task(&t).unwrap();
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[])), 0);
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[])), 0);
        let lines = |p: &Path| std::fs::read_to_string(p).map(|t| t.lines().count()).unwrap_or(0);
        assert_eq!(lines(&counter), 1, "the second `wsp check` was answered by the ledger");
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[("again", "true")])), 0);
        assert_eq!(lines(&counter), 2, "--again ignores it");
        let _ = sha;
    }

    #[test]
    fn an_end_step_parses_and_is_in_neither_a_rows_plan_nor_the_barriers_steps() {
        let s = step("- perf (20m) at end: cargo bench");
        assert!(s.end && !s.barrier && s.on.is_empty());
        assert_eq!(why_in(&s), "in the cold build");
        let (_env, store, _) = world("end", "- a: true\n- full at barrier: true\n- perf at end: true", "exit 0\n");
        assert_eq!(barrier_steps(&store, "pj").len(), 1);
        assert_eq!(end_steps(&store, "pj").len(), 1);
        let t = store.find_task("pj-001").unwrap();
        let tg = target(&store, &t, None).unwrap();
        let names: Vec<_> = plan(&store, &t, &tg, None).steps.into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["a"]);
    }

    #[test]
    fn the_barriers_steps_are_a_projects_and_not_a_rows() {
        let (_env, store, _) = world("barrier", "- a: true\n- full at barrier: true\n- full2 at barrier: true", "exit 0\n");
        assert_eq!(barrier_steps(&store, "pj").len(), 2);
        let code = check(&store, &Args::synth("check", &["pj"], &[("barrier", "true")]));
        assert_eq!(code, 0);
        assert!(v::runs(&store.find_task("pj-001").unwrap()).is_empty(), "the barrier hook writes nothing on a row");
    }

    #[test]
    fn the_verb_runs_at_the_landing_in_a_tree_of_its_own_and_records_it() {
        let (env, store, sha) = world("verb", "- file: test -f check.sh\n- script: sh check.sh", "exit 0\n");
        let code = check(&store, &Args::synth("check", &["pj-001"], &[]));
        assert_eq!(code, 0);
        let t = store.find_task("pj-001").unwrap();
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 1, "{}", t.body);
        assert_eq!(runs[0].state, RunState::Passed, "{}", runs[0].text);
        assert!(runs[0].text.contains("✓ file") && runs[0].text.contains("✓ script"), "{}", runs[0].text);
        let tree = env.state().join("checks/pj-repo/tree");
        assert!(tree.join("check.sh").exists(), "the warm tree is under the state directory");
        assert_eq!(git(&tree, &["rev-parse", "HEAD"]).unwrap().trim(), sha);
        assert_eq!(t.status(), Status::Review, "a pass moves nobody");
    }

    #[test]
    fn the_verb_at_a_named_commit_runs_and_writes_nothing_on_the_row() {
        let (_env, store, sha) = world("replay", "- script: sh check.sh", "exit 1\n");
        let before = store.find_task("pj-001").unwrap().body;
        let code = check(&store, &Args::synth("check", &["pj-001"], &[("at", &sha)]));
        assert_eq!(code, 1, "the failure is the exit status");
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.body, before);
        assert_eq!(t.status(), Status::Review, "a replay moves nobody");
    }

    #[test]
    fn the_verb_on_a_failing_landing_sends_the_row_back() {
        let (_env, store, _) = world("fails", "- script: sh check.sh", "echo broken >&2; exit 4\n");
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[])), 1);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.status(), Status::Doing);
        assert!(v::runs(&t)[0].text.contains("| broken"), "{}", v::runs(&t)[0].text);
    }

    #[test]
    fn set_writes_a_projects_checks_or_a_tasks_and_keeps_the_rest_of_the_body() {
        let (env, store, _) = world("set", "- old: true", "exit 0\n");
        let file = env.path("checks.md");
        std::fs::write(&file, "- build (5m): cargo build\n- tests: cargo test\n").unwrap();
        let args = Args::synth("check", &["pj"], &[("set", "true"), ("from", &file.display().to_string())]);
        assert_eq!(check(&store, &args), 0);
        let p = store.project("pj").unwrap();
        let names: Vec<_> = parse_steps(&p.section(SECTION).unwrap(), Of::Project).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["build", "tests"], "replaced, not appended");

        let args = Args::synth("check", &["pj-001"], &[("set", "true"), ("from", &file.display().to_string())]);
        assert_eq!(check(&store, &args), 0);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(task_steps(&t).len(), 2, "the task's two");
        assert!(t.section("Log").unwrap().contains(crate::repair::LANDED), "the log is untouched");
    }

    #[test]
    fn the_plan_names_the_steps_and_runs_none() {
        let (env, store, _) = world("plan", "- script: sh check.sh", "exit 0\n");
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[("plan", "true")])), 0);
        assert!(v::runs(&store.find_task("pj-001").unwrap()).is_empty());
        assert!(!env.state().join("checks").exists(), "no tree was made");
    }

    #[test]
    fn a_moved_trunk_keeps_only_the_steps_its_paths_can_affect() {
        let rule = |glob: &str| Step { of: Of::Rule, on: vec![glob.into()], ..sh("true", 1) };
        let member = Step { of: Of::Task, ..sh("true", 1) };
        let always = Step { of: Of::Project, ..sh("true", 1) };
        let steps = vec![rule("src/**"), rule("docs/**"), member.clone(), always.clone()];
        let paths = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let table: [(&[&str], &[&str], usize); 5] = [
            // pulled in, ours, which of the four survive (as a bitmask over the list)
            (&["src/a.rs"], &["m.txt"], 0b1001),
            (&["docs/x.md"], &["m.txt"], 0b0101),
            (&["src/a.rs"], &["src/a.rs"], 0b1011),
            (&["other"], &["m.txt"], 0b0001),
            (&[], &["m.txt"], 0b0000),
        ];
        for (pulled, ours, want) in table {
            let got = affected(&steps, &paths(pulled), &paths(ours));
            let mask = steps.iter().enumerate().filter(|(_, s)| got.contains(s)).fold(0, |m, (i, _)| m | (1 << (3 - i)));
            assert_eq!(mask, want, "pulled {pulled:?}, ours {ours:?}");
        }
    }

    #[test]
    fn a_plan_names_each_steps_tier_and_marks_the_one_already_green_at_the_commit() {
        let (_env, store, sha) = world(
            "plan-green",
            "- mod on src/*.rs: cargo test {1}::\n- fast on src/*.rs: true\n- all at barrier: cargo test\n- gold at end: gold.sh",
            "exit 0\n",
        );
        let steps = parse_steps("- mod on src/*.rs: cargo test {1}::\n- fast on src/*.rs: true\n- all at barrier: cargo test\n- gold at end: gold.sh", Of::Project);
        let tiers: Vec<_> = steps.iter().map(tier).collect();
        assert_eq!(tiers, ["inner", "row", "barrier", "end"]);
        let t = store.find_task("pj-001").unwrap();
        let tg = target(&store, &t, None).unwrap();
        let ledger = greens::Greens::open(&store, &tg.project, &tg.repo);
        assert!(!plan_line(&steps[1], ledger.lookup(&sha, "true", greens::Env::Warm).as_ref()).contains("reused"));
        ledger.record(&sha, "true", greens::Env::Warm, 101, "pj-001", false);
        let line = plan_line(&steps[1], ledger.lookup(&sha, "true", greens::Env::Warm).as_ref());
        assert!(line.contains("row") && line.ends_with("green · reused, 101s"), "{line}");
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[("plan", "true"), ("json", "true")])), 0);
    }

    #[test]
    fn inner_steps_follow_the_changed_paths_narrowest_first_and_never_include_barrier_or_end() {
        let (_env, store, _sha) = world(
            "inner",
            "- always: true\n- mod on src/*.rs: cargo test {1}::\n- docs on docs/*: echo docs\n- all at barrier: cargo test\n- gold at end: gold.sh",
            "exit 0\n",
        );
        let mut t = store.find_task("pj-001").unwrap();
        crate::model::set_section_in(&mut t.body, SECTION, "- mine: sh check.sh");
        let names = |changed: &[&str]| {
            let c: Vec<String> = changed.iter().map(|s| s.to_string()).collect();
            inner_steps(&store, &t, &c).into_iter().map(|s| s.name).collect::<Vec<_>>()
        };
        assert_eq!(names(&["src/greens.rs"]), ["mod:greens", "mine", "always"]);
        assert_eq!(names(&["docs/a.md", "src/a.rs"]), ["mod:a", "docs", "mine", "always"]);
        assert_eq!(names(&[]), ["mine", "always"]);
    }

    #[test]
    fn inner_run_reads_the_trees_changes_and_records_nothing() {
        let (env, store, _sha) = world("inner-run", "- mark on src/*.rs: touch ran-{1}", "exit 0\n");
        let tree = env.path("repo");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(tree.join("src/new.rs"), "x").unwrap();
        let before = store.find_task("pj-001").unwrap().body;
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tree).unwrap();
        let code = check(&store, &Args::synth("check", &["pj-001"], &[("inner", "true"), ("run", "true")]));
        std::env::set_current_dir(prev).unwrap();
        assert_eq!(code, 0);
        assert!(tree.join("ran-new").exists(), "an untracked path selected its rule");
        assert_eq!(store.find_task("pj-001").unwrap().body, before, "nothing on the row");
        assert!(!greens::Greens::open(&store, "pj", &tree).lookup(&"0".repeat(40), "touch ran-new", greens::Env::Warm).is_some());
    }

    #[test]
    fn tiers_lists_each_tier_and_set_warns_on_end_without_barrier() {
        let (env, store, _sha) = world("tiers", "- mod on src/*.rs: cargo test {1}::\n- all at barrier: cargo test\n- gold at end: gold.sh", "exit 0\n");
        assert_eq!(check(&store, &Args::synth("check", &["pj"], &[("tiers", "true")])), 0);
        assert_eq!(check(&store, &Args::synth("check", &["nope"], &[("tiers", "true")])), 1);
        let file = env.path("end-only.md");
        std::fs::write(&file, "- gold at end: sh gold.sh\n").unwrap();
        assert_eq!(check(&store, &Args::synth("check", &["pj"], &[("set", "true"), ("from", &file.display().to_string())])), 0);
    }

    #[test]
    fn the_usage_line_names_inner_and_tiers() {
        assert!(USAGE.contains("--inner [<task>] [--run]") && USAGE.contains("--affected [<task>] [--run]") && USAGE.contains("--tiers"));
    }
}
