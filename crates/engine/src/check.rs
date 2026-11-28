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
//! The commands run with every `WSP_*` and `backend` variable removed. A test
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

pub use crate::model::project_of;
pub use crate::util::captures;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cmd_verify::{git, git_ok};
use crate::greens::{self, Green, Reuse};
use crate::model::{Status, Task};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::verification::{self as v, Run, RunState};
use crate::base::args::Args;
use crate::host::{JobClass, JobEnded, JobKey, JobOutput, JobRow, JobRun, JobState};

/// The heading a project or a task lists its checks under.
pub const SECTION: &str = "Checks";

const DEFAULT_LIMIT: Duration = Duration::from_secs(600);

/// How long a run waits for the tree before giving it up for the next trigger.
pub const TREE_WAIT: Duration = Duration::from_secs(45 * 60);

/// The longest a tail of output is kept on the row, in lines and in characters.
const TAIL_LINES: usize = 25;
const TAIL_COLS: usize = 200;

/// Where a step came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Of {
    /// The member's own `## Checks`.
    Task,
    /// A project rule a changed path matched.
    Rule,
    /// A project rule with no paths: every landing.
    Project,
}

/// One declared check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
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
pub fn parse_steps(text: &str, of: Of) -> Vec<Step> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("- ").or_else(|| l.strip_prefix("* "))
        })
        .filter_map(|item| step_of(item, of))
        .collect()
}

/// What stands before the `: ` of a step.
pub struct Head {
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
pub fn step_of(item: &str, of: Of) -> Option<Step> {
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
pub fn unparsable(command: &str) -> Option<String> {
    if let Some(label) = label_left_in(command) {
        return Some(format!(
            "`{label}:` reads as a label but is not one — write `label (10m): command` (a limit in s, m or h) or `label: command`"
        ));
    }
    wsp_base::base::shell::parse_error(command)
}

/// Each step of `steps` that cannot start, with why: `name — reason`.
pub fn malformed_in(steps: &[Step]) -> Vec<String> {
    steps.iter().filter_map(|s| unparsable(&s.command).map(|why| format!("{} — {why}", s.name))).collect()
}

/// Each step of `steps` whose command runs `wsp check`, `wsp review` or `wsp
/// submit` on row `id` — by its id, or with no row named at all, which is the
/// row the check runs in. Such a line calls the very check that is running it:
/// `wsp check <id>` from inside a check of `<id>` is refused as already
/// running and exits 1 every time, so the line can never pass (compounding-520,
/// wsp-638). `name — reason`, like [`malformed_in`].
pub fn calls_itself_in(id: &str, steps: &[Step]) -> Vec<String> {
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
pub fn unrunnable(command: &str) -> Option<String> {
    let first = command
        .split_whitespace()
        .find(|w| !(w.contains('=') && w.split('=').next().is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_'))))?;
    let first = first.trim_start_matches(['(', '{', '!']);
    if first.is_empty() || first.contains('/') || first.starts_with(['$', '`', '"', '\'', '[']) {
        return None;
    }
    (!wsp_base::base::shell::knows(first)).then(|| format!("`{first}` is not a command here (not a builtin, a path, or on PATH)"))
}

/// What a line is refused for where it is written: it cannot start
/// ([`malformed_in`]), or its first word is no program ([`unrunnable`]).
pub fn refused_in(steps: &[Step]) -> Vec<String> {
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
pub fn line_of(item: &str) -> Option<(Head, &str)> {
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
pub fn timed_line(item: &str) -> Result<(String, String, String), String> {
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

pub fn limit_of(s: &str) -> Option<Duration> {
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
pub fn task_steps(t: &Task) -> Vec<Step> {
    t.section(SECTION).map(|s| parse_steps(&s, Of::Task)).unwrap_or_default()
}

/// The steps a project holds back for its barrier, whatever the row.
pub fn barrier_steps(store: &Store, project: &str) -> Vec<Step> {
    store
        .project(project)
        .and_then(|p| p.section(SECTION))
        .map(|s| parse_steps(&s, Of::Project).into_iter().filter(|s| s.barrier).collect())
        .unwrap_or_default()
}

/// The steps a project runs only in the cold build, at the end of a worklist.
pub fn end_steps(store: &Store, project: &str) -> Vec<Step> {
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
pub fn artefact_globs(store: &Store, project: &str) -> Vec<String> {
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
pub struct FullSuite {
    pub commands: Vec<String>,
    pub members_allowed: bool,
}

impl FullSuite {
    /// Whether `command` is one of the project's full-suite commands.
    pub fn names(&self, command: &str) -> bool {
        let c = command.trim();
        self.commands.iter().any(|f| f == c)
    }

    /// Whether a member may run `command`: it is no full-suite command, or the
    /// project allows members the full suite.
    pub fn member_may_run(&self, command: &str) -> bool {
        !self.names(command) || self.members_allowed
    }
}

pub fn full_suite(store: &Store, project: &str) -> FullSuite {
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
pub struct Plan {
    pub steps: Vec<Step>,
    pub changed: Vec<String>,
    /// One line per step the change let [`crate::affected::narrow_steps`]
    /// narrow or leave to the barrier, saying why.
    pub narrowed: Vec<String>,
}

/// The member's steps, then the project's rules the changed paths call for,
/// then its every-landing steps; a command named twice runs once, and a barrier
/// step is not a row's.
pub fn plan(store: &Store, t: &Task, tg: &Target, at: Option<&str>) -> Plan {
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
    // A broad `cargo test` step is narrowed by what changed (`wsp-433`), so
    // the paths are read for one too.
    let narrowable = |c: &str| c.trim_start().starts_with("cargo test") || c.trim_start().starts_with("cargo nextest run");
    let wants_paths = wants_paths || steps.iter().chain(rules.iter()).any(|s| narrowable(&s.command));
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
    let in_tests = |word: &str| git(&tg.repo, &["grep", "-q", "-F", "-e", word, &tg.sha, "--", "tests"]).is_some();
    let (steps, narrowed) = crate::affected::narrow_steps(steps, &changed, &at_sha, &in_tests);
    Plan { steps, changed, narrowed }
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
pub fn layering_step(has: &dyn Fn(&str) -> bool, in_base: &dyn Fn(&str) -> bool, changed: &[String]) -> Option<Step> {
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
pub const HELP_FIXTURE: &str = "fixtures/help.txt";

/// The test that compares the help to [`HELP_FIXTURE`].
pub const HELP_TEST: &str = "the_help_is_what_it_was_before_the_registry";

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
pub fn help_step(read: &dyn Fn(&str) -> Option<String>, changed: &[String]) -> Option<Step> {
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
pub const LAYERING_FILE: &str = "tests/layering.rs";
pub const LAYERING_COMMAND: &str = "cargo test --test layering";
pub const READONLY_FILE: &str = "src/readonly.rs";
pub const READONLY_COMMAND: &str = "cargo test readonly::tests";

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
pub fn structure_steps(read: &dyn Fn(&str) -> Option<String>, changed: &[String]) -> Vec<Step> {
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

// ---- the rule steps at review (wsp-858) -------------------------------------
//
// wsp-844 and wsp-345 each reached their verifier with the layering test red
// and were sent back for it, 8-13 minutes of a 33 minute list. The rule steps
// below are the ones a row's own diff calls for and no `## Checks` line names,
// so `wsp review` runs them in the member's seat, at the commit it is putting
// up, and a red one keeps the row from review and from its verifier.

/// What a rule step said in the member's seat at review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtReview {
    Green,
    /// Red: the tail of its output.
    Red(String),
    /// Not run, or not run to its end: why, for the review's output.
    Skipped(String),
}

/// The most a rule step may take in a seat at review. The layering test is a
/// second on a warm `target/`; past this the build is the cost, and the
/// verifier's and the barrier's runs are left to find it.
pub const REVIEW_RULE_LIMIT: Duration = Duration::from_secs(4 * 60);

/// The rule-derived steps (help, layering, readonly) the diff from `base` to
/// `sha` in `repo` calls for: the same [`structure_steps`] and
/// [`layering_step`] a landing's check plans, and no rule of its own.
pub fn rule_steps_at(repo: &Path, base: &str, sha: &str) -> Vec<Step> {
    let mut changed: Vec<String> = git(repo, &["diff", "--name-only", base, sha])
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    changed.sort();
    changed.dedup();
    let at_sha = |path: &str| git(repo, &["show", &format!("{sha}:{path}")]);
    let mut steps = structure_steps(&at_sha, &changed);
    let in_base = |path: &str| git(repo, &["cat-file", "-e", &format!("{base}:{path}")]).is_some();
    if let Some(l) = layering_step(&|p| at_sha(p).is_some(), &in_base, &changed) {
        if !steps.iter().any(|s| s.command == l.command) {
            steps.push(l);
        }
    }
    steps
}

#[cfg(test)]
thread_local! {
    /// A test's own command for every rule step at review, and its limit: the
    /// real ones are `cargo test`, which a unit test does not build.
    pub static TEST_RULE_RUN: std::cell::RefCell<Option<(String, Duration)>> = const { std::cell::RefCell::new(None) };
}

/// Run one rule step in the member's `tree`, its output to `log`: skipped on a
/// cold `target/` (a build of minutes in the seat), and stopped at
/// [`REVIEW_RULE_LIMIT`] and said as skipped, never as red.
pub fn run_at_review(tree: &Path, step: &Step, log: &Path) -> AtReview {
    if Start::of(tree, Vec::new()).target == "cold" {
        return AtReview::Skipped(format!("{} not run in the seat: the tree's target/ is cold, a build of minutes — the verifier's check runs it", step.name));
    }
    #[allow(unused_mut)]
    let mut step = Step { limit: REVIEW_RULE_LIMIT, ..step.clone() };
    #[cfg(test)]
    if let Some((command, limit)) = TEST_RULE_RUN.with(|r| r.borrow().clone()) {
        step.command = command;
        step.limit = limit;
    }
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let ran = run_step(tree, &step, log, &Runner::Inproc);
    match &ran.done {
        Done::Passed => AtReview::Green,
        Done::Failed { timed_out: true, .. } => AtReview::Skipped(format!(
            "{} not finished in the seat: stopped at its {}s limit at review — the verifier's check runs it",
            step.name,
            step.limit.as_secs()
        )),
        d if d.is_red() => AtReview::Red(ran.tail.clone()),
        _ => AtReview::Skipped(format!("{} not run in the seat: {}", step.name, why_failed(&ran))),
    }
}

/// The steps of a re-plan after the trunk moved that the rebase can have
/// affected, given `pulled_in` (what the rebase brought from the trunk) and
/// `ours` (what the branch changes): a project rule whose glob matches a pulled-in
/// path; a member's own step when the two sets overlap; an every-landing step
/// when anything was pulled in. A rule expanded from a capture is kept by its
/// glob, so a sibling capture may be rerun too — more checks, never fewer.
pub fn affected(steps: &[Step], pulled_in: &[String], ours: &[String]) -> Vec<Step> {
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
pub fn changed_paths(tg: &Target, t: &Task, use_since: bool) -> Vec<String> {
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
pub struct Target {
    /// A base named on the command line for reading which paths changed.
    pub since: Option<String>,
    pub sha: String,
    pub repo: PathBuf,
    pub project: String,
}

/// The landing of `t` — or `at`, when a commit is named — in the first of its
/// project's roots that has the commit.
pub fn target(store: &Store, t: &Task, at: Option<&str>) -> Result<Target, String> {
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
    let row_tree = at.and_then(|_| crate::cmd_checkout::member_tree(store, &t.id));
    for root in &roots {
        let Some(repo) = crate::cmd_verify::toplevel(&util::expand(root)) else { continue };
        let full = match at {
            Some(_) => commit_named(&repo, row_tree.as_deref(), &sha),
            None => commit_in(&repo, &sha),
        };
        if let Some(sha) = full {
            return Ok(Target { sha, repo, project, since: None });
        }
    }
    Err(format!("{sha} is in none of {project}'s roots ({})", roots.join(", ")))
}

fn commit_in(dir: &Path, rev: &str) -> Option<String> {
    git(dir, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).map(|s| s.trim().to_string())
}

/// The commit the caller means by `rev`, in the repository at `repo`. `HEAD`
/// and its like are each tree's own, and the root's is the trunk, so `rev` is
/// read in the tree the verb is run from when that is a tree of `repo`, else in
/// the row's own tree, else in `repo`.
fn commit_named(repo: &Path, row_tree: Option<&Path>, rev: &str) -> Option<String> {
    let common = |dir: &Path| git(dir, &["rev-parse", "--git-common-dir"]).and_then(|c| dir.join(c.trim()).canonicalize().ok());
    let ours = common(repo)?;
    let here = std::env::current_dir().ok();
    let tree = here.as_deref().into_iter().chain(row_tree).find(|d| common(d).as_ref() == Some(&ours));
    commit_in(tree.unwrap_or(repo), rev)
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

pub fn dir_of(store: &Store, tg: &Target) -> PathBuf {
    let repo = tg.repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    store.state_file("checks").join(format!("{}-{repo}", tg.project))
}

/// The barrier's own warm tree and lock for a repository, apart from the
/// per-row `checks/` trees so a row's check never moves the barrier's `target/`.
pub fn barrier_dir(store: &Store, project: &str, repo: &Path) -> PathBuf {
    let repo = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    store.state_file("barrier").join(format!("{project}-{repo}"))
}

/// Where the barrier tree stands: its path and the commit it is at, if it exists.
pub fn barrier_tree_line(store: &Store, project: &str, repo: &Path) -> String {
    let tree = barrier_dir(store, project, repo).join("tree");
    match git(&tree, &["rev-parse", "HEAD"]) {
        Some(head) if tree.join(".git").exists() => format!("barrier tree {} at {}", util::contract(&tree), &head.trim()[..head.trim().len().min(12)]),
        _ => format!("barrier tree {} not made yet", util::contract(&tree)),
    }
}

/// Move the warm tree to `sha`, making it on the first use.
pub fn checkout(repo: &Path, tree: &Path, sha: &str) -> Result<(), String> {
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
pub struct Start {
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
    pub fn of(tree: &Path, seed: Vec<String>) -> Start {
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

    /// Install, by `runner` in `tree`, each `node_modules/` the tree still
    /// lacks after seeding or holds older than its lockfile, and say how each
    /// went on [`Start::seed`] (`compound-react-045`). Seeding clones only on
    /// macOS and only from a trunk whose lockfile matches; without this a
    /// barrier or verifier tree ran `npm run check` to "Cannot find package
    /// 'vite'", a red of the tree's that sent rows back for nothing they did.
    /// Each install is `npm ci`, a job like any step, its output in
    /// `install.log` beside `log`; one that fails is said, and the steps that
    /// need it are left to fail as they will.
    pub fn install(&mut self, tree: &Path, log: &Path, runner: &Runner<'_>) -> Vec<String> {
        #[cfg(test)]
        if let Some(npm) = TEST_NPM.with(|n| n.borrow().clone()) {
            return self.install_by(tree, log, runner, &npm);
        }
        self.install_by(tree, log, runner, "npm")
    }

    /// [`Start::install`] with `npm` the program run; a test's is a script.
    fn install_by(&mut self, tree: &Path, log: &Path, runner: &Runner<'_>, npm: &str) -> Vec<String> {
        let mut wanted: Vec<PathBuf> = crate::warm::missing(tree).into_iter().filter(|d| d.ends_with("node_modules")).collect();
        wanted.extend(crate::warm::stale(tree));
        wanted.sort();
        wanted.dedup();
        let log = log.with_file_name("install.log");
        let said: Vec<String> = wanted.iter().map(|rel| {
            let at = rel.parent().filter(|p| !p.as_os_str().is_empty());
            let command = match at {
                Some(at) => format!("cd '{}' && {npm} ci", at.display().to_string().replace('\'', "'\\''")),
                None => format!("{npm} ci"),
            };
            let step = Step { name: format!("install {}", rel.display()), command, limit: INSTALL_LIMIT, of: Of::Project, on: Vec::new(), barrier: false, end: false };
            let ran = run_step(tree, &step, &log, runner);
            match ran.done {
                Done::Passed => {
                    crate::warm::stamp(tree, rel);
                    format!("{} installed by `npm ci` in {}s", rel.display(), secs(ran.took))
                }
                _ => format!("{} not installed: `npm ci` {} — a step that needs it is red for the tree, not the row (log {})", rel.display(), why_failed(&ran), util::contract(&log)),
            }
        }).collect();
        self.seed.extend(said.iter().cloned());
        said
    }

    /// `target cold, load 3.2 of 8 cores`, for a line of the log.
    pub fn line(&self) -> String {
        let load = self.load.map(|l| format!(", load {l:.1} of {} cores", self.cores)).unwrap_or_default();
        format!("target {}{load}", self.target)
    }

    /// The same as the figures an event carries.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "target": self.target, "load": self.load, "cores": self.cores, "seed": self.seed })
    }
}

#[cfg(test)]
thread_local! {
    /// The `npm` a test's [`Start::install`] runs, on the thread that calls it.
    static TEST_NPM: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The longest one `npm ci` in a check tree may take.
const INSTALL_LIMIT: Duration = Duration::from_secs(10 * 60);

/// [`checkout`], then — for a tree that is new or has lost its `target/` —
/// seed it from the trunk's by clone, as a row's tree is (`wsp-566`), and say
/// how it stands. Only the row-check and barrier trees come through here: the
/// cold build is cold by design and calls [`checkout`] alone.
pub fn checkout_started(repo: &Path, tree: &Path, sha: &str) -> Result<Start, String> {
    let made = !tree.join(".git").exists();
    checkout(repo, tree, sha)?;
    // Any dependency directory the tree lacks, not only cargo's: a web
    // project's barrier tree has a `target/` from its first build and no
    // `node_modules/` ever after (`compound-react-045`).
    let seed = if made || !tree.join("target").exists() || !crate::warm::missing(tree).is_empty() {
        let lines = crate::cmd_checkout::trunk(repo).filter(|t| t != tree).map(|t| crate::warm::clone_from(&t, tree)).unwrap_or_default();
        // A clone came from a trunk whose lockfile matched: it answers for this one.
        for line in &lines {
            if let Some(rel) = line.split_once(" cloned from the parent tree").map(|(rel, _)| rel).filter(|r| r.ends_with("node_modules")) {
                crate::warm::stamp(tree, Path::new(rel));
            }
        }
        lines
    } else {
        Vec::new()
    };
    Ok(Start::of(tree, seed))
}

/// One check at a time in a tree: a file beside it holding the pid that has it
/// (who it is, never asked), and under `WSP_JOBS=manager` the job running in
/// the tree. The file is a lease (wsp-057): its holder beats it while it
/// holds it, so a holder that is killed leaves a lock that ages out, and no
/// process is probed.
pub struct TreeLock {
    path: PathBuf,
    beat: Option<crate::lease::Beat>,
}

/// How long a lock that is not touched, and whose job is not running, stands
/// before it is stale. Its holder's beat touches it four times a grace, and a
/// run touches it as each job is sent and ends; a run that sends no job for
/// longer (the wsp-067 wait for a missing tree) touches it as it waits
/// ([`Runner::keep_held`]), so a live run's lock never ages past it.
const HOLD_GRACE: Duration = crate::lease::GRACE;

/// [`HOLD_GRACE`], or a test's own.
fn hold_grace() -> Duration {
    #[cfg(test)]
    if let Some(d) = GRACE.with(|g| g.get()) {
        return d;
    }
    HOLD_GRACE
}

#[cfg(test)]
thread_local! {
    /// A test's own grace for a lock whose job is not running, kept to its thread.
    static GRACE: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

impl TreeLock {
    pub fn take(path: PathBuf, wait: Duration) -> Result<TreeLock, String> {
        Self::take_saying(path, wait, |_| {})
    }

    /// [`take`](Self::take), saying once — to `said`, with who has it and since
    /// when — if the lock is held, so a run that waits behind another names it.
    pub fn take_saying(path: PathBuf, wait: Duration, mut said: impl FnMut(Option<(u32, String)>)) -> Result<TreeLock, String> {
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
                    let beat = crate::lease::Beat::start(path.clone(), crate::lease::every(hold_grace()), Self::ours);
                    return Ok(TreeLock { path, beat: Some(beat) });
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
    pub fn holder(path: &Path) -> Option<(u32, String)> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut it = text.split_whitespace();
        let pid = it.next()?.parse().ok()?;
        Some((pid, it.next().unwrap_or_default().to_string()))
    }

    /// Whether the lock at `path` is held by a holder that is still there.
    pub fn live(path: &Path) -> bool {
        Self::holder(path).is_some() && !Self::dead(path)
    }

    /// Whether the lock's holder is gone, which is the only reason to break
    /// one. No process is probed: a lock that names a job (`WSP_JOBS=manager`)
    /// is read by key from the job table of its tree's host's manager, and any
    /// lock is its lease, beaten by its holder.
    fn dead(path: &Path) -> bool {
        Self::dead_with(path, hold_grace(), jobs_of)
    }

    /// [`TreeLock::dead`] with the job table and the grace given. A lock is
    /// stale when it has not been touched for `grace` and names no job
    /// running in its host's table (ended, or forgotten by a manager that
    /// restarted). A lock naming a job whose table could not be read is kept,
    /// since a second check in a tree still in use is the worse mistake. A
    /// lock being written this instant, or garbage, ages out the same way.
    fn dead_with(path: &Path, grace: Duration, table: impl Fn(&str) -> Option<Vec<JobRow>>) -> bool {
        if crate::lease::fresh(path, grace) {
            return false;
        }
        let Some((host, key)) = Self::job_of(path) else { return true };
        let Some(rows) = table(&host) else { return false };
        !rows.iter().any(|r| key_text(&r.key) == key && r.state == JobState::Running)
    }

    /// The job a lock names, as `(host, row/attempt/job)`: the third word of
    /// its line, `job:<host>:<key>`.
    fn job_of(path: &Path) -> Option<(String, String)> {
        let text = std::fs::read_to_string(path).ok()?;
        let named = text.split_whitespace().nth(2)?.strip_prefix("job:")?;
        let (host, key) = named.split_once(':')?;
        Some((host.to_string(), key.to_string()))
    }

    /// Names job `key` on `host` as the holder of the lock at `path`, keeping
    /// its pid and its since, and touches it. Written beside and renamed over,
    /// so a reader never sees half a line.
    /// A lock that is not this process's (broken and taken by another run, or
    /// gone) is left as it is: a holder that lost its lock never writes over
    /// the next holder's.
    pub fn name_job(path: &Path, host: &str, key: &JobKey) {
        let Some((pid, since)) = Self::holder(path).filter(|(pid, _)| *pid == std::process::id()) else { return };
        let next = path.with_extension(format!("next-{pid}"));
        if std::fs::write(&next, format!("{pid} {since} job:{host}:{}\n", key_text(key))).is_ok() {
            let _ = std::fs::rename(&next, path);
        }
    }

    /// Touches the lock at `path`, while it is still this process's: a run
    /// that holds a tree and sends no job keeps its lock from aging out.
    pub fn touch(path: &Path) {
        if Self::ours(path) {
            crate::lease::touch(path);
        }
    }

    /// Whether the lock at `path` names this process as its holder.
    fn ours(path: &Path) -> bool {
        Self::holder(path).is_some_and(|(pid, _)| pid == std::process::id())
    }
}

impl Drop for TreeLock {
    /// Removes the lock only while it is still this process's: one that was
    /// broken and taken by another run is that run's to remove.
    fn drop(&mut self) {
        drop(self.beat.take());
        if Self::ours(&self.path) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

// ---- where a step runs (M3 under T2, wsp-053) -------------------------------

/// Where a job runs: [`place_of`] and its types live in `wsp-trees`
/// (`crates/trees/src/job_place.rs`), so `wsp verify` there takes its place from
/// the same function.
pub use wsp_trees::job_place::{place_of, Place, TreeKind, Whose};

/// What runs a step, always as one `job.run` through
/// [`crate::host::Host::run_job`] (wsp-057: wsp spawns no step itself):
/// `wsp-host`'s job runner in this process (`WSP_JOBS=inproc`, and a host
/// with no manager), or a host's manager (`WSP_JOBS=manager`). The barrier
/// chooses its own in its row.
pub enum Runner<'a> {
    /// In this process, the tree taken where it is ([`in_process`]).
    Inproc,
    Jobs(Jobs<'a>),
}

/// A run's steps as jobs: the host whose manager runs them, the place they
/// run in, and the tree lock the run holds, which names the job running.
pub struct Jobs<'a> {
    pub host: &'a dyn crate::host::Host,
    pub place: Place,
    pub lock: Option<PathBuf>,
    /// What makes this run's keys its own (wsp-040): a job key is unique per run.
    run: String,
    seq: std::sync::atomic::AtomicU32,
}

impl<'a> Jobs<'a> {
    pub fn new(host: &'a dyn crate::host::Host, place: Place, lock: Option<PathBuf>) -> Jobs<'a> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        Jobs { host, place, lock, run: format!("{}.{nanos}", std::process::id()), seq: std::sync::atomic::AtomicU32::new(0) }
    }

    /// `job.run` for `step` in `dir`: class `check`, at the place, `sh -c` its
    /// command with wsp's own environment taken off on the argv (the rule of
    /// `seat.spawn`) save `$WSP_SCRATCH`, set to [`scratch_of`] `dir`, bounded
    /// by its limit and [`hung_after`]. The command is [`runner_script`]'s
    /// where it wraps one, which may send a job of its own first
    /// ([`Jobs::metadata`]).
    pub fn run_of(&self, step: &Step, dir: &Path) -> JobRun {
        // The opt-out is a shell comment, and the wrapped script goes on after
        // the command, so it is taken off first.
        let script = runner_script(without_opt_out(&step.command), |cd| self.metadata(step, dir, cd));
        self.sh_job(step, dir, script.unwrap_or_else(|| step.command.clone()))
    }

    /// What `cargo metadata` says in the step's tree, read as a job of the
    /// step's at its place, so on the host and with the cargo the step runs
    /// with (wsp spawns none itself); `None` where that job does not pass.
    fn metadata(&self, step: &Step, dir: &Path, cd: Option<&str>) -> Option<String> {
        let cd = cd.map(|d| format!("cd {d} && ")).unwrap_or_default();
        let step = Step { name: format!("{}-metadata", step.name), ..step.clone() };
        let run = self.sh_job(&step, dir, format!("{cd}cargo metadata --no-deps --format-version 1 2>/dev/null"));
        let mut said = String::new();
        self.hold(&run.key);
        let ended = self.host.run_job(&run, &mut |o: &JobOutput| said.push_str(&o.text));
        self.hold(&run.key);
        (ended.ok()?.exit == Some(0)).then_some(said)
    }

    fn sh_job(&self, step: &Step, dir: &Path, script: String) -> JobRun {
        let n = self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let name: String = step.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(40).collect();
        let job = format!("{}:{name}@{}.{n}", self.place.kind.word(), self.run);
        let mut argv = vec!["env".to_string()];
        for (k, _) in std::env::vars() {
            if k.starts_with("WSP_") || k == "GIT_INDEX_FILE" {
                argv.extend(["-u".to_string(), k]);
            }
        }
        argv.push("--".to_string());
        argv.push(format!("WSP_SCRATCH={}", scratch_of(dir).display()));
        argv.extend(["sh", "-c"].map(String::from));
        argv.push(script);
        JobRun {
            key: crate::host::job_key(&self.place.row, 0, &job),
            class: JobClass::Check,
            argv,
            at: self.place.at.clone(),
            timeout_secs: step.limit.as_secs().max(1),
            hung_after_secs: Some(hung_after().as_secs().max(1)),
            detach: false,
        }
    }

    /// The lock names `key` as the job holding the tree, and is touched: once
    /// as the job is sent and once as it ends, so a lock whose job ended long
    /// ago is stale and one between two steps is not.
    fn hold(&self, key: &JobKey) {
        if let Some(lock) = &self.lock {
            TreeLock::name_job(lock, self.place.host(), key);
        }
    }
}

impl Runner<'_> {
    /// Keeps the run's tree lock held through a stretch in which it sends no
    /// job (the wsp-067 wait for a missing tree): touched, so it does not age
    /// past the grace and read free while the run is alive. The lock's own
    /// beat does the same; this is the run saying so where it knows the lock.
    pub fn keep_held(&self) {
        if let Runner::Jobs(Jobs { lock: Some(lock), .. }) = self {
            TreeLock::touch(lock);
        }
    }
}

impl Runner<'static> {
    /// What `WSP_JOBS` chooses for a run in `whose`'s `kind` tree at `tree`,
    /// holding `lock`: `inproc` runs in this process, each step a job on
    /// `wsp-host`'s runner with the tree where it is ([`in_process`]), and
    /// places nothing;
    /// `manager` takes the place from [`place_of`] and runs on the manager of
    /// the place's host. A place on a host with no manager (only this
    /// machine has one until M4), and a tree outside the tree root (wsp-361),
    /// keep the in-process path. A manager that
    /// cannot be reached is an error, and nothing is run here in its place.
    pub fn chosen(store: &Store, whose: Whose<'_>, kind: TreeKind, tree: &Path, lock: Option<&Path>) -> Result<Runner<'static>, String> {
        use crate::host::JobsChoice;
        if JobsChoice::from_env().map_err(|e| e.to_string())? == JobsChoice::Inproc {
            return Ok(Runner::Inproc);
        }
        // Whether this host has a manager is asked before anything is placed:
        // a tree outside the tree root is an error only for a manager's job
        // (wsp-346), and a host with none runs here wherever the tree is.
        let managed = crate::host_tcp::record(&store.state).ok().map(|(local, _)| local.host);
        if managed.as_deref() != Some(wsp_trees::job_place::host_of(whose).as_str()) {
            return Ok(Runner::Inproc);
        }
        // A tree outside the tree root (the shared check and barrier trees,
        // under the state directory) runs here too, as on a host with no
        // manager (wsp-361): the manager's jobs stay under its root, and a
        // run it cannot take is not refused for that.
        if wsp_trees::job_place::outside_tree_root(tree) {
            return Ok(Runner::Inproc);
        }
        let place = place_of(whose, kind, tree)?;
        let host = crate::host::choose_jobs(JobsChoice::Manager, &store.state, env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
        Ok(Runner::Jobs(Jobs::new(host, place, lock.map(Path::to_path_buf))))
    }
}

/// The job table of `host`'s manager, for a lock or a run named by a job:
/// `None` where it could not be asked (no manager there, or one that did not
/// answer), which keeps what it would have freed.
pub fn jobs_of(host: &str) -> Option<Vec<JobRow>> {
    #[cfg(test)]
    if let Some(table) = JOB_TABLE.with(|t| t.borrow().clone()) {
        return table;
    }
    let state = std::env::var_os("WSP_STATE").map(PathBuf::from).unwrap_or_else(Store::default_state);
    let (local, _) = crate::host_tcp::record(&state).ok()?;
    if local.host != host {
        return None;
    }
    let manager = crate::host::choose_jobs(crate::host::JobsChoice::Manager, &state, env!("CARGO_PKG_VERSION")).ok()?;
    manager.jobs().ok()
}

#[cfg(test)]
thread_local! {
    /// A test's own job table, kept to its thread: `Some(None)` is a manager
    /// that could not be asked.
    static JOB_TABLE: std::cell::RefCell<Option<Option<Vec<JobRow>>>> = const { std::cell::RefCell::new(None) };
}

/// Whether job `key` on `host` is running (`Some(true)`), ended or not in its
/// manager's table (`Some(false)`), or could not be asked (`None`). The
/// store's `suite:running` start reads this for a run named by its job
/// (`checks::Run::gone_by`); the barrier's run is named by its job in the
/// barrier row, which this one leaves its runner to.
#[allow(dead_code)]
pub fn job_runs(host: &str, key: &str) -> Option<bool> {
    let table = jobs_of(host)?;
    Some(table.iter().any(|r| key_text(&r.key) == key && r.state == JobState::Running))
}

/// A job key as a lock or a store entry writes it: `row/attempt/job`, with no
/// spaces (a row id and a check's job name have none).
pub fn key_text(key: &JobKey) -> String {
    format!("{}/{}/{}", key.row.0, key.attempt, key.job)
}

// ---- running ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    Passed,
    Failed { exit: Option<i32>, timed_out: bool },
    /// The command could not start — it does not parse, or names nothing to run.
    /// The declaration is wrong; nothing the work did.
    Malformed(String),
    /// The step wrote nothing and used no CPU for [`hung_after`], and was ended
    /// there rather than left to its limit (`wsp-609`): the process tree it had,
    /// one `pid command` per line.
    Hung(String),
    /// A test runner ran no test at all ([`ran_no_tests`], `wsp-969`): the
    /// step checked nothing, whatever it exited with. The reason, naming what
    /// the command selected.
    NoTests(String),
    /// The tree the step was to run in was not there (`wsp-067`): the volume
    /// went away or the tree was removed under the run. An environment fault, as
    /// "limit too tight" is, and not a failed step: the row is not sent back for
    /// it. The path that was missing.
    TreeMissing(String),
    /// An earlier step failed, so this one was not run.
    Skipped,
    /// Red, and every failure in its output is a known trunk red
    /// ([`TrunkRed`], compound-react-127): the red and the row that owns it.
    /// Reads as a pass, and is recorded as no green.
    HeldOver(String),
}

impl Done {
    /// Anything that stops a run: a red step or one that could not start.
    pub fn is_red(&self) -> bool {
        matches!(self, Done::Failed { .. } | Done::Malformed(_) | Done::Hung(_) | Done::NoTests(_))
    }

    /// Passed, or red on nothing but a known trunk red.
    pub fn is_green(&self) -> bool {
        matches!(self, Done::Passed | Done::HeldOver(_))
    }

    /// Anything after which the next step is not run: a red step, or a tree
    /// that is not there for it either.
    pub fn stops(&self) -> bool {
        self.is_red() || matches!(self, Done::TreeMissing(_) | Done::Skipped)
    }
}

/// How long [`run_again`] waits for a missing tree to come back before it
/// leaves the step at "tree missing": a minute, or `WSP_CHECK_TREE_BACK_SECS`.
pub fn tree_back_after() -> Duration {
    #[cfg(test)]
    if let Some(d) = TREE_BACK.with(|h| h.get()) {
        return d;
    }
    let secs = std::env::var("WSP_CHECK_TREE_BACK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    Duration::from_secs(secs)
}

#[cfg(test)]
thread_local! {
    /// A test's own wait for a missing tree, kept to its thread.
    static TREE_BACK: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

/// The step as "tree missing: <dir>", when `dir` is not a directory.
fn tree_missing(dir: &Path, step: &Step, start: Instant) -> Option<Ran> {
    if dir.is_dir() {
        return None;
    }
    let path = util::contract(dir);
    let tail = format!("tree missing: {path} — the step could not run there; nothing it did failed");
    Some(Ran { step: step.clone(), done: Done::TreeMissing(path), took: start.elapsed(), tail, first: None, reused: None, build: None, via: None })
}

/// How long a step may sit with no output and no CPU before it is called hung:
/// five minutes, or `WSP_CHECK_HUNG_SECS`.
pub fn hung_after() -> Duration {
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

#[derive(Clone, Debug)]
pub struct Ran {
    pub step: Step,
    pub done: Done,
    pub took: Duration,
    pub tail: String,
    /// Why the first run failed and its tail, when the step passed on a second:
    /// a flake is passed and kept in view, not swallowed.
    pub first: Option<(String, String)>,
    /// The green this step was answered by, when it did not run.
    pub reused: Option<Green>,
    /// The share of `took` spent in a cold build (`wsp-364`), when the step's
    /// first run was stopped by its limit while cargo was still building: that
    /// first run, and the second's build as cargo reports it. What is left,
    /// [`run_took`], is the step's own run, and that is what its limit judges.
    pub build: Option<Duration>,
    /// The runner a wrapped `cargo test` step ran by (wsp-527): `cargo nextest
    /// run` or `cargo test`, from its job's first line ([`via_of`]).
    pub via: Option<&'static str>,
}

/// A step's own run: its time less any cold build ([`Ran::build`]).
pub fn run_took(r: &Ran) -> Duration {
    r.took.saturating_sub(r.build.unwrap_or_default())
}

/// The most a cold build may add to a step's limit on its second run: a check
/// tree that starts with no `target/` compiles the workspace inside the first
/// step, and a limit of 1-5 s declared for the run was judged against that
/// compile (`wsp-364`, three m3 rows sent back for nothing they did). The
/// manager's hang bound still ends a build that stops making progress.
const COLD_BUILD_LIMIT: Duration = Duration::from_secs(20 * 60);

/// Whether a step's log ends with cargo still building: a line of its build
/// (`Compiling`, `Blocking waiting for file lock`, …) and no `Finished` line,
/// so a stop at the limit stopped the build, not the step's run.
fn still_building(log: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(log) else { return false };
    let words = |l: &str| l.trim_start().split_whitespace().next().map(str::to_string).unwrap_or_default();
    let building = text.lines().any(|l| {
        let w = words(l);
        matches!(w.as_str(), "Compiling" | "Checking" | "Blocking" | "Downloading" | "Downloaded" | "Updating" | "Locking" | "Building")
    });
    building && !text.lines().any(|l| words(l) == "Finished")
}

/// What cargo's `Finished … in 9.03s` (or `in 1m 05s`) line in a step's log
/// says its build took.
fn built_in(log: &Path) -> Option<Duration> {
    let text = std::fs::read_to_string(log).ok()?;
    let line = text.lines().find(|l| l.trim_start().starts_with("Finished "))?;
    let (_, after) = line.rsplit_once(" in ")?;
    let mut total = 0.0;
    for part in after.split_whitespace() {
        let (n, unit) = part.split_at(part.find(|c: char| !(c.is_ascii_digit() || c == '.'))?);
        let n: f64 = n.parse().ok()?;
        total += match unit {
            "s" => n,
            "m" => n * 60.0,
            "h" => n * 3600.0,
            _ => return None,
        };
    }
    Some(Duration::from_secs_f64(total))
}

/// `$WSP_SCRATCH` for a step run in `dir` (`compound-react-045`): the seat's
/// own where this process has one, else `<dir>.scratch` beside the tree — the
/// check and barrier trees' `tree.scratch`, kept with their warm `target/`. A
/// check line that names `$WSP_SCRATCH/target` passes in the seat, where it is
/// set; a step's job took every `WSP_` variable off, so in a verifier's or the
/// barrier's run it read `/target` and failed for the tree.
pub fn scratch_of(dir: &Path) -> PathBuf {
    scratch_in(std::env::var_os("WSP_SCRATCH"), dir)
}

fn scratch_in(own: Option<std::ffi::OsString>, dir: &Path) -> PathBuf {
    if let Some(own) = own.filter(|s| !s.is_empty()) {
        return PathBuf::from(own);
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "tree".into());
    dir.parent().unwrap_or(dir).join(format!("{name}.scratch"))
}

// ---- one process per test (wsp-527) -----------------------------------------
//
// Every test that touches a store holds `util::ENV`, one lock per process, so
// libtest's threads queue on it: `cycle::` took 11.85 s on 32 threads and on
// one. `cargo nextest run` gives each test its own process, and the lock is
// then each test's own. A plain `cargo test` step is run through nextest where
// the host that runs it has it, and through `cargo test` as written where it
// does not. nextest runs no doctests, so a step that would is followed by
// `cargo test --doc` for the packages that have them, read by `cargo metadata`
// in a job of its own. The job says which on its first line and the step's line repeats
// it. The step's command, and so its green, is the one written: a green from
// one runner counts for the other at the same inputs.

/// The first line a step's job writes when nextest ran it.
pub const BY_NEXTEST: &str = "wsp: run by cargo nextest run (one process per test)";
/// The first line when nextest ran it and `cargo test --doc` its doctests.
pub const BY_NEXTEST_AND_DOC: &str = "wsp: run by cargo nextest run (one process per test), then cargo test --doc";
/// The first line a step's job writes when nextest is not on its host.
pub const BY_CARGO_TEST: &str = "wsp: run by cargo test (no cargo-nextest on this host)";
/// The first line when the step runs as written for want of its doctest
/// libraries.
pub const BY_CARGO_TEST_UNREAD: &str = "wsp: run by cargo test (cargo metadata did not say which libraries have doctests)";

/// The first line when the step runs as written for a glob in its `--exclude`:
/// the doctest packages are left out by exact name.
pub const BY_CARGO_TEST_GLOB: &str = "wsp: run by cargo test (a glob --exclude is not read for nextest)";

/// A `--exclude` value cargo reads as a pattern.
fn is_glob(spec: &str) -> bool {
    spec.contains(['*', '?', '['])
}

/// Whether `command` is a `cargo test` (a leading `cd <dir> &&` kept) with a
/// glob `--exclude`, quoted or not.
fn excludes_by_glob(command: &str) -> bool {
    let words: Vec<&str> = split_cd(command).1.split_whitespace().collect();
    let ["cargo", "test", args @ ..] = words.as_slice() else { return false };
    let args: Vec<&str> = args.iter().copied().take_while(|w| *w != "--").collect();
    args.windows(2).any(|w| w[0] == "--exclude" && is_glob(w[1])) || args.iter().any(|w| w.strip_prefix("--exclude=").is_some_and(is_glob))
}

/// Flags without a value that mean the same to both.
const SAME: &[&str] = &["--no-fail-fast", "--lib", "--bins", "--tests", "--workspace", "--all", "--all-features", "--no-default-features", "-r", "--release"];

/// Flags with a value, as `cargo test` and then nextest spell them: nextest's
/// own `--profile` is its run profile.
const VALUED: &[(&str, &str)] = &[
    ("-p", "-p"), ("--package", "--package"), ("--exclude", "--exclude"), ("--test", "--test"),
    ("-F", "-F"), ("--features", "--features"), ("--profile", "--cargo-profile"),
];

/// The target selections among them: under one, `cargo test` runs no doctests.
const NARROWS: &[&str] = &["--lib", "--bins", "--tests", "--test"];

/// Arguments after `--` that nextest takes as libtest does.
const NEXTEST_AFTER_DASHES: &[&str] = &["--exact", "--ignored", "--include-ignored"];

/// A `cargo test` as nextest reads it ([`by_nextest`]).
#[derive(Debug, PartialEq)]
pub struct ByNextest {
    /// The `cargo nextest run`, without any `cd`.
    pub run: String,
    /// The step's leading `cd <dir>`.
    pub dir: Option<String>,
    /// The doctests the command would also have run: `None` under a target
    /// selection.
    pub doc: Option<Doc>,
}

/// What a `cargo test --doc` following nextest has to carry.
#[derive(Debug, Default, PartialEq)]
pub struct Doc {
    pub workspace: bool,
    pub packages: Vec<String>,
    pub exclude: Vec<String>,
    /// The features, profile, filter and arguments after `--`, as written.
    pub rest: Vec<String>,
}

/// `command` read for `cargo nextest run` with the same package, targets and
/// filters, where it is one plain `cargo test` (a leading `cd <dir> &&`
/// kept) whose every argument is one known to read alike ([`SAME`],
/// [`VALUED`]); `None` otherwise, and for a glob `--exclude`. The run ignores
/// the project's nextest `default-filter`, which `cargo test` does not read.
pub fn by_nextest(command: &str) -> Option<ByNextest> {
    let (dir, rest) = split_cd(command);
    let rest = rest.trim();
    if rest.contains('\n') || rest.chars().any(|ch| matches!(ch, ';' | '|' | '&' | '>' | '<' | '`' | '$' | '(' | '\'' | '"' | '\\')) {
        return None;
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    let ["cargo", "test", args @ ..] = words.as_slice() else { return None };
    // A filter matching nothing exits 0, as under `cargo test`, so the doctests
    // still follow; [`ran_no_tests`] judges the run from its output.
    let mut out: Vec<String> = ["cargo", "nextest", "run", "--no-tests=pass", "--ignore-default-filter"].map(String::from).to_vec();
    let mut doc = Doc::default();
    let (mut narrowed, mut filters, mut after_dashes) = (false, 0, false);
    let mut args = args.iter().copied();
    while let Some(w) = args.next() {
        if after_dashes {
            let mut took = vec![w.to_string()];
            if w == "--skip" {
                took.push(args.next()?.to_string());
            } else if w.starts_with('-') && !NEXTEST_AFTER_DASHES.contains(&w) {
                return None;
            }
            out.extend(took.clone());
            doc.rest.extend(took);
            continue;
        }
        if w == "--" || !w.starts_with('-') {
            after_dashes = w == "--";
            filters += usize::from(!after_dashes);
            out.push(w.to_string());
            doc.rest.push(w.to_string());
            continue;
        }
        let (flag, value) = match w.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (w, None),
        };
        narrowed |= NARROWS.contains(&flag);
        if value.is_none() && SAME.contains(&flag) {
            out.push(w.to_string());
            match flag {
                "--workspace" | "--all" => doc.workspace = true,
                _ => doc.rest.push(w.to_string()),
            }
            continue;
        }
        let (_, to) = VALUED.iter().find(|(from, _)| *from == flag)?;
        let value = match value {
            Some(v) => v,
            None => args.next().filter(|v| !v.starts_with('-'))?,
        };
        if flag == "--exclude" && is_glob(value) {
            return None;
        }
        out.extend([to.to_string(), value.to_string()]);
        match flag {
            "-p" | "--package" => doc.packages.push(value.to_string()),
            "--exclude" => doc.exclude.push(value.to_string()),
            _ => doc.rest.extend([flag.to_string(), value.to_string()]),
        }
    }
    // `cargo test` takes one filter before `--`; two are its error to report.
    if filters > 1 {
        return None;
    }
    Some(ByNextest { run: out.join(" "), dir: dir.map(String::from), doc: (!narrowed).then_some(doc) })
}

/// The packages `doc` selects whose library has doctests on, read from what
/// `cargo metadata` said: an explicit `--doc` runs the blocks of a
/// `doctest = false` library and fails where there is no library, so only
/// these are named to it. `None` where that cannot be told.
fn doctested(metadata: &str, doc: &Doc) -> Option<Vec<String>> {
    let meta: serde_json::Value = serde_json::from_str(metadata).ok()?;
    let packages = meta["packages"].as_array()?;
    let ids = meta[if doc.workspace { "workspace_members" } else { "workspace_default_members" }].as_array()?;
    let name_of = |p: &serde_json::Value| p["name"].as_str().map(String::from);
    // A spec that is no member's name (`name@version`) is not read here.
    let named = |n: &String| packages.iter().find(|p| p["name"].as_str() == Some(n));
    if !doc.exclude.iter().all(|n| named(n).is_some()) {
        return None;
    }
    let selected: Vec<&serde_json::Value> = if doc.packages.is_empty() {
        packages.iter().filter(|p| ids.contains(&p["id"])).collect()
    } else {
        doc.packages.iter().map(named).collect::<Option<_>>()?
    };
    Some(
        selected
            .into_iter()
            .filter(|p| p["targets"].as_array().is_some_and(|t| t.iter().any(|t| t["doctest"].as_bool() == Some(true))))
            .filter_map(name_of)
            .filter(|n| !doc.exclude.contains(n))
            .collect(),
    )
}

/// What a step's job runs in `sh -c` for `command`: [`by_nextest`]'s form
/// (and the doctests after it) where `cargo nextest` answers on the host,
/// `command` as written where it does not, each after its line ([`BY_NEXTEST`],
/// [`BY_NEXTEST_AND_DOC`], [`BY_CARGO_TEST`]). `metadata` gives what `cargo
/// metadata` says under the command's `cd`, and is asked only where doctests
/// would follow; where it cannot tell, `command` runs as written
/// ([`BY_CARGO_TEST_UNREAD`]). A glob `--exclude` runs as written too
/// ([`BY_CARGO_TEST_GLOB`]). `None` for any other [`by_nextest`] leaves alone.
pub fn runner_script(command: &str, metadata: impl FnOnce(Option<&str>) -> Option<String>) -> Option<String> {
    if excludes_by_glob(command) {
        return Some(format!("echo '{BY_CARGO_TEST_GLOB}'; {}", command.trim()));
    }
    let ByNextest { run, dir, doc } = by_nextest(command)?;
    let (mut run, mut line) = (run, BY_NEXTEST);
    if let Some(doc) = doc {
        let Some(packages) = metadata(dir.as_deref()).and_then(|m| doctested(&m, &doc)) else {
            return Some(format!("echo '{BY_CARGO_TEST_UNREAD}'; {}", command.trim()));
        };
        if !packages.is_empty() {
            let named: String = packages.iter().map(|p| format!(" -p {p}")).collect();
            let rest: String = doc.rest.iter().map(|w| format!(" {w}")).collect();
            run = if run.contains(" --no-fail-fast") {
                // The doctests run whatever nextest said, so none is hidden.
                format!("{{ {run}; a=$?; cargo test --doc{named}{rest} && [ $a -eq 0 ]; }}")
            } else {
                format!("{run} && cargo test --doc{named}{rest}")
            };
            line = BY_NEXTEST_AND_DOC;
        }
    }
    if let Some(d) = dir {
        run = format!("cd {d} && {run}");
    }
    Some(format!(
        "if cargo nextest --version >/dev/null 2>&1; then echo '{line}'; {run}; else echo '{BY_CARGO_TEST}'; {}; fi",
        command.trim()
    ))
}

/// Which runner a step's log says ran it: `cargo nextest run`, `cargo test`,
/// or `None` for a step [`runner_script`] did not wrap.
pub fn via_of(log: &Path) -> Option<&'static str> {
    use std::io::{BufRead, BufReader};
    let first = BufReader::new(std::fs::File::open(log).ok()?).lines().next()?.ok()?;
    match first.trim() {
        BY_NEXTEST => Some("cargo nextest run"),
        BY_NEXTEST_AND_DOC => Some("cargo nextest run + cargo test --doc"),
        BY_CARGO_TEST | BY_CARGO_TEST_UNREAD | BY_CARGO_TEST_GLOB => Some("cargo test"),
        _ => None,
    }
}

/// Run one step in `dir`, its output to `log`, by `runner`: as one `job.run`
/// on `wsp-host`'s runner in this process, or on a host's manager.
pub fn run_step(dir: &Path, step: &Step, log: &Path, runner: &Runner<'_>) -> Ran {
    match runner {
        Runner::Inproc => run_as_job(dir, step, log, &in_process(dir)),
        Runner::Jobs(jobs) => run_as_job(dir, step, log, jobs),
    }
}

/// The jobs of a step run in this process (`WSP_JOBS=inproc`, and a host with
/// no manager): `wsp-host`'s runner, its files under `<state>/inproc` as
/// `choose_jobs(Inproc)`'s are, and the tree placed as its own root, its
/// parent the root and its name the path. Rule (e), a place relative to the
/// tree root, is for a job that crosses to a manager; in this process there is
/// no host boundary, so a tree outside the root runs where it is (wsp-057's
/// d1, after wsp-346). Its lock is held by this process's pid and names no
/// job.
fn in_process(dir: &Path) -> Jobs<'static> {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};
    static HOSTS: OnceLock<Mutex<BTreeMap<(PathBuf, PathBuf), &'static crate::host::InProcess>>> = OnceLock::new();
    let (root, place) = wsp_trees::job_place::own_root(dir);
    let exec = inproc_exec();
    let host = *HOSTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry((exec.clone(), root.clone()))
        .or_insert_with(|| Box::leak(Box::new(crate::host::InProcess::new().with_jobs(&exec, &root))));
    Jobs::new(host, place, None)
}

/// Where [`in_process`]'s runner keeps its jobs' files: `<state>/inproc`, as
/// `choose_jobs(Inproc)` does. A test's are its process's own, since the tests
/// that run beside it move `WSP_STATE` and remove their states.
pub fn inproc_exec() -> PathBuf {
    #[cfg(test)]
    return std::env::temp_dir().join(format!("wsp-inproc-jobs-{}", std::process::id()));
    #[cfg(not(test))]
    std::env::var_os("WSP_STATE").map(PathBuf::from).unwrap_or_else(Store::default_state).join("inproc")
}

/// [`run_step`] as one job: `job.run` of class `check`
/// at the place [`place_of`] gave, its limit the step's and its hung bound
/// [`hung_after`]. What it writes comes back as `job.output` into `log`, which
/// the tail and the first error are read from; its timeout and its hang come
/// from `job.ended`. The manager admits it (no check slot is taken here) and
/// ends its group; wsp probes no process for it.
fn run_as_job(dir: &Path, step: &Step, log: &Path, jobs: &Jobs<'_>) -> Ran {
    use std::io::Write;
    let start = Instant::now();
    let failed = |why: String| Ran {
        step: step.clone(),
        done: Done::Failed { exit: None, timed_out: false },
        took: start.elapsed(),
        tail: why,
        first: None,
        reused: None,
        build: None,
        via: None,
    };
    if let Some(why) = unparsable(&step.command) {
        return Ran { done: Done::Malformed(why.clone()), tail: why, ..failed(String::new()) };
    }
    if let Some(missing) = tree_missing(dir, step, start) {
        return missing;
    }
    let Ok(mut out) = std::fs::File::create(log) else { return failed(format!("could not write {}", util::contract(log))) };
    let _ = std::fs::create_dir_all(scratch_of(dir));
    let run = jobs.run_of(step, dir);
    jobs.hold(&run.key);
    let ended = jobs.host.run_job(&run, &mut |o: &JobOutput| {
        let _ = out.write_all(o.text.as_bytes());
    });
    jobs.hold(&run.key);
    drop(out);
    match ended {
        Ok(end) => ran_of_job(dir, step, log, &end, start),
        Err(e) => {
            if let Some(missing) = tree_missing(dir, step, start) {
                return missing;
            }
            failed(format!("the step was not run as a job on {}: {e}", jobs.place.host()))
        }
    }
}

/// What a step's `job.ended` reads as: the same pass, fail, timeout and hang
/// a step run here reads as.
fn ran_of_job(dir: &Path, step: &Step, log: &Path, end: &JobEnded, start: Instant) -> Ran {
    let via = via_of(log);
    let ran = |done: Done, tail: String| Ran { step: step.clone(), done, took: start.elapsed(), tail, first: None, reused: None, build: None, via };
    if end.exit != Some(0) {
        if let Some(missing) = tree_missing(dir, step, start) {
            return missing;
        }
    }
    if end.hung {
        let what = format!("job {} (its group ended by the host's manager)", key_text(&end.key));
        let tail = format!("hung — no output and no CPU for {}s; {what}", secs(hung_after()));
        return ran(Done::Hung(what), tail);
    }
    let mut tail = tail_of(log);
    if end.timed_out {
        return ran(Done::Failed { exit: None, timed_out: true }, tail);
    }
    if !may_run_no_tests(step) {
        if let Some(runner) = ran_no_tests(log, end.exit == Some(0)) {
            let why = no_tests_why(runner, &step.command);
            tail = format!("{tail}\n{why}");
            return ran(Done::NoTests(why), tail);
        }
    }
    match end.exit {
        Some(0) => ran(Done::Passed, tail),
        Some(code) => match failed_to_start(Some(code), &tail) {
            Some(why) => ran(Done::Malformed(why), tail),
            None => ran(Done::Failed { exit: Some(code), timed_out: false }, tail),
        },
        None => {
            tail = format!("{tail}\nended by a signal (not by its limit)");
            ran(Done::Failed { exit: None, timed_out: false }, tail)
        }
    }
}

/// The comment that ends a step's command when running no test is what it is
/// for: `label (5m): cargo test --features x # may-run-0-tests`.
pub const MAY_RUN_NO_TESTS: &str = "# may-run-0-tests";

/// `command` without its [`MAY_RUN_NO_TESTS`].
fn without_opt_out(command: &str) -> &str {
    command.trim_end().strip_suffix(MAY_RUN_NO_TESTS).map_or(command, str::trim_end)
}

/// Whether `step` may run no test: it says so, or it is one wsp derived from
/// the changed paths ([`crate::affected::derived`], the only rule step with no
/// `on`), which names no filter to be stale and which nobody can opt out: a
/// crate with no tests, or a source no Vitest file reaches.
fn may_run_no_tests(step: &Step) -> bool {
    without_opt_out(&step.command) != step.command || (step.of == Of::Rule && step.on.is_empty())
}

/// The test runner whose output in `log` says it ran no test at all, summed
/// over every binary and run of that runner: libtest's `running N tests` and
/// nextest's `N tests run` together, Vitest's `Tests` line and its "No test
/// files found", Playwright's `Running N tests`. A command that failed
/// (`passed` false) is read this way only where the runner failed for want of
/// tests; any other failure stays its own.
fn ran_no_tests(log: &Path, passed: bool) -> Option<&'static str> {
    use std::io::{BufRead, BufReader};
    const CARGO: usize = 0;
    const VITEST: usize = 1;
    const PLAYWRIGHT: usize = 2;
    let mut ran: [Option<u64>; 3] = [None; 3];
    let mut refused = [false; 3];
    let mut add = |runner: usize, n: u64| ran[runner] = Some(ran[runner].unwrap_or(0) + n);
    for line in BufReader::new(std::fs::File::open(log).ok()?).split(b'\n').map_while(Result::ok) {
        let line = plain_text(&String::from_utf8_lossy(&line));
        let line = line.trim();
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["running", n, "test" | "tests"] => n.parse().into_iter().for_each(|n| add(CARGO, n)),
            ["Running", n, "test" | "tests", "using", ..] => n.parse().into_iter().for_each(|n| add(PLAYWRIGHT, n)),
            ["Summary", ..] if line.contains(" run: ") => {
                let count = line.split_once("] ").and_then(|(_, r)| r.split_whitespace().next());
                count.and_then(|n| n.split('/').next()?.parse().ok()).into_iter().for_each(|n| add(CARGO, n));
            }
            _ => {}
        }
        // Vitest's summary line has two spaces after `Tests`.
        if let Some(counts) = line.strip_prefix("Tests  ") {
            let words: Vec<&str> = counts.split_whitespace().collect();
            let n = words.windows(2).filter(|w| w[1] == "passed" || w[1] == "failed").filter_map(|w| w[0].parse::<u64>().ok()).sum();
            add(VITEST, n);
        }
        if line.starts_with("No test files found") {
            add(VITEST, 0);
            refused[VITEST] = true;
        }
        if line.starts_with("Error: No tests found") || line.starts_with("Error: no tests found") {
            add(PLAYWRIGHT, 0);
            refused[PLAYWRIGHT] = true;
        }
        if line == "error: no tests to run" {
            add(CARGO, 0);
            refused[CARGO] = true;
        }
    }
    ["cargo test", "vitest", "playwright"]
        .into_iter()
        .enumerate()
        .find(|&(i, _)| ran[i] == Some(0) && (passed || refused[i]))
        .map(|(_, name)| name)
}

/// A line without its terminal colour codes.
fn plain_text(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            chars.by_ref().find(|c| c.is_ascii_alphabetic());
        } else {
            out.push(c);
        }
    }
    out
}

/// Why a step that ran no test failed, naming the filter: what its command
/// gives the runner.
fn no_tests_why(runner: &str, command: &str) -> String {
    let filter = ["cargo nextest run", "cargo test", "vitest run", "vitest", "playwright test"]
        .iter()
        .find_map(|r| command.split_once(r))
        .map(|(_, rest)| rest.split(['&', ';', '|', '\n']).next().unwrap_or_default().trim())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| command.trim());
    format!(
        "{runner} ran 0 tests: the filter `{filter}` matches no test — fix the line (a module may have moved to another crate), \
         or end it with `{MAY_RUN_NO_TESTS}` if running none is meant"
    )
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
pub fn run_all(dir: &Path, steps: &[Step], log: &Path, reuse: Option<&Reuse>, runner: &Runner<'_>, mut each: impl FnMut(&Ran)) -> Vec<Ran> {
    let mut out: Vec<Ran> = Vec::new();
    for s in steps {
        let stop = out.last().is_some_and(|r| r.done.stops());
        let ran = if stop {
            Ran { step: s.clone(), done: Done::Skipped, took: Duration::ZERO, tail: String::new(), first: None, reused: None, build: None, via: None }
        } else {
            run_again(dir, s, log, reuse, runner)
        };
        each(&ran);
        out.push(ran);
    }
    out
}

/// Run every step whatever the one before did. A barrier wants all of its
/// answers — a red suite does not excuse a member's own check from being read —
/// where a row's run stops at the first. Its `runner` is the caller's: the
/// barrier chooses its own in its row.
pub fn run_every(dir: &Path, steps: &[Step], log: &Path, reuse: Option<&Reuse>, runner: &Runner<'_>, mut each: impl FnMut(&Ran)) -> Vec<Ran> {
    steps
        .iter()
        .map(|s| {
            let ran = run_again(dir, s, log, reuse, runner);
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
/// A step that timed out is run once more with its limit doubled: a measured
/// time at its declared limit is a budget set too tight, not a failure of the
/// code. A pass there is "limit too tight", and the row is not sent back; a
/// step that also takes twice its limit really hangs and fails. One stopped
/// while cargo was still building (a cold check tree, `wsp-364`) is run once
/// more with room for the build, and its limit judges the run after cargo's
/// `Finished` line: a cold compile is the tree's, not the step's.
///
/// With a `reuse`, a step already green at that commit is not run (it costs 0s
/// and is not written again), and a pass is recorded when the tree was clean at
/// that commit before the step ran.
pub fn run_again(dir: &Path, step: &Step, log: &Path, reuse: Option<&Reuse>, runner: &Runner<'_>) -> Ran {
    run_again_as(dir, step, step, log, reuse, runner)
}

/// [`run_again`] for `step`, running `as_run`'s command: the ledger, the entry
/// and what the check wrote are the declared step's.
fn run_again_as(dir: &Path, step: &Step, as_run: &Step, log: &Path, reuse: Option<&Reuse>, runner: &Runner<'_>) -> Ran {
    let inputs = reuse.and_then(|r| crate::affected::key(dir, r.sha, &step.command));
    if let Some(r) = reuse {
        if let Some(green) = r.greens.lookup_by(r.sha, inputs.as_deref(), &step.command, r.env) {
            return Ran { step: step.clone(), done: Done::Passed, took: Duration::ZERO, tail: String::new(), first: None, reused: Some(green), build: None, via: None };
        }
    }
    let clean = reuse.is_some_and(|r| greens::clean_at(dir, r.sha));
    // What the step creates in the tree is noted as the check's (`wsp-848`).
    let before = crate::cmd_checkout::untracked_paths(dir);
    let mut first = run_step(dir, as_run, log, runner);
    // `wsp-067`: a missing tree is waited for, and the step run once more when
    // it is back; still gone, the step stays "tree missing".
    if matches!(first.done, Done::TreeMissing(_)) {
        let waited = Instant::now();
        while !dir.is_dir() && waited.elapsed() < tree_back_after() {
            runner.keep_held();
            std::thread::sleep(Duration::from_millis(200).min(tree_back_after()));
        }
        if !dir.is_dir() {
            return Ran { took: first.took + waited.elapsed(), ..first };
        }
        first = run_step(dir, as_run, log, runner);
    }
    let mut ran = if matches!(first.done, Done::Failed { timed_out: false, .. }) {
        let second = run_step(dir, as_run, log, runner);
        Ran { took: first.took + second.took, first: Some((why_failed(&first), first.tail.clone())), ..second }
    } else if matches!(first.done, Done::Failed { timed_out: true, .. }) && still_building(log) {
        // `wsp-364`: stopped in a cold build. The second run has room for the
        // build, and what is judged is its run once cargo said it had built.
        let why = format!("stopped at its {}s limit while cargo was still building (a cold `target/`)", step.limit.as_secs());
        let wide = Step { limit: step.limit * 2 + COLD_BUILD_LIMIT, ..as_run.clone() };
        let second = run_step(dir, &wide, log, runner);
        let build = built_in(log).map(|b| first.took + b.min(second.took));
        let mut ran = Ran { step: step.clone(), took: first.took + second.took, first: Some((why, first.tail.clone())), build, ..second };
        if ran.done == Done::Passed && ran.build.is_some() && run_took(&ran) > step.limit * 2 {
            ran.done = Done::Failed { exit: None, timed_out: true };
        }
        ran
    } else if matches!(first.done, Done::Failed { timed_out: true, .. }) {
        let wide = Step { limit: step.limit * 2, ..as_run.clone() };
        let second = run_step(dir, &wide, log, runner);
        Ran { step: step.clone(), took: first.took + second.took, first: Some((why_failed(&first), first.tail.clone())), ..second }
    } else {
        first
    };
    ran.step = Step { limit: ran.step.limit, ..step.clone() };
    crate::cmd_checkout::note_check_wrote(dir, &step.name, before.as_ref());
    if let (Some(r), true, Done::Passed) = (reuse, clean, &ran.done) {
        r.greens.record_by(r.sha, inputs.as_deref(), &step.command, r.env, secs(ran.took), r.who, ran.first.is_some());
    }
    ran
}

// ---- a trunk red that has its own row (compound-react-127) -------------------
//
// A row says it owns a red the trunk has with a line in its log, `wsp note
// <row> "trunk red: <test>"`. While that row is open, another row's check that
// fails on nothing but the named tests passes over them, and the first check
// that names one pulls the row into the running group
// ([`crate::cycle::pull_trunk_red`]). Decision d1, 2026-11-23.

/// What opens the log line a row declares its trunk red with.
pub const TRUNK_RED: &str = "trunk red:";

/// A red the trunk has, and the open row that owns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrunkRed {
    pub row: String,
    /// The test, or its `file:line`, as the runner's output writes it.
    pub name: String,
}

/// The names `t`'s log declares as its trunk reds.
fn declared_reds(t: &Task) -> Vec<String> {
    t.section("Log")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (_, said) = l.trim().strip_prefix("- ")?.split_once(' ')?;
            let name = said.strip_prefix(TRUNK_RED)?;
            // A person's note is signed `· <engineer>` at its end.
            let name = name.split(" · ").next().unwrap_or(name).trim().trim_matches('`');
            (name.len() >= 3).then(|| name.to_string())
        })
        .collect()
}

/// The trunk reds `own`'s check may hold over: those declared by another row of
/// its project that has not landed. Never one `own` declares itself.
pub fn known_reds(store: &Store, own: &Task) -> Vec<TrunkRed> {
    let mine = declared_reds(own);
    store
        .tasks_shared()
        .iter()
        .filter(|t| t.id != own.id && t.project == own.project && t.status() != Status::Done)
        .filter(|t| crate::repair::landed_since_sent_back(t).is_none())
        .flat_map(|t| declared_reds(t).into_iter().map(|name| TrunkRed { row: t.id.clone(), name }))
        .filter(|r| !mine.contains(&r.name))
        .collect()
}

/// Whether `line` names `red` as a whole word, so `a::b` is not `a::bc`.
fn names_red(line: &str, red: &str) -> bool {
    let part = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(red).any(|(i, _)| !line[..i].chars().next_back().is_some_and(part) && !line[i + red.len()..].chars().next().is_some_and(part))
}

/// The tests a runner's output says failed, and whether those are all that did.
struct Failures {
    tests: Vec<String>,
    /// False where something could be red and unnamed: a build error, a crash,
    /// a run cut short, or a summary counting more failures than are named.
    whole: bool,
}

/// Read libtest's, nextest's, Vitest's and Playwright's output for what failed.
fn failures_in(output: &str) -> Failures {
    const NEXTEST: &[&str] = &["FAIL", "TIMEOUT", "ABORT", "SIGSEGV", "SIGABRT", "SIGKILL", "LEAK-FAIL"];
    let (mut libtest, mut nextest, mut vitest, mut playwright) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut libtest_said, mut nextest_said, mut vitest_said, mut playwright_said) = (0, 0, 0, 0);
    let (mut bad_binaries, mut reruns, mut whole) = (0, 0, true);
    let mut in_playwright = false;
    // The number before `word` in a summary.
    let count = |line: &str, word: &str| -> usize {
        let words: Vec<&str> = line.split_whitespace().collect();
        words.windows(2).filter(|w| w[1].trim_end_matches([',', ';']) == word).filter_map(|w| w[0].parse::<usize>().ok()).sum()
    };
    for line in output.lines() {
        let line = plain_text(line);
        let line = line.trim();
        let words: Vec<&str> = line.split_whitespace().collect();
        if in_playwright {
            if line.contains(" › ") {
                playwright.push(line.to_string());
                continue;
            }
            in_playwright = false;
        }
        match words.as_slice() {
            [n, "failed"] => {
                if let Ok(n) = n.parse::<usize>() {
                    playwright_said += n;
                    in_playwright = true;
                }
            }
            [_, "interrupted"] | [_, "did", "not", "run"] => whole = false,
            [first, second, ..] if NEXTEST.contains(first) && second.starts_with('[') => {
                if let Some((_, rest)) = line.split_once("] ") {
                    let rest = rest.trim_start();
                    let rest = rest.strip_prefix('(').and_then(|r| r.split_once(") ")).map_or(rest, |(_, r)| r);
                    let test = rest.trim().to_string();
                    if !nextest.contains(&test) {
                        nextest.push(test);
                    }
                }
            }
            ["FAIL", ..] => {
                let test = line["FAIL".len()..].trim().to_string();
                if !test.is_empty() && !vitest.contains(&test) {
                    vitest.push(test);
                }
            }
            _ => {}
        }
        if let Some(test) = line.strip_prefix("test ").and_then(|l| l.strip_suffix(" ... FAILED")) {
            libtest.push(test.to_string());
        }
        if line.starts_with("test result: FAILED.") {
            libtest_said += count(line, "failed");
            bad_binaries += 1;
        }
        if line.starts_with("Summary [") && line.contains(" run: ") {
            nextest_said += count(line, "failed") + count(line, "timed");
            whole &= count(line, "exec") == 0;
        }
        if line.starts_with("Tests  ") {
            vitest_said += count(line, "failed");
        }
        if line.starts_with("error: test failed, to rerun pass") {
            reruns += 1;
        }
        let cut = line.starts_with("error[E")
            || line.starts_with("error: could not compile")
            || line.starts_with("Errors  ")
            || line.starts_with("Canceling due to")
            || line.contains("not run due to")
            || line.contains("was not a part of any test")
            || line.contains("(signal:");
        whole &= !cut;
    }
    whole &= libtest.len() >= libtest_said && nextest.len() >= nextest_said && vitest.len() >= vitest_said && playwright.len() >= playwright_said;
    // A test binary that failed with no result of its own crashed.
    whole &= reruns <= bad_binaries;
    let tests: Vec<String> = [libtest, nextest, vitest, playwright].concat();
    whole &= !tests.is_empty();
    Failures { tests, whole }
}

/// Whether `command` is one command (after a `cd <dir> &&`), so no later part
/// of it was left unrun by the failure.
fn one_command(command: &str) -> bool {
    let (_, rest) = split_cd(without_opt_out(command));
    !rest.replace("2>&1", "").trim().contains(['\n', ';', '|', '&'])
}

/// `step` as it runs while a trunk red is known: a cargo run goes past its
/// first failing binary, so a second red is seen and not hidden behind the
/// known one.
pub fn past_failures(reds: &[TrunkRed], step: &Step) -> Step {
    let mut out = step.clone();
    let (_, rest) = split_cd(&step.command);
    if reds.is_empty() || step.command.contains("--no-fail-fast") {
        return out;
    }
    if let Some(runner) = ["cargo test", "cargo nextest run"].into_iter().find(|r| rest.trim_start().starts_with(r)) {
        out.command = step.command.replacen(runner, &format!("{runner} --no-fail-fast"), 1);
    }
    out
}

/// What a red step's `log` says of the known reds: the rows it names are pulled
/// into `row`'s running group, and `Some` says what the step is held over,
/// where those are all that failed.
pub fn weigh_red(store: &Store, row: &str, reds: &[TrunkRed], step: &Step, log: &Path) -> Option<String> {
    if reds.is_empty() {
        return None;
    }
    let output = std::fs::read(log).ok()?;
    let failed = failures_in(&String::from_utf8_lossy(&output));
    let named: Vec<&TrunkRed> = reds.iter().filter(|r| failed.tests.iter().any(|t| names_red(t, &r.name))).collect();
    let mut rows: Vec<&str> = named.iter().map(|r| r.row.as_str()).collect();
    rows.dedup();
    for red in rows {
        crate::cycle::pull_trunk_red(store, row, red);
    }
    let all = failed.whole && one_command(&step.command) && failed.tests.iter().all(|t| named.iter().any(|r| names_red(t, &r.name)));
    all.then(|| named.iter().map(|r| format!("`{}` ({})", r.name, r.row)).collect::<Vec<_>>().join(", "))
}

/// [`run_all`] for the row `row`'s own check: a step red on nothing but a known
/// trunk red does not fail it, and does not stop the steps after it.
pub fn run_row(store: &Store, row: &Task, dir: &Path, steps: &[Step], log: &Path, reuse: Option<&Reuse>, runner: &Runner<'_>, mut each: impl FnMut(&Ran)) -> Vec<Ran> {
    let reds = known_reds(store, row);
    let mut out: Vec<Ran> = Vec::new();
    for s in steps {
        let ran = if out.last().is_some_and(|r| r.done.stops()) {
            Ran { step: s.clone(), done: Done::Skipped, took: Duration::ZERO, tail: String::new(), first: None, reused: None, build: None, via: None }
        } else {
            let mut ran = run_again_as(dir, s, &past_failures(&reds, s), log, reuse, runner);
            if matches!(ran.done, Done::Failed { timed_out: false, .. }) {
                if let Some(what) = weigh_red(store, &row.id, &reds, s, log) {
                    ran.done = Done::HeldOver(what);
                }
            }
            ran
        };
        each(&ran);
        out.push(ran);
    }
    out
}

/// How many steps of a run were reused from the ledger and how many ran.
pub fn tally(ran: &[Ran]) -> (usize, usize) {
    let reused = ran.iter().filter(|r| r.reused.is_some()).count();
    let skipped = ran.iter().filter(|r| r.done == Done::Skipped).count();
    (reused, ran.len() - reused - skipped)
}

/// `N reused, M ran`, said on every run.
pub fn tally_line(ran: &[Ran]) -> String {
    let (reused, ran) = tally(ran);
    format!("{reused} reused, {ran} ran")
}

pub fn passed(ran: &[Ran]) -> bool {
    ran.iter().all(|r| r.done.is_green())
}

pub fn secs(d: Duration) -> u64 {
    d.as_secs().max(u64::from(d > Duration::ZERO))
}

pub fn why_failed(r: &Ran) -> String {
    match &r.done {
        Done::Failed { timed_out: true, .. } if r.build.is_some() => format!(
            "ran {}s past twice its declared budget of {}s, after a cold build of {}s",
            secs(run_took(r)),
            r.step.limit.as_secs(),
            secs(r.build.unwrap_or_default())
        ),
        Done::Failed { timed_out: true, .. } => format!(
            "timed out after {}s (declared budget {}s, measured {}s)",
            r.step.limit.as_secs(),
            r.step.limit.as_secs(),
            secs(r.took)
        ),
        Done::Failed { exit: Some(n), .. } => format!("exit {n}"),
        Done::Failed { exit: None, .. } => "no exit status".into(),
        Done::Malformed(why) => format!("malformed: {why}"),
        Done::NoTests(why) => why.clone(),
        Done::Hung(pids) => format!("hung (no output, no CPU) — pids {}", pids.lines().filter_map(|l| l.split_whitespace().next()).collect::<Vec<_>>().join(" ")),
        Done::TreeMissing(path) => format!("tree missing: {path}"),
        _ => String::new(),
    }
}

/// The step lines an entry carries.
pub fn account(ran: &[Ran]) -> String {
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
            (Done::Passed, Some((why, _))) if r.build.is_some() => {
                let (run, build, limit) = (secs(run_took(r)), secs(r.build.unwrap_or_default()), r.step.limit.as_secs());
                let judged = if run_took(r) <= r.step.limit {
                    format!("within its {limit}s limit")
                } else {
                    format!("limit too tight: past its {limit}s limit and within twice it; raise the step's limit")
                };
                ("✓", format!("{run}s · {judged} — the first run was {why}; built in {build}s, then ran {run}s"))
            }
            (Done::Passed, Some((why, _))) if why.starts_with("timed out") => {
                ("✓", format!("{}s · limit too tight: passed on a second run at twice the limit, the first was {why}; raise the step's limit", secs(r.took)))
            }
            (Done::Passed, Some((why, _))) => ("✓", format!("{}s · passed on a second run, the first was {why}", secs(r.took))),
            (Done::Malformed(why), _) => ("✗", format!("malformed — the line cannot start: {why}")),
            (Done::NoTests(_), _) => ("✗", format!("{}s · {}", secs(r.took), why_failed(r))),
            (Done::Hung(_), _) => ("✗", format!("{}s · {}", secs(r.took), why_failed(r))),
            (Done::TreeMissing(_), _) => ("?", format!("{}s · {} — an environment fault, not a failed step", secs(r.took), why_failed(r))),
            (Done::Failed { .. }, first) => {
                let twice = if first.is_some() { " both times" } else { "" };
                ("✗", format!("{}s · {}{twice}", secs(r.took), why_failed(r)))
            }
            (Done::Skipped, _) => ("–", "skipped".to_string()),
            (Done::HeldOver(what), _) => ("✓", format!("{}s · red only on the trunk red {what}, held over until that row lands", secs(r.took))),
        };
        let via = r.via.map(|v| format!(" · by {v}")).unwrap_or_default();
        out.push(format!("{mark} {} · {}{via} · {rest} · {}", r.step.name, r.step.command.lines().next().unwrap_or_default(), why_in(&r.step)));
        if r.done.is_red() || matches!(r.done, Done::TreeMissing(_)) {
            out.extend(r.tail.lines().map(|l| format!("  | {l}")));
        } else if let Some((_, tail)) = &r.first {
            out.extend(tail.lines().map(|l| format!("  ~ {l}")));
        }
    }
    out.join("\n")
}

/// What a failed run owes, for the row's log and the pane: the first failing
/// step, its command and the end of its output.
pub fn owed(ran: &[Ran], sha: &str) -> String {
    if let Some(r) = ran.iter().find(|r| matches!(r.done, Done::TreeMissing(_))) {
        return format!(
            "wsp check could not run `{}` at {}: {}. The tree went away under the run, not the work — nothing was sent back; \
             the check runs again when the tree is back.",
            r.step.name,
            &sha[..sha.len().min(12)],
            why_failed(r),
        );
    }
    let Some(r) = ran.iter().find(|r| r.done.is_red()) else { return String::new() };
    if let Done::Malformed(why) = &r.done {
        return format!(
            "wsp check could not start `{}` at {}: {why}. The line is malformed, not the work — nothing was sent back. \
             Fix it with `wsp check <id> --set --why \"…\" -` (one `- name: command` item per check, the command alone in backticks if a note follows).",
            r.step.name,
            &sha[..sha.len().min(12)],
        );
    }
    let tail: Vec<&str> = r.tail.lines().collect();
    let tail = tail[tail.len().saturating_sub(12)..].join("\n");
    format!(
        "wsp check failed at {}: `{}` ({}) — {}, {}s. Run it yourself and fix what it reports; the whole run is under ## Verification. \
         A test red on the trunk too is not yours to fix: the row that owns it says so with `wsp note <that row> \"{TRUNK_RED} <test>\"`.\n\n{tail}",
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
pub fn stale_after(steps: &[Step]) -> i64 {
    (steps.iter().map(|s| s.limit).sum::<Duration>() + TREE_WAIT + Duration::from_secs(300)).as_secs() as i64
}

/// Write a `running` entry for the landing, unless one is owed to nobody.
///
/// `Some(at)` is an entry written and waiting for its run. `None` is a run
/// already under way or finished for this commit — unless `again`, which a
/// person asks for and which ignores a finished one.
pub fn begin(store: &Store, id: &str, sha: &str, stale: i64, again: bool) -> Option<String> {
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
pub enum Settled {
    Passed,
    /// Failed, and the row was at review: it is back at `doing`.
    SentBack,
    /// Failed, and the row was not at review, or the verifier's hold had
    /// already weighed the red ([`weighed_by_hold`]): recorded, nobody moved.
    Failed,
    /// A step could not start: the line is wrong, not the work, so the row is
    /// recorded and left where it is, and its owner is asked to fix the line.
    Malformed,
    /// Timed out while the machine was over its load line: not a verdict, the
    /// row stays at review, and the gate runs the check again once the load falls.
    Waiting,
    /// A step's tree was not there (`wsp-067`): an environment fault, not a
    /// verdict. The row stays where it is and the gate runs the check again.
    TreeMissing,
    /// The row or its entry is gone.
    Lost,
}

/// Whether a red at `read` is one the verifier's hold already weighed
/// (`compound-react-121`): the newest pass holds that commit, as read or as
/// landed, and every red step's command had failed in the last run at the commit
/// the pass read, before it decided. `this` is the running entry's stamp. A red
/// the hold did not see, one the rebase brought in, is not weighed.
fn weighed_by_hold(t: &Task, runs: &[Run], this: &str, read: &str, ran: &[Ran]) -> bool {
    let Some(pass) = v::passes(t).pop().filter(|p| p.state == v::State::Holds) else { return false };
    if !pass.landed.as_deref().or(pass.own()).is_some_and(|c| v::same_commit(c, read)) {
        return false;
    }
    let decided = pass.decided.as_deref().map(util::epoch_of);
    let seen = |r: &&Run| {
        r.at != this
            && decided.is_none_or(|d| util::epoch_of(&r.at) <= d)
            && r.read.as_deref().is_some_and(|x| [pass.read.as_deref(), pass.own()].into_iter().flatten().any(|c| v::same_commit(x, c)))
    };
    let Some(before) = runs.iter().rev().find(seen).filter(|r| r.state == RunState::Failed) else { return false };
    // By command, not name: a label is often rewritten to name the red.
    ran.iter().filter(|r| r.done.is_red()).all(|r| {
        let command = format!(" · {} · ", r.step.command.lines().next().unwrap_or_default());
        before.text.lines().any(|l| l.starts_with("✗ ") && l.contains(&command))
    })
}

/// Write the outcome over its `running` entry — and, when it failed on a row at
/// review, send the row back in the same write, so no reader ever sees a failed
/// landing as finished. A red the verifier's hold already weighed sends nothing
/// back ([`weighed_by_hold`]).
pub fn settle(store: &Store, id: &str, at: &str, ran: &[Ran], took: Duration) -> Settled {
    settle_started(store, id, at, ran, took, None)
}

/// [`settle`], with how the tree stood when the steps began, so the event says
/// whether a long step began on a cold `target/` or a busy machine.
pub fn settle_started(store: &Store, id: &str, at: &str, ran: &[Ran], took: Duration, start: Option<&Start>) -> Settled {
    settle_loaded(store, id, at, ran, took, start, crate::throttle::load1())
}

/// [`settle_started`], with the load at the end given.
fn settle_loaded(store: &Store, id: &str, at: &str, ran: &[Ran], took: Duration, start: Option<&Start>, load_end: Option<f64>) -> Settled {
    // `wsp-644`: a step that took its whole budget at a commit it already timed
    // out at under load is not read as load again: the budget is under the step's
    // own run time, and a third and fourth run would only time out again.
    let before = store.find_task(id).map(|t| v::runs(&t)).unwrap_or_default();
    let read_now = before.iter().find(|r| r.at == at).and_then(|r| r.read.clone());
    let timed_before = before.iter().any(|r| r.at != at && r.state == RunState::Waiting && r.read == read_now);
    let busy = start.filter(|_| !timed_before).and_then(|s| timed_out_under_load(ran, s, load_end));
    let missing = ran.iter().find_map(|r| match &r.done {
        Done::TreeMissing(path) => Some(path.clone()),
        _ => None,
    });
    let state = if passed(ran) {
        RunState::Passed
    } else if busy.is_some() || missing.is_some() {
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
        } else if let Some(path) = &missing {
            t.updated = util::now_iso();
            t.log(&format!("wsp: check tree missing: {path} — an environment fault, not the work; the row stays where it is and the check runs again"));
            out = Settled::TreeMissing;
        } else if state == RunState::Waiting {
            t.updated = util::now_iso();
            t.log(&format!("wsp: check {WAITING}{} — the row stays where it is", busy.as_deref().unwrap_or_default()));
        } else if state == RunState::Failed && t.status() == Status::Review && weighed_by_hold(&t, &runs, at, read.as_deref().unwrap_or_default(), ran) {
            t.updated = util::now_iso();
            t.log(&format!(
                "wsp: check red at {}, as the verifier's hold had already weighed — the row stays at review",
                read.as_deref().unwrap_or_default()
            ));
        } else if state == RunState::Failed && t.status() == Status::Review {
            let mut owes = owed(ran, read.as_deref().unwrap_or_default());
            if !earlier.is_empty() && ran.iter().any(|r| matches!(r.done, Done::Failed { timed_out: true, .. })) {
                let now = start.map(|s| format!(", then at normal {}", s.line())).unwrap_or_default();
                owes = format!("Timed out twice: under load ({}){now}.\n\n{owes}", earlier.join("; "));
            }
            if let Some(r) = ran.iter().find(|r| matches!(r.done, Done::Failed { timed_out: true, .. })) {
                owes = format!(
                    "`{}` took its whole budget: declared {}s, measured {}s{}. Raise its limit past its run time (`wsp check <id> --set --why … -`) or make it faster; it is not queued to rerun.\n\n{owes}",
                    r.step.name,
                    r.step.limit.as_secs(),
                    secs(run_took(r)),
                    r.build.map(|b| format!(" after a cold build of {}s, which is not counted", secs(b))).unwrap_or_default()
                );
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
    let steps: Vec<_> = ran.iter().map(|r| serde_json::json!({ "name": r.step.name, "secs": secs(r.took), "reused": r.reused.is_some(), "build": r.build.map(secs) })).collect();
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
pub const WAITING: &str = "timed out under load, ";

/// The run timed out (a step stopped by its limit) while the one-minute load,
/// at the start or at the end, stood over the line at which new work is held
/// (`wsp-595`): a timeout on a machine nobody can work on is not a verdict on
/// the change. Says which loads, for the line on the row.
///
/// `wsp-644`: only the load **at the start** excuses a timeout. The load at the
/// end is the step's own (a test that builds and spawns raises it), so a step
/// that began on a machine under the line and took its whole budget is a
/// verdict on its budget, not on the machine.
pub fn timed_out_under_load(ran: &[Ran], start: &Start, load_end: Option<f64>) -> Option<String> {
    ran.iter().find(|r| matches!(r.done, Done::Failed { timed_out: true, .. }))?;
    start.load.filter(|l| crate::throttle::load_over_hold(*l, start.cores))?;
    let f = |l: Option<f64>| l.map(|l| format!("{l:.1}")).unwrap_or_else(|| "?".into());
    Some(format!("load {} at the start, {} at the end, of {} cores", f(start.load), f(load_end), start.cores))
}

pub const CHECK_HEAD: &str = "`wsp check` failed on this row's landing. What is owed:\n\n";

// ---- the verb -----------------------------------------------------------------

const USAGE: &str = "wsp check <task> [--plan] [--at <commit>] [--since <commit>] [--json]\n       wsp check --inner [<task>] [--run]   the steps this tree's changes call for\n       wsp check --affected [<task>] [--run]   those and the targets the crate and module graphs reach\n       wsp check <project> --barrier [--plan|--barrier-tree] [--at <commit>]\n       wsp check <project> --tiers   its declared steps by tier\n       wsp check <task|project> --set [--why \"…\"] --from FILE   (or `-`) declare its ## Checks; a change to a row's says why";

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
        Ok(tg) => {
            let row_tree = args.get("since").and_then(|_| crate::cmd_checkout::member_tree(store, &t.id));
            let since = args.get("since").map(|s| commit_named(&tg.repo, row_tree.as_deref(), &s).unwrap_or(s));
            Target { since, ..tg }
        }
        Err(why) => {
            if auto {
                crate::store::log_line(store, &format!("check {}: not run — {why}", t.id));
            } else {
                eprintln!("wsp: {why}");
            }
            return if auto { 0 } else { 1 };
        }
    };
    let Plan { steps, changed, narrowed } = plan(store, &t, &tg, at.as_deref());
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
            println!("{}", serde_json::json!({ "task": t.id, "commit": tg.sha, "steps": rows, "narrowed": narrowed }));
            return 0;
        }
        println!("{} at {}", p.bold(&t.id), &tg.sha[..12]);
        for s in &steps {
            println!("  {}", plan_line(s, green(s).as_ref()));
        }
        for l in &narrowed {
            println!("  {}", p.dim(&format!("narrowed: {l}")));
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
    let runner = match Runner::chosen(store, Whose::Row(&t.id), TreeKind::Check, &tree, Some(&dir.join("lock"))) {
        Ok(r) => r,
        Err(why) => {
            drop(lock);
            return gave_up(store, &t.id, entry.as_deref(), &why);
        }
    };
    // The member's own tree too: it is where the verifier's gate checks and the
    // verifier's seat stands, and nothing else installs there (clonefile is
    // macOS only), so a Rust-only row met "Cannot find package 'vite'" at its
    // verifier. A `node_modules/` the member made by hand carries no stamp and
    // is left as it is; only a missing one, or one wsp made for another
    // lockfile, is installed.
    let mut start = start;
    for line in start.install(&tree, &dir.join("last.log"), &runner) {
        if !auto && !args.json() {
            println!("  {}", p.dim(&line));
        }
    }
    if !auto && !args.json() {
        for l in &narrowed {
            println!("  {}", p.dim(&format!("narrowed: {l}")));
        }
    }
    let ran = run_row(store, &t, &tree, &steps, &dir.join("last.log"), ledger.as_ref(), &runner, |r| {
        if !auto && !args.json() {
            let (mark, tail) = match &r.done {
                Done::Passed if r.reused.is_some() => (p.green("✓"), " · reused".to_string()),
                Done::Passed => (p.green("✓"), String::new()),
                Done::Failed { .. } | Done::Malformed(_) | Done::Hung(_) | Done::NoTests(_) => (p.red("✗"), format!(" · {}", why_failed(r))),
                Done::TreeMissing(_) => (p.dim("?"), format!(" · {}", why_failed(r))),
                Done::Skipped => (p.dim("–"), " · skipped".to_string()),
                Done::HeldOver(what) => (p.green("✓"), format!(" · held over the trunk red {what}")),
            };
            // A named commit may have been named by a symbol, so each step says
            // which commit it was.
            let of = at.as_ref().map(|_| format!(" · at {}", &tg.sha[..12])).unwrap_or_default();
            println!("  {mark} {} · {}s{of}{tail}", r.step.name, secs(r.took));
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
            let skip = if matches!(settled, Settled::Malformed | Settled::TreeMissing) { 0 } else { 2 };
            for l in owed(&ran, &tg.sha).lines().skip(skip) {
                println!("    {}", p.dim(l));
            }
        }
        let tail = match (&settled, record) {
            (Settled::SentBack, _) => format!(" · {} is back at doing with this as what is owed", t.id),
            (Settled::Waiting, _) => format!(" · timed out under load, rerunning once the load falls; {} stays where it is", t.id),
            (Settled::TreeMissing, _) => format!(" · a tree was missing, not a failure; {} stays where it is", t.id),
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
        // At the leg cap no rework leg carries the failure, and nothing else
        // would say so to whoever decides (`wsp-690`).
        crate::verification::tell_if_capped(store, &t.id, &owed(&ran, &tg.sha));
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
pub fn tier(s: &Step) -> &'static str {
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
pub fn plan_text(store: &Store, t: &Task, tg: &Target) -> String {
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
pub fn greens_held(store: &Store, t: &Task) -> Option<(String, Vec<(String, Green)>)> {
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
pub fn pieces_results(store: &Store, t: &Task) -> Vec<(usize, Vec<String>)> {
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
pub fn inner_steps(store: &Store, t: &Task, changed: &[String]) -> Vec<Step> {
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
pub fn affected_steps(store: &Store, t: &Task, tree: &Path, changed: &[String]) -> Vec<Step> {
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
    let in_tests = |word: &str| git(&tree, &["grep", "-q", "--untracked", "-F", "-e", word, "--", "tests"]).is_some();
    let (steps, narrowed) = crate::affected::narrow_steps(steps, &changed, &in_tree, &in_tests);
    if steps.is_empty() {
        println!("{} {}", p.dim("nothing to run —"), p.dim(&format!("{} changed path(s) call for no check of {}", changed.len(), t.id)));
        return 0;
    }
    if !run {
        println!("{} · {} changed path(s)", p.bold(&t.id), changed.len());
        for s in &steps {
            println!("  {} · {} · up to {}s · {}", s.name, s.command, s.limit.as_secs(), why_in(s));
        }
        for l in &narrowed {
            println!("  {}", p.dim(&format!("narrowed: {l}")));
        }
        return 0;
    }
    for l in &narrowed {
        println!("  {}", p.dim(&format!("narrowed: {l}")));
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
    let runner = match Runner::chosen(store, Whose::Row(&t.id), TreeKind::Work, &tree, None) {
        Ok(r) => r,
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    };
    let ran = run_all(&tree, &steps, &log, reuse.as_ref(), &runner, |r| match &r.done {
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
    let Some(sha) = args.get("at").map(|a| commit_named(&repo, None, &a)).unwrap_or_else(|| {
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
    let runner = match Runner::chosen(store, Whose::Barrier(&found.id), TreeKind::Barrier, &tree, Some(&dir.join("lock"))) {
        Ok(r) => r,
        Err(why) => {
            drop(lock);
            return gave_up(store, &found.id, None, &why);
        }
    };
    let mut start = start;
    for line in start.install(&tree, &dir.join("last.log"), &runner) {
        println!("  {}", p.dim(&line));
    }
    let ran = run_all(&tree, &steps, &dir.join("last.log"), None, &runner, |r| {
        let (mark, tail) = match &r.done {
            Done::Passed => (p.green("✓"), String::new()),
            Done::Failed { .. } | Done::Malformed(_) | Done::Hung(_) | Done::NoTests(_) => (p.red("✗"), format!(" · {}", why_failed(r))),
            Done::TreeMissing(_) => (p.dim("?"), format!(" · {}", why_failed(r))),
            Done::Skipped => (p.dim("–"), " · skipped".to_string()),
            Done::HeldOver(what) => (p.green("✓"), format!(" · held over the trunk red {what}")),
        };
        println!("  {mark} {} · {}s{tail}", r.step.name, secs(r.took));
    });
    drop(lock);
    let ok = passed(&ran);
    if !ok {
        let skip = if ran.iter().any(|r| matches!(r.done, Done::TreeMissing(_))) { 0 } else { 2 };
        for l in owed(&ran, &tg.sha).lines().skip(skip) {
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
    set_as(store, args, needle, usage, &setter(store))
}

/// [`set`] for a caller already read (wsp-681).
pub fn set_as(store: &Store, args: &Args, needle: &str, usage: &str, role: &Setter) -> i32 {
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
    let why = args.get("why").map(|w| w.trim().to_string()).filter(|w| !w.is_empty());
    let what = if let Some(mut proj) = store.project(needle) {
        if let Err(no) = project_checks_allowed(role, &proj.id) {
            return refuse_set(store, role, &proj.id, &no);
        }
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
        let before = t.section(SECTION).unwrap_or_default();
        let edits = edits(&before, &text, &touched_by(store, &t));
        let refused: Vec<String> = edits.iter().filter_map(|e| e.refused.clone()).collect();
        let list = crate::worklist::Running::read(store).list_for(&t).map(str::to_string);
        if let Err(no) = checks_allowed(role, &t.id, list.as_deref(), t.project.as_deref(), &refused) {
            return refuse_set(store, role, &t.id, &no);
        }
        if !edits.is_empty() && !before.trim().is_empty() && why.is_none() {
            eprintln!("wsp: not written — a change to {}'s checks says why: `wsp check {} --set --why \"…\" -`; the reason is logged on the row beside each change, and its verifier reads it", t.id, t.id);
            return 2;
        }
        crate::model::set_section_in(&mut t.body, SECTION, &text);
        if !before.trim().is_empty() {
            let who = role.name();
            for e in &edits {
                t.log(&e.log_line(&who, why.as_deref().unwrap_or_default()));
            }
        }
        if let Err(e) = store.save_task(&t) {
            eprintln!("wsp: {e}");
            return 1;
        }
        t.id
    };
    store.git_commit(&format!("wsp: {what} declares {} check(s)", steps.len()));
    println!("{what}: {} check(s) — {}", steps.len(), steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "));
    for line in store.find_task(&what).map(|t| under_measured(&t, &steps)).unwrap_or_default() {
        println!("  warning: {line}");
    }
    0
}

/// `wsp-364`: each of `steps` declared under what the row's last check
/// measured for it (its own run, a cold build left out), named with both, so
/// a `--set` that will time out says so when it is written, not at the gate.
pub fn under_measured(t: &Task, steps: &[Step]) -> Vec<String> {
    let runs = v::runs(t);
    steps
        .iter()
        .filter_map(|s| {
            let (took, at) = runs.iter().rev().find_map(|r| {
                r.text.lines().find_map(|l| {
                    let mut parts = l.get(l.find(' ')? + 1..)?.split(" · ");
                    (parts.next()? == s.name).then_some(())?;
                    let took: u64 = parts.nth(1)?.strip_suffix('s')?.parse().ok()?;
                    Some((took, r.read.clone().unwrap_or_default()))
                })
            })?;
            (s.limit < Duration::from_secs(took)).then(|| {
                format!("`{}` is declared {}s and its last check at {at} measured {took}s — it will time out; raise it past its run", s.name, s.limit.as_secs())
            })
        })
        .collect()
}

fn refuse_set(store: &Store, role: &Setter, id: &str, why: &str) -> i32 {
    eprintln!("wsp: not written — {why}");
    crate::store::log_line(store, &format!("refused check {id} --set by {}: {why}", role.name()));
    2
}

/// Who is setting checks, read from the seat wsp recorded for the caller and
/// never from anything it says: the reading the seats layer's role check makes, from the
/// same seat helpers in wire and store, so the engine does not reach up into
/// the seats layer for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setter {
    /// No seat: a person at a terminal, or the daemon.
    Person,
    /// The primary seat.
    Primary,
    /// The governor of this list (or project).
    Governor(String),
    /// A member, verifier or check seat, and the row its seat is bound to.
    Member(Option<String>),
}

impl Setter {
    pub fn name(&self) -> String {
        match self {
            Setter::Person => "a person".into(),
            Setter::Primary => "the primary seat".into(),
            Setter::Governor(s) => format!("the governor of {s}"),
            Setter::Member(_) => "the row's member seat".into(),
        }
    }
}

/// The caller of `wsp check --set`.
pub fn setter(store: &Store) -> Setter {
    let Some(pane) = crate::place::my_pane() else { return Setter::Person };
    match crate::place::governs(&store.governors(), &crate::place::Seat::new(&pane)) {
        Some(s) if crate::governors::is_primary(&s) => Setter::Primary,
        Some(s) => Setter::Governor(s),
        None => {
            let spawned = std::env::var_os(crate::base::seat::SEAT_ENV).is_some_and(|v| !v.is_empty());
            let bound = store.bindings().get(&pane).cloned();
            match spawned || bound.is_some() {
                true => Setter::Member(bound.and_then(|b| b.get("task_id")?.as_str().map(str::to_string))),
                false => Setter::Person,
            }
        }
    }
}

/// Who may make what a member may not, for a row on `list`.
pub fn who_may(list: Option<&str>) -> String {
    match list {
        Some(l) => format!("the governor of {l}, or Ed"),
        None => "Ed, or the primary seat".into(),
    }
}

/// Whether `role` may write `row`'s checks, as a pure decision. `list` and
/// `project` are the row's, and `refused` what of the change a member may not
/// make ([`edits`]).
///
/// A person and the primary set anything. A governor sets the checks of the
/// rows on the list (or project) it governs, whatever the change. A member sets
/// its own row's only, and only what [`edits`] lets it.
pub fn checks_allowed(role: &Setter, row: &str, list: Option<&str>, project: Option<&str>, refused: &[String]) -> Result<(), String> {
    match role {
        Setter::Person | Setter::Primary => Ok(()),
        Setter::Governor(scope) if list == Some(scope.as_str()) || project == Some(scope.as_str()) => Ok(()),
        Setter::Governor(scope) => Err(format!(
            "{row} is {}; the governor of {scope} sets the checks of rows on {scope} only — it needs {}",
            list.map(|l| format!("on list {l}")).unwrap_or_else(|| "on no list".into()),
            who_may(list)
        )),
        Setter::Member(own) if own.as_deref() != Some(row) => Err(format!(
            "a member sets the checks of its own row only ({}); {row}'s are set by {}",
            own.as_deref().unwrap_or("this seat is bound to none"),
            who_may(list)
        )),
        Setter::Member(_) if !refused.is_empty() => Err(format!(
            "a member may raise a step's budget, add or correct a leading `cd <dir> &&`, narrow a step to the files its commits touched, or add a step; {}: only {} may do that",
            refused.join("; "),
            who_may(list)
        )),
        Setter::Member(_) => Ok(()),
    }
}

/// Whether `role` may write a project's checks: never a member, verifier or
/// check seat, and a governor only its own project's.
pub fn project_checks_allowed(role: &Setter, project: &str) -> Result<(), String> {
    match role {
        Setter::Person | Setter::Primary => Ok(()),
        Setter::Governor(scope) if scope == project => Ok(()),
        Setter::Governor(scope) => Err(format!("the governor of {scope} does not set project {project}'s checks; Ed or the primary seat does")),
        Setter::Member(_) => Err(format!("a member, verifier or check seat does not set project {project}'s checks; Ed or the primary seat does")),
    }
}

// ---- what a `--set` changes (wsp-681) ----------------------------------------
//
// A plan row writes its checks before anyone has run them, so they are often a
// little wrong: a budget under a cold compile, a step missing its `cd`, a step
// far broader than the row's files. The member's seat sees the fix at once. It
// may make that fix itself, and only that: raise a budget, add or correct a
// leading `cd <dir> &&`, narrow a step to the files its commits touched, add a
// step. Deleting a step, lowering a budget or replacing a command with an
// unrelated one is its governor's (or Ed's). Every change is logged on the row,
// old line to new line, who and why, and the verifier is shown them.

/// The start of the row's log line for one change to its checks.
pub const CHECKS_CHANGED: &str = "checks changed by";

/// One change a `--set` makes to a row's declared checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// The line as it was; `None` for a step added.
    pub old: Option<String>,
    /// The line as it is; `None` for a step deleted.
    pub new: Option<String>,
    /// What the change does, in words.
    pub said: String,
    /// Why a member may not make it; `None` when it may.
    pub refused: Option<String>,
}

impl Edit {
    pub fn log_line(&self, who: &str, why: &str) -> String {
        let line = |l: &Option<String>, none: &str| l.as_ref().map(|l| format!("`{l}`")).unwrap_or_else(|| none.to_string());
        format!("{CHECKS_CHANGED} {who}: {} → {} ({}); why: {why}", line(&self.old, "(none)"), line(&self.new, "(deleted)"), self.said)
    }
}

/// The items of a section's text with the step each declares.
fn items_of(text: &str) -> Vec<(String, Step)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("- ").or_else(|| l.strip_prefix("* "))
        })
        .filter_map(|item| step_of(item, Of::Task).map(|s| (item.trim().to_string(), s)))
        .collect()
}

/// A command's leading `cd <dir> &&`, and what it runs there.
pub fn split_cd(command: &str) -> (Option<&str>, &str) {
    let c = command.trim();
    if let Some((dir, rest)) = c.strip_prefix("cd ").and_then(|r| r.split_once("&&")) {
        let dir = dir.trim();
        if !dir.is_empty() && !dir.contains(char::is_whitespace) {
            return (Some(dir), rest.trim());
        }
    }
    (None, c)
}

fn words(c: &str) -> Vec<&str> {
    c.split_whitespace().collect()
}

/// Flags that only narrow a test run to less of it.
const NARROWING: [&str; 6] = ["--", "--lib", "--test", "--exact", "-p", "--package"];

/// Whether `token`, an argument of a command run in `dir`, names files the row
/// touched: a path that is one of them or holds one, or a test filter whose
/// first segment is the module or file name of one (`check::`, `layering`).
pub fn touches(token: &str, dir: Option<&str>, touched: &[String]) -> bool {
    let t = token.trim_matches(|c| c == '"' || c == '\'').trim_end_matches('/');
    let t = t.strip_prefix("./").unwrap_or(t);
    if t.is_empty() || t == "." {
        return false;
    }
    let under = |f: &str, p: &str| f == p || f.starts_with(&format!("{p}/"));
    let rooted = dir.map(|d| format!("{}/{t}", d.trim_end_matches('/')));
    let first = t.split("::").next().unwrap_or_default();
    touched.iter().any(|f| {
        under(f, t)
            || rooted.as_deref().is_some_and(|r| under(f, r))
            || (!t.contains('/')
                && !first.is_empty()
                && (module_of(f).is_some_and(|m| m.rsplit('/').next() == Some(first))
                    || f.rsplit('/').next().and_then(|n| n.split('.').next()) == Some(first)))
    })
}

/// Whether `new` runs what `old` runs, narrowed to the files the row touched:
/// the same program, every word of `old` kept or a path of it replaced by a
/// narrower one, and every word added a touched path or filter, or a flag that
/// only narrows.
pub fn narrows(old: &str, new: &str, dir: Option<&str>, touched: &[String]) -> bool {
    let (o, n) = (words(old), words(new));
    if o.is_empty() || n.is_empty() || o[0] != n[0] {
        return false;
    }
    let added: Vec<(usize, &str)> = n.iter().enumerate().filter(|(_, w)| !o.contains(w)).map(|(i, w)| (i, *w)).collect();
    if added.is_empty() {
        return false;
    }
    let pathy = |w: &str| w.contains('/') || w == ".";
    for r in o.iter().filter(|w| !n.contains(w)) {
        let r = r.trim_end_matches('/');
        let narrower = |a: &str| r == "." || a.trim_end_matches('/').starts_with(&format!("{r}/"));
        if !pathy(r) || !added.iter().any(|(_, a)| narrower(a)) {
            return false;
        }
    }
    added.iter().all(|&(i, w)| {
        let after_p = i > 0 && matches!(n[i - 1], "-p" | "--package");
        touches(w, dir, touched)
            || NARROWING.contains(&w)
            || (after_p && touched.iter().any(|f| f.strip_prefix("crates/").and_then(|r| r.split('/').next()).is_some_and(|c| w == c || w.ends_with(&format!("-{c}")))))
    })
}

/// What a change from one step to another does, and what of it a member may
/// not do.
fn compare(old: &Step, new: &Step, touched: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut said, mut refused) = (Vec::new(), Vec::new());
    let (o, n) = (secs(old.limit), secs(new.limit));
    if n > o {
        said.push(format!("raises its budget {o}s → {n}s"));
    } else if n < o {
        refused.push(format!("`{}` lowers its budget {o}s → {n}s", old.name));
    }
    let ((od, oc), (nd, nc)) = (split_cd(&old.command), split_cd(&new.command));
    match (od, nd) {
        (a, b) if a == b => {}
        (None, Some(d)) => said.push(format!("adds `cd {d} &&`")),
        (Some(a), Some(b)) => said.push(format!("corrects its cd {a} → {b}")),
        (Some(a), None) => refused.push(format!("`{}` drops its `cd {a} &&`", old.name)),
        (None, None) => {}
    }
    if words(oc) != words(nc) {
        match narrows(oc, nc, nd, touched) {
            true => said.push(format!("narrows `{oc}` to `{nc}`")),
            false => refused.push(format!("`{}` replaces `{oc}` with `{nc}`, which is not it narrowed to the files the row touched", old.name)),
        }
    }
    if old.name != new.name {
        said.push(format!("renames {} → {}", old.name, new.name));
    }
    (said, refused)
}

/// Every change from the `before` text of a row's checks to the `after`, with
/// what a member may not make. Steps are paired by name, then by the command
/// they run once a `cd` is set aside; a step of `before` with no pair is
/// deleted, one of `after` with none is added.
pub fn edits(before: &str, after: &str, touched: &[String]) -> Vec<Edit> {
    let (old, new) = (items_of(before), items_of(after));
    let mut used = vec![false; new.len()];
    let mut out = Vec::new();
    for (line, o) in &old {
        let by_name = (0..new.len()).find(|&j| !used[j] && new[j].1.name == o.name);
        let by_command = || (0..new.len()).find(|&j| !used[j] && words(split_cd(&new[j].1.command).1) == words(split_cd(&o.command).1));
        let Some(j) = by_name.or_else(by_command) else {
            out.push(Edit {
                old: Some(line.clone()),
                new: None,
                said: format!("deletes `{}`", o.name),
                refused: Some(format!("`{}` is deleted", o.name)),
            });
            continue;
        };
        used[j] = true;
        let (nline, n) = &new[j];
        if nline == line {
            continue;
        }
        let (said, refused) = compare(o, n, touched);
        if said.is_empty() && refused.is_empty() {
            continue;
        }
        let mut all = said.clone();
        all.extend(refused.iter().cloned());
        out.push(Edit {
            old: Some(line.clone()),
            new: Some(nline.clone()),
            said: all.join("; "),
            refused: (!refused.is_empty()).then(|| refused.join("; ")),
        });
    }
    for (j, (line, n)) in new.iter().enumerate() {
        if !used[j] {
            out.push(Edit { old: None, new: Some(line.clone()), said: format!("adds `{}`", n.name), refused: None });
        }
    }
    out
}

/// The files the row's commits touched: in its own tree, the commits that name
/// it, and at its landing, what the landing changed.
pub fn touched_by(store: &Store, t: &Task) -> Vec<String> {
    let grep = format!("--grep=(^|[^[:alnum:]-]){}([^0-9]|$)", t.id);
    let mut out: Vec<String> = crate::cmd_checkout::member_tree(store, &t.id)
        .and_then(|tree| git(&tree, &["log", "HEAD", "-E", &grep, "--name-only", "--format=", "--max-count=200"]))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if let Ok(tg) = target(store, t, None) {
        out.extend(changed_paths(&tg, t, true));
    }
    out.sort();
    out.dedup();
    out
}

/// The changes to a row's checks its log records, oldest first: the lines
/// [`Edit::log_line`] wrote, from the who onward.
pub fn changes_logged(t: &Task) -> Vec<String> {
    let log = t.section("Log").unwrap_or_default();
    log.lines().filter_map(|l| l.split_once(CHECKS_CHANGED).map(|(_, rest)| rest.trim().to_string())).collect()
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
pub enum Gate {
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
pub fn gate(store: &Store, member: &Task) -> Gate {
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
pub fn running_at(store: &Store, member: &Task, sha: &str) -> bool {
    let Some(entry) = v::runs(member).into_iter().rev().find(|r| r.read.as_deref().is_some_and(|x| v::same_commit(x, sha))) else { return false };
    if entry.state != RunState::Running {
        return false;
    }
    let Ok(tg) = target(store, member, None) else { return false };
    let stale = stale_after(&plan(store, member, &tg, None).steps);
    util::epoch_secs() - util::epoch_of(&entry.at) <= stale
}

pub use crate::verification::carry_pass;

/// What `wsp review` says to a member that has recorded no checks: tier 1 is the
/// member's to write, and a warning rather than a refusal, because design work
/// has nothing to run. Silent for a project with no repository to run in.
pub fn review_hint(store: &Store, t: &Task) -> Option<String> {
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
pub fn rerun_steps(
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
pub fn rerun(
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
    let runner = match Runner::chosen(store, Whose::Row(task), TreeKind::Work, dir, Some(&lockdir.join("lock"))) {
        Ok(r) => r,
        Err(why) => return Rerun::NoLock(why),
    };
    let log = lockdir.join("land.log");
    let ran = match store.find_task(task) {
        Some(t) => run_row(store, &t, dir, &steps, &log, reuse.as_ref(), &runner, |_| {}),
        None => run_all(dir, &steps, &log, reuse.as_ref(), &runner, |_| {}),
    };
    drop(lock);
    if !passed(&ran) {
        return Rerun::Failed(owed(&ran, new_head));
    }
    Rerun::Ran(steps.iter().map(|s| s.name.clone()).collect())
}

/// After `wsp land`: start the run for a row that no list will gate.
pub fn after_land(store: &Store, id: &str) {
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
    use crate::host::JobAt;
    use wsp_trees::job_place::host_of;

    fn step(text: &str) -> Step {
        parse_steps(text, Of::Project).remove(0)
    }

    #[test]
    fn a_plain_cargo_test_reads_as_nextest_with_its_package_and_filters_and_anything_else_stays() {
        let run = |c: &str| by_nextest(c).map(|n| n.run);
        let to = |rest: &str| Some(format!("cargo nextest run --no-tests=pass --ignore-default-filter {rest}"));
        assert_eq!(run("cargo test -p wsp-engine --lib cycle::"), to("-p wsp-engine --lib cycle::"));
        assert_eq!(run("cargo test --test layering --features=a,b"), to("--test layering --features a,b"));
        // cargo's `--profile` is the build's; nextest's is not.
        assert_eq!(run("cargo test --lib --profile slim"), to("--lib --cargo-profile slim"));
        assert_eq!(run("cargo test --lib --profile=slim"), to("--lib --cargo-profile slim"));
        let n = by_nextest("cd web && cargo test --lib census:: -- --exact --skip slow").unwrap();
        assert_eq!((n.dir.as_deref(), n.doc), (Some("web"), None), "a target selection has no doctests to follow");
        assert_eq!(Some(n.run), to("--lib census:: -- --exact --skip slow"));
        // No target selection: its doctests follow, same packages, features and filter.
        let doc = by_nextest("cargo test --workspace --exclude x -F a --profile slim census:: -- --skip slow").unwrap().doc.unwrap();
        assert_eq!((doc.workspace, doc.exclude), (true, vec!["x".to_string()]));
        assert_eq!(doc.rest, ["-F", "a", "--profile", "slim", "census::", "--", "--skip", "slow"]);
        assert_eq!(by_nextest("cargo test -p a -p b --release").unwrap().doc.unwrap().packages, ["a", "b"]);
        for kept in [
            "cargo test --lib -j 2",
            "cargo test --lib -j8",
            "cargo test --lib --message-format short",
            "cargo test --lib -q",
            "cargo test --lib --no-run",
            "cargo test --all-targets",
            "cargo test --bench b",
            "cargo test --example e",
            "cargo test --lib -p",
            "cargo test --lib one two",
            "cargo test --lib -- --list",
            "cargo test --lib -- --nocapture",
            "cargo test --lib -- --test-threads 1",
            "cargo test --doc",
            "cargo test --lib | tee x",
            "X=$WSP_SCRATCH cargo test --lib",
            "cargo nextest run -p x",
            "npm test",
        ] {
            assert_eq!(by_nextest(kept), None, "{kept}");
        }
        // A glob `--exclude` stays as written, under cargo test, and says so.
        for glob in [
            "cargo test --workspace --exclude wsp-*",
            "cargo test --workspace --exclude 'wsp-*'",
            "cargo test --workspace --lib --exclude=wsp-?",
            "cd web && cargo test --workspace --exclude \"[ab]\"",
        ] {
            assert_eq!(by_nextest(glob), None, "{glob}");
            let script = runner_script(glob, |_| panic!("no metadata is asked for {glob}"));
            assert_eq!(script, Some(format!("echo '{BY_CARGO_TEST_GLOB}'; {glob}")));
        }
    }

    /// `$PATH` put back when the test that moved it ends, passed or not.
    struct PathBack(std::ffi::OsString);
    impl Drop for PathBack {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.0);
        }
    }

    /// wsp-527's done-when: one check step through [`run_step`], on a host
    /// where `cargo nextest` answers and one where it does not, by a `cargo`
    /// that says what it was asked; the `Ran` records the runner, its line names
    /// it, and both are green. A step with no target selection runs the
    /// doctests of the packages that have them after nextest, and is red when
    /// they fail.
    #[test]
    fn a_check_step_runs_by_nextest_where_the_host_has_it_and_by_cargo_test_where_not_and_its_line_says_which() {
        use std::os::unix::fs::PermissionsExt;
        let _env = util::isolated("check-runner");
        let (dir, log) = scratch_log("runner");
        let dir = dir.canonicalize().unwrap();
        let log = dir.join(log.file_name().unwrap());
        let old = PathBack(std::env::var_os("PATH").unwrap_or_default());
        let cargo = dir.join("cargo");
        // `nextest --version` answers as the marker file says, `metadata` with
        // one library that has doctests, one with them off and one binary;
        // anything else prints its argv. A test beside this one gets the real
        // cargo.
        let fake = format!(
            "#!/bin/sh\ncase \"$(pwd -P)\" in '{dir}'*) ;; *) PATH='{path}' exec cargo \"$@\" ;; esac\n\
             if [ \"$1 $2\" = 'nextest --version' ]; then [ -f has-nextest ]; exit $?; fi\n\
             if [ $1 = metadata ]; then cat metadata.json; exit; fi\necho \"argv: $*\"\n\
             if [ \"$1 $2\" = 'test --doc' ] && [ -f doc-fails ]; then exit 101; fi\n",
            dir = dir.display(),
            path = old.0.to_string_lossy(),
        );
        std::fs::write(&cargo, fake).unwrap();
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let package = |n: &str, doctest: bool| serde_json::json!({"id": n, "name": n, "targets": [{"doctest": doctest}]});
        let meta = serde_json::json!({
            "packages": [package("on", true), package("off", false), package("bin", false)],
            "workspace_members": ["on", "off", "bin"],
            "workspace_default_members": ["off"],
        });
        std::fs::write(dir.join("metadata.json"), meta.to_string()).unwrap();
        std::env::set_var("PATH", format!("{}:{}", dir.display(), old.0.to_string_lossy()));
        let run = |command: &str| -> (String, Ran) {
            let ran = run_step(&dir, &sh(command, 30), &log, &Runner::Inproc);
            (std::fs::read_to_string(&log).unwrap(), ran)
        };
        let line = |ran: &Ran| account(std::slice::from_ref(ran));
        let asked = |said: &str| said.lines().filter_map(|l| l.strip_prefix("argv: ")).map(String::from).collect::<Vec<_>>();
        let command = "cargo test -p on --lib cycle::";
        std::fs::write(dir.join("has-nextest"), "").unwrap();
        let (said, ran) = run(command);
        assert_eq!(asked(&said), ["nextest run --no-tests=pass --ignore-default-filter -p on --lib cycle::"], "{said}");
        assert_eq!((&ran.done, ran.via), (&Done::Passed, Some("cargo nextest run")));
        assert!(line(&ran).starts_with(&format!("✓ s · {command} · by cargo nextest run · ")), "{}", line(&ran));
        // The workspace's doctests: only the library that has them on.
        std::fs::write(dir.join("doc-fails"), "").unwrap();
        let (said, ran) = run("cargo test --workspace");
        assert_eq!(asked(&said), ["nextest run --no-tests=pass --ignore-default-filter --workspace", "test --doc -p on"], "{said}");
        assert_eq!(ran.done, Done::Failed { exit: Some(101), timed_out: false }, "a failing doctest is a failing step");
        assert_eq!(ran.via, Some("cargo nextest run + cargo test --doc"));
        std::fs::remove_file(dir.join("doc-fails")).unwrap();
        // None of what it selects has them: nextest alone.
        for none in ["cargo test", "cargo test -p off -p bin"] {
            let (said, ran) = run(none);
            assert_eq!(asked(&said).len(), 1, "{said}");
            assert_eq!((&ran.done, ran.via), (&Done::Passed, Some("cargo nextest run")), "{none}");
        }
        // A glob is not read, and no metadata is no answer: as written, and said.
        let unread = |command: &str, argv: &str| {
            let (said, ran) = run(command);
            assert_eq!(asked(&said), [argv], "{said}");
            assert_eq!((&ran.done, ran.via), (&Done::Passed, Some("cargo test")), "{said}");
        };
        unread("cargo test --workspace --exclude o*", "test --workspace --exclude o*");
        std::fs::remove_file(dir.join("metadata.json")).unwrap();
        unread("cargo test", "test");
        std::fs::remove_file(dir.join("has-nextest")).unwrap();
        let (said, ran) = run(command);
        assert_eq!(asked(&said), ["test -p on --lib cycle::"], "{said}");
        assert_eq!((&ran.done, ran.via), (&Done::Passed, Some("cargo test")));
        assert!(line(&ran).starts_with(&format!("✓ s · {command} · by cargo test · ")), "{}", line(&ran));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// wsp-969's done-when: a step whose runner ran no test fails and says
    /// which filter, whatever it exited with; one that ran any, or says it may
    /// run none, or is wsp's own derived step, does not.
    #[test]
    fn a_step_whose_test_runner_ran_no_test_fails_naming_the_filter() {
        let _env = util::isolated("check-no-tests");
        let (dir, log) = scratch_log("no-tests");
        let run = |output: &str, exit: u8, tail: &str| {
            std::fs::write(dir.join("out"), output).unwrap();
            run_step(&dir, &sh(&format!("cat out; exit {exit} {tail}"), 30), &log, &Runner::Inproc)
        };
        let empty = [
            ("cargo test", 0, "running 0 tests\n\ntest result: ok. 0 passed; 9 filtered out\nrunning 0 tests\n"),
            ("cargo test", 0, "    Starting 0 tests across 1 binary (9 tests skipped)\n\u{1b}[32;1m     Summary\u{1b}[0m [   0.003s] 0 tests run: 0 passed, 9 skipped\nrunning 0 tests\n"),
            ("cargo test", 4, "     Summary [   0.003s] 0 tests run: 0 passed, 9 skipped\nerror: no tests to run\n"),
            ("vitest", 0, "No test files found, exiting with code 0\n"),
            ("vitest", 1, "No test files found, exiting with code 1\n"),
            ("vitest", 0, " Test Files  1 skipped (1)\n      Tests  3 skipped (3)\n"),
            ("playwright", 1, "Error: No tests found\n"),
        ];
        for (runner, exit, output) in empty {
            let ran = run(output, exit, "");
            let Done::NoTests(why) = &ran.done else { panic!("{output}: {:?}", ran.done) };
            assert!(why.starts_with(&format!("{runner} ran 0 tests: the filter ")), "{why}");
            assert!(ran.done.is_red() && ran.tail.ends_with(why.as_str()), "{}", ran.tail);
            assert!(account(std::slice::from_ref(&ran)).starts_with("✗ s · "), "{}", account(&[ran]));
        }
        for some in [
            "running 0 tests\nrunning 2 tests\nrunning 0 tests\n",
            "     Summary [   1.2s] 3 tests run: 3 passed, 9 skipped\nrunning 0 tests\n",
            "      Tests  2 passed | 1 skipped (3)\n",
            "Running 4 tests using 2 workers\n",
            "Compiling x\n",
        ] {
            assert_eq!(run(some, 0, "").done, Done::Passed, "{some}");
        }
        // Another failure stays itself, and the opt-out is read.
        assert_eq!(run("running 0 tests\n", 1, "").done, Done::Failed { exit: Some(1), timed_out: false });
        assert_eq!(run("running 0 tests\n", 0, MAY_RUN_NO_TESTS).done, Done::Passed);
        let derived = Step { of: Of::Rule, ..sh("echo 'running 0 tests'", 30) };
        assert_eq!(run_step(&dir, &derived, &log, &Runner::Inproc).done, Done::Passed);
        assert!(no_tests_why("cargo test", "cd x && cargo test --bin wsp cycle:: && true").contains("`--bin wsp cycle::`"));
        let wrapped = runner_script(without_opt_out(&format!("cargo test -p on --lib {MAY_RUN_NO_TESTS}")), |_| None).unwrap();
        assert!(wrapped.ends_with("cargo test -p on --lib; fi"), "{wrapped}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_step_the_runner_did_not_wrap_names_no_runner() {
        let dir = std::env::temp_dir().join(format!("wsp-check-via-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        std::fs::write(&log, "Compiling x\n").unwrap();
        assert_eq!(via_of(&log), None);
        assert_eq!(via_of(&dir.join("absent")), None);
        let _ = std::fs::remove_dir_all(&dir);
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

    /// wsp-057: a lock is a lease. Its holder's beat keeps it past the grace
    /// with no job named; a lock nobody beats (its holder killed) ages out
    /// whatever pid it names, and no process is asked.
    #[test]
    fn a_tree_lock_is_held_while_its_holder_beats_it_and_free_once_it_stops() {
        let env = util::isolated("check-lock-lease");
        let path = env.state().join("lock");
        GRACE.with(|g| g.set(Some(Duration::from_millis(400))));
        let held = TreeLock::take(path.clone(), Duration::from_secs(1)).unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        assert!(TreeLock::live(&path), "the beat kept it past the grace");
        assert!(TreeLock::take(path.clone(), Duration::ZERO).is_err());
        drop(held);

        // Left by a holder that was killed: this process's pid, so a pid
        // probe would have kept it, and untouched past the grace.
        std::fs::write(&path, format!("{} {}\n", std::process::id(), util::now_iso())).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(5);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        assert!(!TreeLock::live(&path), "nobody beats it: free");
        let again = TreeLock::take(path.clone(), Duration::ZERO).expect("taken back");
        GRACE.with(|g| g.set(None));
        drop(again);
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
        let r = run_step(&dir, &s, &log, &Runner::Inproc);
        assert!(matches!(r.done, Done::Failed { timed_out: true, .. }), "the label's limit is the step's: {:?}", r.done);
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "only-the-command");
        let s = step("- say it: echo said");
        let r = run_step(&dir, &s, &log, &Runner::Inproc);
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
        let r = run_step(&dir, &s, &log, &Runner::Inproc);
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

    fn scratch_log(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("wsp-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("log"))
    }

    fn sh(command: &str, limit: u64) -> Step {
        Step { name: "s".into(), command: command.into(), limit: Duration::from_secs(limit), of: Of::Project, on: Vec::new(), barrier: false, end: false }
    }

    /// The two choices a step is run by (wsp-053): in this process (`WSP_JOBS=inproc`, a job on `wsp-host`'s runner, wsp-057),
    /// and as jobs (`manager`) on a host whose job runner is the manager's
    /// own (`wsp-host`'s, in this process), its tree root the parent of `dir`
    /// and its jobs' files beside it.
    fn both(dir: &Path) -> Vec<(&'static str, Runner<'static>)> {
        let root = dir.parent().unwrap().to_path_buf();
        wsp_trees::job_place::set_tree_root(Some(root.clone()));
        let exec = root.join(format!("{}-jobs", dir.file_name().unwrap().to_string_lossy()));
        let _ = std::fs::remove_dir_all(&exec);
        let host: &'static crate::host::InProcess = Box::leak(Box::new(crate::host::InProcess::new().with_jobs(&exec, &root)));
        let place = place_of(Whose::Row("pj-001"), TreeKind::Check, dir).unwrap();
        vec![("inproc", Runner::Inproc), ("manager", Runner::Jobs(Jobs::new(host, place, None)))]
    }

    #[test]
    fn exit_zero_passes_and_a_failure_keeps_the_end_of_its_output() {
        let (dir, log) = scratch_log("exit");
        for (how, runner) in both(&dir) {
            let ok = run_step(&dir, &sh("echo fine", 10), &log, &runner);
            assert_eq!(ok.done, Done::Passed, "{how}");
            let bad = run_step(&dir, &sh("echo one; echo two >&2; exit 3", 10), &log, &runner);
            assert_eq!(bad.done, Done::Failed { exit: Some(3), timed_out: false }, "{how}");
            assert!(bad.tail.contains("one") && bad.tail.contains("two"), "{how}: both streams: {}", bad.tail);
        }
    }

    #[test]
    fn a_step_past_its_limit_is_killed_with_everything_it_started() {
        let (dir, log) = scratch_log("limit");
        for (how, runner) in both(&dir) {
            let started = Instant::now();
            let r = run_step(&dir, &sh("sleep 30 & sleep 30", 1), &log, &runner);
            assert!(started.elapsed() < Duration::from_secs(10), "{how}: waited on the sleep");
            assert_eq!(r.done, Done::Failed { exit: None, timed_out: true }, "{how}");
            assert!(why_failed(&r).contains("timed out after 1s"), "{how}");
        }
    }

    #[test]
    fn a_step_at_its_limit_is_retried_at_twice_it_and_a_hang_still_fails() {
        let (dir, log) = scratch_log("tight");
        for (how, runner) in both(&dir) {
            let tight = run_again(&dir, &sh("sleep 1.5", 1), &log, None, &runner);
            assert_eq!(tight.done, Done::Passed, "{how}");
            assert!(account(&[tight]).contains("limit too tight"), "{how}");
            let hung = run_again(&dir, &sh("sleep 30", 1), &log, None, &runner);
            assert_eq!(hung.done, Done::Failed { exit: None, timed_out: true }, "{how}");
        }
    }

    /// A fake `cargo test` in `dir`: on a tree with no `built` mark it prints
    /// `Compiling`, builds for `build`s and says `Finished … in {build}s`;
    /// then it runs for `run`s. A first run stopped in the build leaves no mark.
    fn cold_cargo(dir: &Path, build: u64, run: &str) -> String {
        let _ = std::fs::remove_file(dir.join("built"));
        format!(
            "cd '{}' && if [ ! -f built ]; then echo '   Compiling wsp v0.1.0'; sleep {build}; touch built; \
             echo '    Finished `test` profile [unoptimized + debuginfo] target(s) in {build}.00s'; fi; \
             echo '     Running unittests'; sleep {run}",
            dir.display()
        )
    }

    /// `wsp-364`: a step declared at 3s on a cold check tree whose build takes
    /// 9s, then runs 2.8s, is judged on its run and the row is not sent back.
    #[test]
    fn a_step_stopped_in_a_cold_build_is_judged_on_its_run_and_the_row_is_not_sent_back() {
        let (_env, store, sha) = world("cold-fits", "- s: true", "exit 0\n");
        let (dir, log) = scratch_log("cold-fits");
        let ran = run_again(&dir, &sh(&cold_cargo(&dir, 9, "2.8"), 3), &log, None, &Runner::Inproc);
        assert_eq!(ran.done, Done::Passed, "{}", ran.tail);
        let build = ran.build.expect("the cold build was read off cargo's Finished line");
        assert!(build >= Duration::from_secs(12) && run_took(&ran) < Duration::from_secs(4), "{build:?} {:?}", run_took(&ran));
        let text = account(std::slice::from_ref(&ran));
        assert!(text.contains("built in 12s") && text.contains("while cargo was still building"), "{text}");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        assert_eq!(settle(&store, "pj-001", &at, &[ran], Duration::from_secs(15)), Settled::Passed);
        assert_eq!(store.find_task("pj-001").unwrap().status(), Status::Review, "not sent back");
    }

    /// The same step declared at 1s, with a run of 3s after the build, is past
    /// twice its limit: the row is sent back, and the build is not counted.
    #[test]
    fn a_run_past_twice_its_limit_after_a_cold_build_still_sends_the_row_back() {
        let (_env, store, sha) = world("cold-over", "- s: true", "exit 0\n");
        let (dir, log) = scratch_log("cold-over");
        let ran = run_again(&dir, &sh(&cold_cargo(&dir, 9, "3"), 1), &log, None, &Runner::Inproc);
        assert_eq!(ran.done, Done::Failed { exit: None, timed_out: true }, "{}", ran.tail);
        assert!(why_failed(&ran).contains("ran 3s past twice its declared budget of 1s"), "{}", why_failed(&ran));
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        assert_eq!(settle(&store, "pj-001", &at, &[ran], Duration::from_secs(13)), Settled::SentBack);
        let t = store.find_task("pj-001").unwrap();
        let log = t.section("Log").unwrap();
        assert!(log.contains("declared 1s, measured 3s after a cold build of 10s, which is not counted"), "{log}");
    }

    #[test]
    fn cargos_finished_line_reads_as_its_build_time() {
        let (dir, log) = scratch_log("finished");
        let _ = dir;
        std::fs::write(&log, "   Compiling a v0.1.0\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 05s\n").unwrap();
        assert_eq!(built_in(&log), Some(Duration::from_secs(65)));
        assert!(!still_building(&log), "it finished");
        std::fs::write(&log, "   Compiling a v0.1.0\nwarning: unused doc comment\n").unwrap();
        assert!(still_building(&log));
        std::fs::write(&log, "running 3 tests\n").unwrap();
        assert!(!still_building(&log), "a test run, not a build");
    }

    /// Under `manager` the hang is the manager's to see and end (its
    /// `job.ended` says `hung`); it reads as the same red.
    #[test]
    fn a_hung_job_reads_hung_as_a_hung_step_does() {
        HUNG_AFTER.with(|h| h.set(Some(Duration::from_secs(1))));
        let (dir, log) = scratch_log("hung-job");
        let (_, runner) = both(&dir).pop().unwrap();
        let start = Instant::now();
        let r = run_step(&dir, &sh("sleep 30", 60), &log, &runner);
        HUNG_AFTER.with(|h| h.set(None));
        assert!(start.elapsed() < Duration::from_secs(20), "ended when hung, not at its limit");
        assert!(matches!(r.done, Done::Hung(_)), "{:?}", r.done);
        assert!(why_failed(&r).starts_with("hung") && r.done.is_red(), "{}", r.tail);
    }

    /// Under the default `WSP_JOBS` (`manager`), a host with no manager
    /// record runs here whatever the tree root is, and places nothing; once
    /// this host has a manager, a tree outside the root runs here too, before
    /// the manager is dialled (wsp-346, wsp-361), and only a tree under the
    /// root goes to the manager.
    #[test]
    fn a_host_with_no_manager_stays_in_process_and_one_with_a_manager_runs_a_tree_outside_the_root_here() {
        let env = util::isolated("check-chosen-unmanaged");
        let store = Store::open();
        let root = env.path("root");
        std::fs::create_dir_all(&root).unwrap();
        wsp_trees::job_place::set_tree_root(Some(root.clone()));
        let tree = env.path("trees/m-1");
        let lock = env.path("lock");
        let chosen = |tree: &Path| Runner::chosen(&store, Whose::Row("m-1"), TreeKind::Check, tree, Some(&lock));

        assert!(matches!(chosen(&tree), Ok(Runner::Inproc)), "no manager here: in-process");
        assert!(!tree.exists(), "an in-process run places nothing");
        assert!(matches!(chosen(Path::new("/")), Ok(Runner::Inproc)), "whatever the tree root");

        let local = crate::manager::Local::new(&env.path("exec"), 1);
        crate::manager::write_record(&store.state, &local, "token").unwrap();
        assert!(matches!(chosen(&tree), Ok(Runner::Inproc)), "the in-process host's record names no manager here");
        std::env::set_var(wsp_wire::host_tcp::ENV, "manager");
        let outside = chosen(&tree);
        let under = chosen(&root.join("m-1"));
        std::env::set_var(wsp_wire::host_tcp::ENV, "inproc");
        assert!(matches!(outside, Ok(Runner::Inproc)), "a manager here leaves a tree outside its root in-process");
        assert!(!tree.exists(), "and places nothing");
        assert!(!matches!(under, Ok(Runner::Inproc)), "a tree under the root is the manager's");
        wsp_trees::job_place::set_tree_root(None);
    }

    /// Every job a check sends takes its place from `place_of`: the row's
    /// host and a path relative to that host's tree root, never a path on
    /// wsp's machine. A step is one `job.run`.
    #[test]
    fn a_step_is_one_job_placed_on_the_rows_host_relative_to_its_tree_root() {
        let (dir, log) = scratch_log("placed");
        let (_, runner) = both(&dir).pop().unwrap();
        let Runner::Jobs(jobs) = &runner else { unreachable!() };
        let JobAt::Dir(at) = &jobs.place.at else { panic!("{:?}", jobs.place.at) };
        assert_eq!(at.host.0, host_of(Whose::Row("pj-001")));
        assert_eq!(at.path, dir.file_name().unwrap().to_string_lossy(), "relative to the tree root");
        assert!(!at.escapes() && !at.path.contains(&*dir.to_string_lossy()));
        let run = jobs.run_of(&sh("true", 10), &dir);
        assert_eq!((run.at.clone(), run.class, run.key.row.0.as_str()), (jobs.place.at.clone(), JobClass::Check, "pj-001"));
        assert_eq!(run.argv.last().map(String::as_str), Some("true"));
        assert_ne!(run.key, jobs.run_of(&sh("true", 10), &dir).key, "a key is unique per run");
        let before = jobs.host.jobs().unwrap().len();
        assert_eq!(run_step(&dir, &sh("echo one", 10), &log, &runner).done, Done::Passed);
        let table = jobs.host.jobs().unwrap();
        assert_eq!(table.len(), before + 1, "one job.run for one step");
        assert!(table.iter().all(|r| r.class == JobClass::Check && r.state == JobState::Ended));
        assert!(place_of(Whose::Row("pj-001"), TreeKind::Check, Path::new("/")).is_err(), "a tree outside the root is no place");
    }

    /// A stale lock whose job ended reads free, read by key from the job
    /// table; a running job, a job just ended (the
    /// moment between two steps) and a table that cannot be read keep it.
    #[test]
    fn a_lock_whose_job_ended_reads_free_with_no_kill() {
        let env = util::isolated("check-lock-job");
        let path = env.state().join("lock");
        // Written by hand, so no beat touches it while the test ages it.
        std::fs::write(&path, format!("{} {}\n", std::process::id(), util::now_iso())).unwrap();
        let key = crate::host::job_key("pj-001", 0, "check:s@1.0");
        TreeLock::name_job(&path, "mac", &key);
        assert_eq!(TreeLock::holder(&path).map(|(p, _)| p), Some(std::process::id()), "the holder is kept");
        let row = |state| JobRow { key: key.clone(), class: JobClass::Check, pgid: 1, started: crate::host::At(0), state };
        let table = |rows: Option<Vec<JobRow>>| move |host: &str| -> Option<Vec<JobRow>> {
            assert_eq!(host, "mac", "asked of the tree's host");
            rows.clone()
        };
        assert!(!TreeLock::dead_with(&path, HOLD_GRACE, table(Some(vec![row(JobState::Ended)]))), "just ended: between two steps");
        let old = std::time::SystemTime::now() - Duration::from_secs(600);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        assert!(TreeLock::dead_with(&path, HOLD_GRACE, table(Some(vec![row(JobState::Ended)]))), "its job ended: free");
        assert!(TreeLock::dead_with(&path, HOLD_GRACE, table(Some(Vec::new()))), "its job is gone from the table: free");
        assert!(!TreeLock::dead_with(&path, HOLD_GRACE, table(Some(vec![row(JobState::Running)]))), "running: held");
        assert!(!TreeLock::dead_with(&path, HOLD_GRACE, table(None)), "a table not read keeps it");
        JOB_TABLE.with(|t| *t.borrow_mut() = Some(Some(vec![row(JobState::Ended)])));
        assert!(!TreeLock::live(&path), "TreeLock::live reads the same table");
        JOB_TABLE.with(|t| *t.borrow_mut() = None);
        assert_eq!(job_runs("mac", &key_text(&key)), None, "no manager here: not asked");
        let _ = std::fs::remove_file(&path);
    }

    /// cpd-197's block: a run under `manager` that waits for its missing tree
    /// longer than the grace sends no job, and still holds its lock — a
    /// second take does not get it. A holder that lost its lock neither
    /// writes over the next holder's nor removes it.
    #[test]
    fn a_run_waiting_for_its_tree_past_the_grace_keeps_its_lock() {
        let env = util::isolated("check-lock-wait");
        let (dir, log) = scratch_log("lock-wait");
        let lock = env.state().join("lock");
        let (_, mut manager) = both(&dir).pop().unwrap();
        if let Runner::Jobs(jobs) = &mut manager {
            jobs.lock = Some(lock.clone());
        }
        let held = TreeLock::take(lock.clone(), Duration::from_secs(1)).unwrap();
        assert_eq!(run_step(&dir, &sh("true", 10), &log, &manager).done, Done::Passed, "step 1 names its job in the lock");
        let named = TreeLock::job_of(&lock).expect("the lock names the job");
        GRACE.with(|g| g.set(Some(Duration::from_millis(500))));
        TREE_BACK.with(|t| t.set(Some(Duration::from_secs(2))));
        JOB_TABLE.with(|t| *t.borrow_mut() = Some(Some(Vec::new())));
        std::fs::remove_dir_all(&dir).unwrap();
        let r = run_again(&dir, &sh("true", 10), &log, None, &manager);
        assert!(matches!(r.done, Done::TreeMissing(_)), "{:?}", r.done);
        assert!(r.took >= Duration::from_secs(2), "waited past the grace");
        assert!(TreeLock::take(lock.clone(), Duration::ZERO).is_err(), "a live run's lock read free after its wait");
        assert_eq!(TreeLock::job_of(&lock), Some(named.clone()));

        // Lost to another holder: neither renamed, touched nor removed by this one.
        std::fs::write(&lock, format!("4000000000 {} job:mac:other/0/check:x@1.0\n", util::now_iso())).unwrap();
        TreeLock::name_job(&lock, "mac", &crate::host::job_key("pj-001", 0, "check:s@9.9"));
        assert!(std::fs::read_to_string(&lock).unwrap().contains("other/0/"), "wrote over the next holder's lock");
        drop(held);
        assert!(lock.exists(), "removed the next holder's lock");
        GRACE.with(|g| g.set(None));
        TREE_BACK.with(|t| t.set(None));
        JOB_TABLE.with(|t| *t.borrow_mut() = None);
    }

    /// A check under `manager` leaves the steps, their bounds and the lock to
    /// the manager and its job table; under `inproc` they are `wsp-host`'s
    /// runner's. wsp has no process probe left to make (wsp-057: the lock is
    /// a lease, and `tests/host_seam.rs` fails a spawn outside a job).
    #[test]
    fn a_check_under_manager_probes_no_process_from_wsp() {
        let env = util::isolated("check-no-probes");
        let (dir, log) = scratch_log("no-probes");
        let lock = env.state().join("lock");
        let mut runners = both(&dir);
        let (_, mut manager) = runners.pop().unwrap();
        let (_, inproc) = runners.pop().unwrap();
        if let Runner::Jobs(jobs) = &mut manager {
            jobs.lock = Some(lock.clone());
        }
        let held = TreeLock::take(lock.clone(), Duration::from_secs(1)).unwrap();
        let steps = [sh("echo a", 10), sh("exit 1", 10), sh("sleep 3", 1)];
        let ran = run_every(&dir, &steps, &log, None, &manager, |_| {});
        assert_eq!(ran.iter().map(|r| r.done.is_red()).collect::<Vec<_>>(), [false, true, true], "{:?}", ran.iter().map(|r| &r.done).collect::<Vec<_>>());
        assert!(TreeLock::job_of(&lock).is_some(), "the lock names the job in the tree");
        JOB_TABLE.with(|t| *t.borrow_mut() = Some(Some(Vec::new())));
        assert!(TreeLock::live(&lock), "touched as the last job ended");
        JOB_TABLE.with(|t| *t.borrow_mut() = None);
        drop(held);
        assert_eq!(run_step(&dir, &sh("true", 10), &log, &inproc).done, Done::Passed, "inproc is a job too (wsp-057)");
    }

    /// `wsp-453`: a step that passes still has its group killed — the Vite and
    /// Vitest it started stood for hours after the box check.
    #[test]
    fn a_step_that_passes_leaves_nothing_it_started_running() {
        let (dir, log) = scratch_log("leaves");
        let pidfile = dir.join("server.pid");
        for (how, runner) in both(&dir) {
            let r = run_step(&dir, &sh(&format!("sleep 60 > /dev/null 2>&1 & echo $! > {}", pidfile.display()), 20), &log, &runner);
            assert_eq!(r.done, Done::Passed, "{how}");
            let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
            let alive = || pid.parse().is_ok_and(|pid| crate::host::Host::probe(&crate::host::LOCAL, pid) == Some(true));
            let until = Instant::now() + Duration::from_secs(5);
            while alive() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(!alive(), "{how}: the step's background process {pid} is still running");
        }
    }

    #[test]
    fn a_step_that_fails_once_and_passes_is_passed_and_the_flake_is_kept_on_the_entry() {
        let (dir, log) = scratch_log("flake");
        for (how, runner) in both(&dir) {
            let _ = std::fs::remove_file(dir.join("seen"));
            let flaky = sh("test -f seen || { touch seen; echo first-time >&2; exit 1; }", 10);
            let ran = run_all(&dir, &[flaky], &log, None, &runner, |_| {});
            assert!(passed(&ran), "{how}: the second run counts");
            let text = account(&ran);
            assert!(text.contains("passed on a second run, the first was exit 1") && text.contains("~ first-time"), "{how}: {text}");
            let broken = sh("echo no; exit 2", 10);
            let ran = run_all(&dir, &[broken], &log, None, &runner, |_| {});
            assert!(!passed(&ran), "{how}");
            assert!(account(&ran).contains("exit 2 both times"), "{how}: {}", account(&ran));
        }
    }

    #[test]
    fn steps_stop_at_the_first_failure_and_say_which_were_not_run() {
        let (dir, log) = scratch_log("stop");
        let steps = vec![sh("true", 10), sh("exit 1", 10), sh("true", 10)];
        for (how, runner) in both(&dir) {
            let ran = run_all(&dir, &steps, &log, None, &runner, |_| {});
            let done: Vec<_> = ran.iter().map(|r| r.done.clone()).collect();
            assert_eq!(done[0], Done::Passed, "{how}");
            assert!(matches!(done[1], Done::Failed { .. }), "{how}");
            assert_eq!(done[2], Done::Skipped, "{how}");
            assert!(!passed(&ran), "{how}");
            let text = account(&ran);
            assert!(text.contains("✓ s") && text.contains("✗ s") && text.contains("– s · true · skipped"), "{how}: {text}");
        }
    }

    #[test]
    fn a_check_runs_without_wsps_own_environment() {
        let (dir, log) = scratch_log("env");
        std::env::set_var("WSP_CHECK_PROBE", "set");
        for (how, runner) in both(&dir) {
            let r = run_step(&dir, &sh(r#"test -z "$WSP_CHECK_PROBE""#, 10), &log, &runner);
            assert_eq!(r.done, Done::Passed, "{how}: WSP_* reached the command");
        }
        std::env::remove_var("WSP_CHECK_PROBE");
    }

    // ---- what a row's step leaves in its tree (wsp-848) ----

    /// A row's own tree, made by wsp, with one commit of the member's on it.
    fn member_world(name: &str) -> (util::Isolated, Store, PathBuf) {
        let (env, store, _) = world(name, "- x: true", "exit 0\n");
        let tree = PathBuf::from(crate::cmd_checkout::tree_for(&env.path("repo").display().to_string(), "pj-001").unwrap());
        std::fs::write(tree.join("mine"), "1\n").unwrap();
        git_run(&tree, &["add", "mine"]);
        git_run(&tree, &["commit", "--quiet", "-m", "pj-001: mine"]);
        (env, store, tree)
    }

    fn sent_back(store: &Store) -> String {
        match crate::cmd_checkout::rebase_ready(store, "pj-001") {
            crate::cmd_checkout::Prepared::Refused(why) => why,
            _ => panic!("a dirty tree was not sent back"),
        }
    }

    #[test]
    fn what_a_check_step_created_is_named_as_the_checks_and_not_to_be_committed() {
        let (env, store, tree) = member_world("wrote");
        let step = Step { name: "shots".into(), ..sh("echo a > out.txt && mkdir shots && echo b > shots/one", 10) };
        assert!(passed(&run_all(&tree, &[step], &env.path("log"), None, &Runner::Inproc, |_| {})));
        let said = sent_back(&store);
        assert!(said.contains("written by check step `shots`, not your work: shots/, out.txt"), "{said}");
        assert!(said.contains("do not commit what a check step wrote"), "{said}");
        assert!(!said.contains("commit it") && !said.contains("commit-help"), "{said}");
        let review = crate::cmd_checkout::ready_from(&store, "pj-001", &tree).unwrap_err();
        assert!(review.contains("do not commit") && review.contains("not your work: shots/, out.txt"), "{review}");
        // The member's own edit beside them is still theirs to commit.
        std::fs::write(tree.join("mine"), "2\n").unwrap();
        let said = sent_back(&store);
        assert!(said.contains("no check's — commit what is work (`wsp commit-help`): mine"), "{said}");
    }

    #[test]
    fn a_build_directory_a_step_wrote_is_named_once_and_the_send_back_stays_short() {
        let (env, store, tree) = member_world("build");
        std::fs::create_dir(tree.join("src-tauri")).unwrap();
        std::fs::write(tree.join("src-tauri/lib"), "1\n").unwrap();
        git_run(&tree, &["add", "src-tauri"]);
        git_run(&tree, &["commit", "--quiet", "-m", "pj-001: tauri"]);
        let writes = "for d in OUT src-tauri/target-served; do mkdir -p $d/deps && for i in $(seq 60); do echo > $d/deps/$i; echo > \"$d/a $i\"; done; done";
        let step = Step { name: "baseline".into(), ..sh(writes, 10) };
        assert!(passed(&run_all(&tree, &[step], &env.path("log"), None, &Runner::Inproc, |_| {})));
        let said = sent_back(&store);
        assert!(said.ends_with("written by check step `baseline`, not your work: OUT/, src-tauri/target-served/"), "{said}");
        let first: String = said.chars().take(300).collect();
        assert!(first.contains("do not commit") && first.contains("$WSP_SCRATCH"), "{said}");
        assert!(said.len() < 600, "{said}");

        // A directory with a file of the member's in it is not named whole, and
        // the list under it is capped without crowding that file out.
        std::fs::write(tree.join("OUT/deps/mine"), "1\n").unwrap();
        crate::cmd_checkout::note_check_wrote(&tree, "later", None);
        let said = sent_back(&store);
        assert!(said.contains("(`wsp commit-help`): OUT/deps/mine\n"), "{said}");
        assert!(!said.contains("OUT/,") && !said.contains("OUT/deps/,") && said.contains(" more"), "{said}");
        assert!(said.contains("src-tauri/target-served/") && said.len() < 900, "{said}");
    }

    #[test]
    fn nothing_is_the_checks_where_the_tree_could_not_be_read_before_the_step() {
        let (_env, store, tree) = member_world("unread");
        std::fs::write(tree.join("new"), "new\n").unwrap();
        crate::cmd_checkout::note_check_wrote(&tree, "x", None);
        let said = sent_back(&store);
        assert!(said.contains("commit it (`wsp commit-help`)") && !said.contains("check step"), "{said}");
    }

    #[test]
    fn a_members_own_uncommitted_edit_is_sent_back_to_be_committed() {
        let (env, store, tree) = member_world("own-edit");
        std::fs::write(tree.join("mine"), "2\n").unwrap();
        std::fs::write(tree.join("new"), "new\n").unwrap();
        // A step that ran over them claims neither.
        assert!(passed(&run_all(&tree, &[sh("true", 10)], &env.path("log"), None, &Runner::Inproc, |_| {})));
        let said = sent_back(&store);
        assert!(said.contains("has uncommitted work, so the rebase onto") && said.contains("commit it (`wsp commit-help`)"), "{said}");
        assert!(!said.contains("check step"), "{said}");
    }

    #[test]
    fn a_row_step_sees_wsp_scratch_outside_its_tree() {
        let (env, _store, tree) = member_world("scratch");
        let log = env.path("log");
        let ran = run_step(&tree, &sh(r#"printf %s "$WSP_SCRATCH""#, 10), &log, &Runner::Inproc);
        assert_eq!(ran.done, Done::Passed);
        let seen = PathBuf::from(std::fs::read_to_string(&log).unwrap().trim());
        assert!(seen.is_absolute() && !seen.starts_with(&tree), "{}", seen.display());
        // With none of a seat's own to hand down, as in a verifier's run.
        assert!(!scratch_in(None, &tree).starts_with(&tree));
    }

    // ---- the row ----

    use crate::model::Project;

    fn git_run(dir: &Path, args: &[&str]) {
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
    fn only_an_output_whose_every_failure_is_named_reads_as_whole() {
        let read = |s: &str| {
            let f = failures_in(s);
            (f.tests.len(), f.whole)
        };
        let libtest = "test a::tp1 ... FAILED\ntest result: FAILED. 3 passed; 1 failed; 0 ignored\nerror: test failed, to rerun pass `--lib`\n";
        assert_eq!(read(libtest), (1, true));
        assert_eq!(read("test result: FAILED. 3 passed; 2 failed;\ntest a::tp1 ... FAILED\n"), (1, false), "one failure unnamed");
        assert_eq!(read(&format!("{libtest}error: could not compile `x`\n")), (1, false));
        assert_eq!(read(&format!("{libtest}error: test failed, to rerun pass `--test b`\n")), (1, false), "a binary that crashed");
        let nextest = "        FAIL [   0.012s] (1/2) pj a::tp1\n     Summary [ 1.0s] 2 tests run: 1 passed, 1 failed, 0 skipped\n        FAIL [   0.012s] pj a::tp1\n";
        assert_eq!(read(nextest), (1, true));
        assert_eq!(read(&format!("{nextest}warning: 3/5 tests were not run due to test failure\n")), (1, false));
        let playwright = "  ✘  1 [chromium] › tests/taskpage.spec.ts:99:5 › TP1 (1.2s)\n\n  1 failed\n    [chromium] › tests/taskpage.spec.ts:99:5 › TP1 ───\n  4 passed (9s)\n";
        assert_eq!(read(playwright), (1, true));
        assert_eq!(read(&format!("{playwright}  2 did not run\n")), (1, false));
        let vitest = " FAIL  src/x.test.ts > suite > name\n      Tests  1 failed | 5 passed (6)\n";
        assert_eq!(read(vitest), (1, true));
        assert_eq!(read("error: something else\n"), (0, false), "a red that names no test");

        assert!(names_red("[chromium] › tests/taskpage.spec.ts:99:5 › TP1", "taskpage.spec.ts:99"));
        assert!(!names_red("tests/taskpage.spec.ts:990:5", "taskpage.spec.ts:99"));
        assert!(!names_red("a::tp10", "a::tp1"));
        assert!(one_command("cd web && npx playwright test 2>&1"));
        assert!(!one_command("cargo test -p a && cargo test -p b"));
    }

    #[test]
    fn while_a_trunk_red_is_known_a_cargo_run_goes_past_its_first_failure() {
        let red = [TrunkRed { row: "pj-002".into(), name: "a::tp1".into() }];
        let run = |reds: &[TrunkRed], c: &str| past_failures(reds, &step(&format!("- t: {c}"))).command;
        assert_eq!(run(&red, "cargo test -p a"), "cargo test --no-fail-fast -p a");
        assert_eq!(run(&red, "cd x && cargo nextest run"), "cd x && cargo nextest run --no-fail-fast");
        assert_eq!(run(&red, "npx playwright test"), "npx playwright test");
        assert_eq!(run(&[], "cargo test"), "cargo test");
        let script = runner_script("cargo test --no-fail-fast -p a", |_| Some(r#"{"packages":[{"name":"a","id":"a","targets":[{"doctest":true}]}],"workspace_members":["a"],"workspace_default_members":["a"]}"#.into())).unwrap();
        assert!(script.contains("a=$?; cargo test --doc -p a --no-fail-fast && [ $a -eq 0 ]; }"), "{script}");
    }

    /// `pj-001`'s check, red on `a::tp1`, which the open row `pj-002` declares,
    /// with `pj-001` in a running list.
    fn trunk_red_world(name: &str, script: &str) -> (util::Isolated, Store) {
        let (env, store, _) = world(name, "- script: sh check.sh", script);
        let mut red = Task::new("tp1 is red on the trunk", "pj-002");
        red.project = Some("pj".into());
        red.log("trunk red: `a::tp1`");
        store.save_task(&red).unwrap();
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["pj-001".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();
        (env, store)
    }

    const TP1_RED: &str = "echo 'test a::tp1 ... FAILED'; echo 'test result: FAILED. 3 passed; 1 failed;'; exit 101\n";

    #[test]
    fn a_check_holds_over_a_known_trunk_red_and_pulls_its_row_into_the_running_group() {
        let (_env, store) = trunk_red_world("trunk-held", TP1_RED);
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[])), 0);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.status(), Status::Review, "not sent back");
        assert!(v::runs(&t)[0].text.contains("held over until that row lands"), "{}", v::runs(&t)[0].text);
        assert_eq!(store.worklist("run").unwrap().groups()[0].members, ["pj-002", "pj-001"]);
        assert!(store.find_task("pj-002").unwrap().section("Log").unwrap().contains(crate::cycle::TRUNK_RED_PULLED));

        // The red's own row owes it, and so does everybody once that row is done.
        assert!(known_reds(&store, &store.find_task("pj-002").unwrap()).is_empty());
        let mut red = store.find_task("pj-002").unwrap();
        red.set_status(Status::Done);
        store.save_task(&red).unwrap();
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[("again", "true")])), 1);
    }

    #[test]
    fn a_check_with_another_red_beside_the_known_one_still_fails_and_still_pulls_the_row() {
        let (_env, store) = trunk_red_world(
            "trunk-other",
            "echo 'test a::tp1 ... FAILED'; echo 'test a::mine ... FAILED'; echo 'test result: FAILED. 3 passed; 2 failed;'; exit 101\n",
        );
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[])), 1);
        assert_eq!(store.find_task("pj-001").unwrap().status(), Status::Doing);
        assert_eq!(store.worklist("run").unwrap().groups()[0].members, ["pj-002", "pj-001"]);
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
        let ran = vec![Ran { step: sh("sh check.sh", 10), done: Done::Passed, took: Duration::from_secs(4), tail: String::new(), first: None, reused: None, build: None, via: None }];
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
        let ran = vec![Ran { step: sh("true", 10), done: Done::Passed, took: Duration::ZERO, tail: String::new(), first: None, reused: None, build: None, via: None }];
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
        let ran = vec![Ran { step: sh("true", 10), done: Done::Passed, took: Duration::ZERO, tail: String::new(), first: None, reused: None, build: None, via: None }];
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
            build: None,
            via: None,
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

    #[test]
    fn a_hold_weighs_only_the_reds_that_had_failed_at_the_commit_it_read() {
        let (_env, store, sha) = world("weighed", "- tests: sh check.sh", "exit 7\n");
        let red = |command: &str| {
            vec![Ran {
                step: Step { name: "tests, red from one".into(), command: command.into(), limit: DEFAULT_LIMIT, of: Of::Project, on: Vec::new(), barrier: false, end: false },
                done: Done::Failed { exit: Some(7), timed_out: false },
                took: Duration::from_secs(2),
                tail: String::new(),
                first: None,
                reused: None,
                build: None,
                via: None,
            }]
        };
        let read: String = sha.chars().take(12).collect();
        let landed = "ddde618483b4";
        let mut t = store.find_task("pj-001").unwrap();
        let before = Run { at: "2026-01-01T00:00:00Z".into(), state: RunState::Failed, read: Some(read.clone()), took: Some(2), text: "✗ tests · sh check.sh · 2s · exit 7".into() };
        v::save_runs(&store, &mut t, &[before]).unwrap();
        let mut pass = v::Pass::opened(Some(read), None);
        (pass.state, pass.decided, pass.landed) = (v::State::Holds, Some("2026-01-01T00:05:00Z".into()), Some(landed.into()));
        v::write(&mut t, &[pass]);
        store.save_task(&t).unwrap();

        let at = begin(&store, "pj-001", landed, 3600, false).unwrap();
        assert_eq!(settle(&store, "pj-001", &at, &red("sh check.sh"), Duration::ZERO), Settled::Failed);
        assert_eq!(store.find_task("pj-001").unwrap().status(), Status::Review);
        let at = begin(&store, "pj-001", landed, 3600, true).unwrap();
        assert_eq!(settle(&store, "pj-001", &at, &red("sh other.sh"), Duration::ZERO), Settled::SentBack, "a red the hold did not see");
    }

    fn timed_out() -> Vec<Ran> {
        vec![Ran {
            step: Step { name: "build".into(), command: "cargo build".into(), limit: Duration::from_secs(900), of: Of::Project, on: Vec::new(), barrier: false, end: false },
            done: Done::Failed { exit: None, timed_out: true },
            took: Duration::from_secs(900),
            tail: String::new(),
            first: None,
            reused: None,
            build: None,
            via: None,
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
    fn a_step_declared_at_2s_that_runs_3s_is_sent_back_once_with_both_figures_whatever_the_end_load() {
        let (_env, store, sha) = world("budget", "- build: cargo build", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let mut ran = timed_out();
        ran[0].step.limit = Duration::from_secs(2);
        ran[0].took = Duration::from_secs(3);
        // the load at the end is the step's own: only the start excuses
        assert_eq!(settle_loaded(&store, "pj-001", &at, &ran, Duration::from_secs(3), Some(&at_load(2.0)), Some(30.0)), Settled::SentBack);
        let log = store.find_task("pj-001").unwrap().section("Log").unwrap();
        assert!(log.contains("declared 2s") && log.contains("measured 3s") && !log.contains("under load"), "{log}");
        assert_eq!(v::runs(&store.find_task("pj-001").unwrap()).len(), 1, "no rerun is queued");
    }

    #[test]
    fn a_second_timeout_is_sent_back_even_when_the_load_is_over_the_line_again() {
        let (_env, store, sha) = world("twiceloaded", "- build: cargo build", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        settle_loaded(&store, "pj-001", &at, &timed_out(), Duration::from_secs(900), Some(&at_load(30.0)), Some(30.0));
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        assert_eq!(settle_loaded(&store, "pj-001", &at, &timed_out(), Duration::from_secs(900), Some(&at_load(30.0)), Some(30.0)), Settled::SentBack);
        let log = store.find_task("pj-001").unwrap().section("Log").unwrap();
        assert!(log.contains("declared 900s") && log.contains("measured 900s"), "{log}");
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
        let (dir, log) = scratch_log("malformed");
        for (how, runner) in both(&dir) {
            let broken = Ran { ..run_step(&dir, &sh("cd x &&", 30), &log, &runner) };
            assert!(matches!(broken.done, Done::Malformed(_)), "{how}: {:?}", broken.done);
            let missing = run_step(&dir, &sh("wsp-no-such-command-anywhere", 30), &log, &runner);
            assert!(matches!(missing.done, Done::Malformed(_)), "{how}: a command that is not found is not a red test: {:?}", missing.done);
            let red = run_step(&dir, &sh("exit 3", 30), &log, &runner);
            assert!(matches!(red.done, Done::Failed { exit: Some(3), .. }), "{how}: and a real failure still is one");
        }
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
            build: None,
            via: None,
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
    fn a_step_whose_tree_is_removed_before_it_runs_is_tree_missing_and_the_row_stays_at_review() {
        TREE_BACK.with(|h| h.set(Some(Duration::from_millis(300))));
        let (scratch, log) = scratch_log("tree-missing");
        let tree = scratch.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::remove_dir_all(&tree).unwrap();
        let ran = run_all(&tree, &[sh("true", 10), sh("true", 10)], &log, None, &Runner::Inproc, |_| {});
        assert!(matches!(&ran[0].done, Done::TreeMissing(p) if p.ends_with("tree")), "{:?}", ran[0].done);
        assert_eq!(ran[1].done, Done::Skipped, "the next step has no tree either");
        assert!(!ran[0].done.is_red(), "not a failed step");
        let text = account(&ran);
        assert!(text.contains("tree missing") && !text.contains("no exit status") && !text.contains("No such file"), "{text}");

        let (_env, store, sha) = world("treemissing", "- t: true", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        assert_eq!(settle(&store, "pj-001", &at, &ran, Duration::from_secs(1)), Settled::TreeMissing);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.status(), Status::Review, "the row stays at review");
        let log_text = t.section("Log").unwrap();
        assert!(!log_text.contains("sent back") && log_text.contains("tree missing"), "{log_text}");
        let run = &v::runs(&t)[0];
        assert_eq!(run.state, RunState::Waiting, "owed a rerun, not a verdict");
        assert!(owed(&ran, &sha).contains("nothing was sent back"));
    }

    #[test]
    fn a_tree_that_comes_back_is_run_once_more_and_a_failure_on_a_present_tree_still_fails() {
        TREE_BACK.with(|h| h.set(Some(Duration::from_secs(10))));
        let (scratch, log) = scratch_log("tree-back");
        let tree = scratch.join("tree");
        let _ = std::fs::remove_dir_all(&tree);
        let back = tree.clone();
        let maker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            std::fs::create_dir_all(&back).unwrap();
        });
        let r = run_again(&tree, &sh("touch ran", 10), &log, None, &Runner::Inproc);
        maker.join().unwrap();
        assert_eq!(r.done, Done::Passed, "{}", r.tail);
        assert!(tree.join("ran").exists(), "run once the tree was back");
        let bad = run_again(&tree, &sh("exit 4", 10), &log, None, &Runner::Inproc);
        assert_eq!(bad.done, Done::Failed { exit: Some(4), timed_out: false });
        assert!(why_failed(&bad).contains("exit 4"));
    }

    #[test]
    fn a_failure_on_a_row_still_at_doing_is_recorded_and_nobody_is_moved() {
        let (_env, store, sha) = world("doing", "- t: false", "exit 1\n");
        let mut t = store.find_task("pj-001").unwrap();
        t.set_status(Status::Doing);
        store.save_task(&t).unwrap();
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let ran = vec![Ran { step: sh("false", 10), done: Done::Failed { exit: Some(1), timed_out: false }, took: Duration::ZERO, tail: String::new(), first: None, reused: None, build: None, via: None }];
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

        let ran = vec![Ran { step: sh("true", 10), done: Done::Passed, took: Duration::ZERO, tail: String::new(), first: None, reused: None, build: None, via: None }];
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
    fn a_rows_plan_narrows_a_workspace_step_to_the_crate_its_commit_changed_and_says_why() {
        let (env, store, base) = world("narrow", "", "exit 0\n");
        let repo = env.path("repo");
        let mut t = store.find_task("pj-001").unwrap();
        t.body.push_str("\n## Checks\n- all: cargo test --workspace\n");
        store.save_task(&t).unwrap();
        let t = store.find_task("pj-001").unwrap();
        let at = |sha: &str| Target { since: Some(base.clone()), ..target(&store, &t, Some(sha)).unwrap() };
        let manifests = commit_files(&repo, &[("Cargo.toml", "[package]\nname = \"app\"\n"), ("crates/wire/Cargo.toml", "[package]\nname = \"app-wire\"\n")]);

        // The manifests themselves are build-wide: nothing narrows.
        let got = plan(&store, &t, &at(&manifests), Some(&manifests));
        assert_eq!(got.steps.iter().map(|s| s.command.as_str()).collect::<Vec<_>>(), ["cargo test --workspace"]);
        assert!(got.narrowed.is_empty(), "{:?}", got.narrowed);

        // One crate's source since a base past the manifests: that crate only.
        let since = Target { since: Some(manifests.clone()), ..at(&manifests) };
        let wire = commit_files(&repo, &[("crates/wire/src/lib.rs", "pub fn f() {}\n")]);
        let got = plan(&store, &t, &Target { sha: wire.clone(), ..since }, Some(&wire));
        assert_eq!(got.steps.iter().map(|s| s.command.as_str()).collect::<Vec<_>>(), ["cargo test -p app-wire"]);
        assert_eq!(got.narrowed.len(), 1);
        assert!(got.narrowed[0].contains("only app-wire changed"), "{:?}", got.narrowed);
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
        let first = run_all(&dir, &[step.clone()], &log, Some(&reuse), &Runner::Inproc, |_| {});
        assert!(passed(&first) && first[0].reused.is_none());
        assert!(g.lookup(&sha, &step.command, greens::Env::Warm).is_some());
        let second = run_all(&dir, &[step], &log, Some(&reuse), &Runner::Inproc, |_| {});
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
        assert_eq!(tally_line(&run_all(&dir, &[step.clone()], &log, Some(&reuse), &Runner::Inproc, |_| {})), "0 reused, 1 ran");
        git_run(&dir, &["commit", "--quiet", "--allow-empty", "-m", "a rebase that changed nothing"]);
        let sha2 = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let reuse = Reuse { greens: &g, sha: &sha2, env: greens::Env::Warm, who: "a" };
        let again = run_all(&dir, &[step], &log, Some(&reuse), &Runner::Inproc, |_| {});
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
        run_all(&dir, &[counting(&dir, "one")], &log, Some(&reuse), &Runner::Inproc, |_| {});
        run_all(&dir, &[counting(&dir, "two")], &log, Some(&reuse), &Runner::Inproc, |_| {});
        assert_eq!((runs_of(&dir, "one"), runs_of(&dir, "two")), (1, 1));
        std::fs::write(dir.join("f"), "2").unwrap();
        git_run(&dir, &["commit", "--quiet", "-am", "two"]);
        let sha2 = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let reuse = Reuse { greens: &g, sha: &sha2, env: greens::Env::Warm, who: "a" };
        run_all(&dir, &[counting(&dir, "one")], &log, Some(&reuse), &Runner::Inproc, |_| {});
        assert_eq!(runs_of(&dir, "one"), 2, "a green at the old commit says nothing of the new one");
    }

    #[test]
    fn a_dirty_tree_records_nothing_and_a_failing_step_is_never_recorded() {
        let (_env, store, _) = world("reuse-dirty", "- x: true", "exit 0\n");
        let (dir, log, sha) = repo_at_head("reuse-dirty");
        let g = greens::Greens::open(&store, "pj", &dir);
        let reuse = Reuse { greens: &g, sha: &sha, env: greens::Env::Warm, who: "a" };
        let bad = sh("exit 3", 10);
        assert!(!passed(&run_all(&dir, &[bad.clone()], &log, Some(&reuse), &Runner::Inproc, |_| {})));
        assert!(g.lookup(&sha, &bad.command, greens::Env::Warm).is_none());
        std::fs::write(dir.join("untracked"), "x").unwrap();
        let step = counting(&dir, "count");
        run_all(&dir, &[step.clone()], &log, Some(&reuse), &Runner::Inproc, |_| {});
        assert!(g.lookup(&sha, &step.command, greens::Env::Warm).is_none(), "a dirty tree proved the diff, not the commit");
        std::fs::remove_file(dir.join("untracked")).unwrap();
        std::fs::remove_file(dir.join("count")).unwrap();
        std::fs::write(dir.join(".gitignore"), "count\nlog\n").unwrap();
        git_run(&dir, &["add", ".gitignore"]);
        git_run(&dir, &["commit", "--quiet", "-m", "ignore"]);
        let sha3 = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let reuse = Reuse { greens: &g, sha: &sha3, env: greens::Env::Warm, who: "a" };
        run_all(&dir, &[step.clone()], &log, Some(&reuse), &Runner::Inproc, |_| {});
        assert!(g.lookup(&sha3, &step.command, greens::Env::Warm).is_some(), "ignored files do not make a tree dirty");
    }

    #[test]
    fn a_cold_green_satisfies_a_warm_run_and_a_warm_green_does_not_satisfy_a_cold_one() {
        let (_env, store, _) = world("reuse-env", "- x: true", "exit 0\n");
        let (dir, log, sha) = repo_at_head("reuse-env");
        let g = greens::Greens::open(&store, "pj", &dir);
        let cold = Reuse { greens: &g, sha: &sha, env: greens::Env::Cold, who: "cold" };
        let warm = Reuse { greens: &g, sha: &sha, env: greens::Env::Warm, who: "warm" };
        run_all(&dir, &[counting(&dir, "a")], &log, Some(&cold), &Runner::Inproc, |_| {});
        run_all(&dir, &[counting(&dir, "a")], &log, Some(&warm), &Runner::Inproc, |_| {});
        assert_eq!(runs_of(&dir, "a"), 1, "the cold green answered the warm ask");
        run_all(&dir, &[counting(&dir, "b")], &log, Some(&warm), &Runner::Inproc, |_| {});
        run_all(&dir, &[counting(&dir, "b")], &log, Some(&cold), &Runner::Inproc, |_| {});
        assert_eq!(runs_of(&dir, "b"), 2, "the warm green did not answer the cold ask");
    }

    /// `compound-react-045`: a step's job carries a `$WSP_SCRATCH` that is
    /// there, so a check line naming `$WSP_SCRATCH/target` runs in a verifier's
    /// or the barrier's tree as it ran in the seat.
    #[test]
    fn a_step_has_a_wsp_scratch_it_can_write_in() {
        let (dir, log) = scratch_log("scratch-env");
        let step = sh(r#"test -n "$WSP_SCRATCH" && mkdir -p "$WSP_SCRATCH/target" && echo x > "$WSP_SCRATCH/target/probe""#, 10);
        assert_eq!(run_step(&dir, &step, &log, &Runner::Inproc).done, Done::Passed, "{}", tail_of(&log));
        assert!(scratch_of(&dir).join("target/probe").is_file());
        let tree = Path::new("/s/checks/pj-repo/tree");
        assert_eq!(scratch_in(None, tree), Path::new("/s/checks/pj-repo/tree.scratch"), "beside the tree, kept with it");
        assert_eq!(scratch_in(Some("".into()), tree), Path::new("/s/checks/pj-repo/tree.scratch"));
        assert_eq!(scratch_in(Some("/seat/scratch".into()), tree), Path::new("/seat/scratch"), "the seat's own where there is one");
    }

    /// `compound-react-045`: a check tree with a lockfile and no `node_modules/`
    /// beside it installs one before its steps, as a job, says so, and stamps
    /// it; a lockfile moved on installs again, and one that installs nothing
    /// is said as a red of the tree's.
    #[test]
    fn a_check_tree_without_node_modules_installs_them_and_again_when_the_lockfile_moves() {
        let (dir, log) = scratch_log("install");
        let tree = dir.join("tree");
        std::fs::create_dir_all(tree.join("web")).unwrap();
        std::fs::write(tree.join("web/package-lock.json"), "{1}").unwrap();
        let npm = dir.join("npm");
        std::fs::write(&npm, "#!/bin/sh\nmkdir -p node_modules/vite && echo $(cat package-lock.json) >> ../installs\n").unwrap();
        std::fs::set_permissions(&npm, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let npm = npm.display().to_string();
        let mut start = Start::of(&tree, Vec::new());
        let said = start.install_by(&tree, &log, &Runner::Inproc, &npm);
        assert!(said.len() == 1 && said[0].starts_with("web/node_modules installed by `npm ci` in "), "{said:?}");
        assert_eq!(start.seed, said, "on the record's seed lines");
        assert!(tree.join("web/node_modules/vite").is_dir());
        assert!(start.install_by(&tree, &log, &Runner::Inproc, &npm).is_empty(), "installed for this lockfile: nothing to do");
        std::fs::write(tree.join("web/package-lock.json"), "{2}").unwrap();
        assert_eq!(start.install_by(&tree, &log, &Runner::Inproc, &npm).len(), 1, "the lockfile moved on");
        assert_eq!(std::fs::read_to_string(tree.join("installs")).unwrap(), "{1}\n{2}\n");
        std::fs::remove_dir_all(tree.join("web/node_modules")).unwrap();
        let said = start.install_by(&tree, &log, &Runner::Inproc, "false");
        assert!(said.len() == 1 && said[0].starts_with("web/node_modules not installed: `npm ci` ") && said[0].contains("red for the tree, not the row"), "{said:?}");
    }

    /// `compound-react-045`'s verifier half: a ready row's checks run in the
    /// member's own tree, which is where the verifier's gate checks and the
    /// verifier's seat stands. A tree with a lockfile and no `node_modules/` is
    /// installed in before the steps, so `npm run check` there meets its
    /// packages and not "Cannot find package 'vite'".
    #[test]
    fn a_ready_rows_own_tree_without_node_modules_is_installed_in_before_its_checks() {
        let env = util::isolated("check-own-install");
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        crate::testkit::repo(&repo);
        std::fs::write(repo.join(".gitignore"), "node_modules\n.worktrees\n").unwrap();
        git_run(&repo, &["add", ".gitignore"]);
        git_run(&repo, &["commit", "--quiet", "-m", "trunk"]);
        let own = repo.join(crate::cmd_checkout::WORKTREES).join("pj-001");
        git_run(&repo, &["worktree", "add", "--quiet", "-b", "pj-001", &own.display().to_string()]);
        std::fs::create_dir_all(own.join("web")).unwrap();
        std::fs::write(own.join("web/package-lock.json"), "{1}").unwrap();
        git_run(&own, &["add", "web/package-lock.json"]);
        git_run(&own, &["commit", "--quiet", "-m", "pj-001: a rust-only change"]);
        let sha = git(&own, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let store = Store::open();
        let mut p = Project::new("pj");
        p.roots = vec![repo.display().to_string()];
        p.body = "## Checks\n- vite: test -d web/node_modules/vite\n".into();
        store.save_project(&p).unwrap();
        let mut t = Task::new("a row", "pj-001");
        t.project = Some("pj".into());
        t.set_status(Status::Review);
        crate::log::note(&store, &mut t, &format!("{} {sha} on pj-001", crate::repair::READY)).unwrap();
        assert_eq!(member_tree_at(&store, &store.find_task("pj-001").unwrap(), &sha, false).as_deref(), Some(own.as_path()), "the check runs in the member's own tree");

        let npm = env.path("npm");
        std::fs::write(&npm, "#!/bin/sh\nmkdir -p node_modules/vite\n").unwrap();
        std::fs::set_permissions(&npm, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        TEST_NPM.with(|n| *n.borrow_mut() = Some(npm.display().to_string()));
        let code = check(&store, &Args::synth("check", &["pj-001"], &[]));
        TEST_NPM.with(|n| *n.borrow_mut() = None);
        let runs = v::runs(&store.find_task("pj-001").unwrap());
        assert_eq!(code, 0, "{:?}", runs.iter().map(|r| &r.text).collect::<Vec<_>>());
        assert!(own.join("web/node_modules/vite").is_dir(), "installed in the member's own tree");
        assert!(git(&own, &["status", "--porcelain"]).unwrap().trim().is_empty(), "and the tree is as clean as the member left it");
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
        // Only macOS seeded one to remove; elsewhere there is none to take away.
        let _ = std::fs::remove_dir_all(tree.join("target"));
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
        let ran = run_all(&dir, &[sh("true", 10)], &log, Some(&reuse), &Runner::Inproc, |_| {});
        let text = account(&ran);
        assert!(text.contains("reused (green at this commit, 101s, by wsp-9,"), "{text}");
        assert!(passed(&ran));
    }

    /// wsp-057's done-when: `WSP_JOBS=inproc` still runs a check, end to end,
    /// each step a job on `wsp-host`'s runner in this process (its files under
    /// [`inproc_exec`]) in a tree outside the tree root (d1), and a red step
    /// reads red.
    #[test]
    fn a_check_under_wsp_jobs_inproc_runs_its_steps_as_jobs_in_this_process() {
        let (_env, store, _) = world("inproc-e2e", "- ok: sh check.sh", "exit 0\n");
        let jobs = || std::fs::read_dir(inproc_exec().join("jobs")).map(|d| d.count()).unwrap_or(0);
        let before = jobs();
        std::env::set_var(crate::host_tcp::JOBS_ENV, "inproc");
        let green = check(&store, &Args::synth("check", &["pj-001"], &[]));
        let mut t = store.find_task("pj-001").unwrap();
        t.body = format!("{}\n## Checks\n- bad: echo said-red; exit 3\n", t.body);
        store.save_task(&t).unwrap();
        let red = check(&store, &Args::synth("check", &["pj-001"], &[("again", "true")]));
        std::env::remove_var(crate::host_tcp::JOBS_ENV);
        assert_eq!((green, red), (0, 1), "a green check passes and a red one fails under inproc");
        assert!(jobs() > before, "the steps ran as wsp-host's jobs, not as spawns of wsp's own");
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

    /// `wsp check <id>` under `WSP_JOBS=manager`, on a host whose manager the
    /// install gave a tree root the state directory is not under (wsp-361):
    /// the shared check tree is outside it, and the check runs in this
    /// process rather than "not run".
    #[test]
    fn the_verb_under_a_manager_runs_in_the_shared_check_tree_outside_the_installed_root() {
        let (env, store, _) = world("outside-root", "- script: sh check.sh", "exit 0\n");
        let root = env.path("root");
        std::fs::create_dir_all(&root).unwrap();
        wsp_wire::manager::write_jobs(&store.state, &wsp_wire::manager::Jobs { build: 2, check: 4, tree_root: root.clone() }).unwrap();
        wsp_trees::job_place::set_tree_root(None);
        assert_eq!(wsp_trees::job_place::tree_root(), root, "the installed root");
        let local = crate::manager::Local::new(&env.path("exec"), 1);
        crate::manager::write_record(&store.state, &local, "token").unwrap();
        std::env::set_var(wsp_wire::host_tcp::ENV, "manager");
        let code = check(&store, &Args::synth("check", &["pj-001"], &[]));
        std::env::set_var(wsp_wire::host_tcp::ENV, "inproc");
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(code, 0, "{}", t.body);
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 1, "{}", t.body);
        assert_eq!(runs[0].state, RunState::Passed, "{}", runs[0].text);
        let tree = env.state().join("checks/pj-repo/tree");
        assert!(tree.join("check.sh").exists() && !tree.starts_with(&root), "the shared check tree, outside the root");
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

    /// `wsp-948`: `--at HEAD` from a row's tree one commit ahead of the trunk
    /// is that tree's HEAD, not the root's. The verb prints to stdout, so the
    /// test runs itself again as a child and reads what the child printed.
    #[test]
    fn at_head_from_a_rows_tree_checks_that_trees_head_and_prints_it() {
        const CHILD: &str = "WSP_TEST_AT_HEAD_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let me = "check::tests::at_head_from_a_rows_tree_checks_that_trees_head_and_prints_it";
            let out = wsp_base::calls::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", me, "--nocapture", "--test-threads", "1"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "{said}{}", String::from_utf8_lossy(&out.stderr));
            let head = said.split("tree-head=").nth(1).and_then(|s| s.get(..40)).expect("the child ran");
            let step = said.lines().find(|l| l.contains("script")).unwrap_or_default();
            assert!(step.contains(&format!("at {}", &head[..12])), "{said}");
            return;
        }
        let (env, store, base) = world("at-head", "- script: sh check.sh", "exit 1\n");
        let own = env.path("repo").join(crate::cmd_checkout::WORKTREES).join("pj-001");
        git_run(&env.path("repo"), &["worktree", "add", "--quiet", "-b", "pj-001", &own.display().to_string()]);
        std::fs::write(own.join("check.sh"), "exit 0\n").unwrap();
        git_run(&own, &["commit", "--quiet", "-am", "pj-001: one ahead"]);
        let head = git(&own, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        assert_ne!(head, base);
        let t = store.find_task("pj-001").unwrap();
        let named = |rev: &str| target(&store, &t, Some(rev)).unwrap().sha;
        assert_eq!(named("HEAD"), head, "from elsewhere, the row's tree");
        std::env::set_current_dir(env.path("repo")).unwrap();
        assert_eq!(named("HEAD"), base, "from a tree of the repository, that tree");
        std::env::set_current_dir(&own).unwrap();
        assert_eq!(named("HEAD"), head);
        assert_eq!(named("HEAD~1"), base);
        println!("tree-head={head}");
        // The step is red at the base and green at the tree's HEAD.
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[("at", "HEAD")])), 0);
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

    // ---- wsp-681: what a seat may change of its own checks -----------------

    const BEFORE: &str = "- tests (60s): ls .\n- build (5m): true\n";

    fn set_by(store: &Store, env: &util::Isolated, role: &Setter, row: &str, text: &str, why: Option<&str>) -> i32 {
        let file = env.path("set.md");
        std::fs::write(&file, text).unwrap();
        let from = file.display().to_string();
        let mut flags = vec![("set", "true"), ("from", from.as_str())];
        if let Some(w) = why {
            flags.push(("why", w));
        }
        set_as(store, &Args::synth("check", &[row], &flags), row, USAGE, role)
    }

    fn declared(store: &Store) -> String {
        store.find_task("pj-001").unwrap().section(SECTION).unwrap_or_default()
    }

    /// `wsp-364`: a `--set` with a limit under the step's last measured run is
    /// written, and its answer names the step with both figures.
    #[test]
    fn a_set_under_the_steps_last_measured_run_names_it_in_its_answer() {
        let (env, store, sha) = world("set-under", "", "exit 0\n");
        assert_eq!(set_by(&store, &env, &Setter::Person, "pj-001", "- tests (60s): sh check.sh\n- build (5m): true\n", None), 0);
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let ran = vec![Ran {
            step: Step { name: "tests".into(), command: "sh check.sh".into(), limit: Duration::from_secs(60), of: Of::Project, on: Vec::new(), barrier: false, end: false },
            done: Done::Passed,
            took: Duration::from_secs(4),
            tail: String::new(),
            first: None,
            reused: None,
            build: None,
            via: None,
        }];
        settle(&store, "pj-001", &at, &ran, Duration::from_secs(4));
        let after = "- tests (2s): sh check.sh\n- build (5m): true\n";
        assert_eq!(set_by(&store, &env, &Setter::Person, "pj-001", after, Some("tighter")), 0, "written all the same");
        let said = under_measured(&store.find_task("pj-001").unwrap(), &parse_steps(after, Of::Project));
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("`tests` is declared 2s") && said[0].contains("measured 4s"), "{}", said[0]);
        assert!(under_measured(&store.find_task("pj-001").unwrap(), &parse_steps("- tests (5s): sh check.sh", Of::Project)).is_empty(), "a limit past the run says nothing");
    }

    #[test]
    fn a_member_raises_a_budget_adds_a_cd_and_narrows_a_path_on_its_own_row_and_each_is_logged() {
        let (env, store, _) = world("set-member", "", "exit 0\n");
        let seat = Setter::Member(Some("pj-001".into()));
        assert_eq!(set_by(&store, &env, &seat, "pj-001", BEFORE, None), 0, "its first declaration needs no why");
        let after = "- tests (180s): cd . && ls check.sh\n- build (5m): true\n";
        assert_eq!(set_by(&store, &env, &seat, "pj-001", after, None), 2, "a change says why");
        assert_eq!(declared(&store).trim(), BEFORE.trim(), "nothing was written without one");
        assert_eq!(set_by(&store, &env, &seat, "pj-001", after, Some("vitest took 74s cold")), 0);
        assert_eq!(declared(&store).trim(), after.trim());
        let t = store.find_task("pj-001").unwrap();
        let logged = changes_logged(&t);
        assert_eq!(logged.len(), 1, "{logged:?}");
        let l = &logged[0];
        assert!(l.starts_with("the row's member seat:"), "who: {l}");
        assert!(l.contains("`tests (60s): ls .` → `tests (180s): cd . && ls check.sh`"), "old → new: {l}");
        assert!(l.contains("raises its budget 60s → 180s") && l.contains("adds `cd . &&`") && l.contains("narrows `ls .` to `ls check.sh`"), "{l}");
        assert!(l.ends_with("why: vitest took 74s cold"), "{l}");
        // Adding a step is a member's too.
        let more = format!("{after}- lint (30s): true\n");
        assert_eq!(set_by(&store, &env, &seat, "pj-001", &more, Some("the lint the row asked for")), 0);
        assert!(changes_logged(&store.find_task("pj-001").unwrap())[1].contains("adds `lint`"));
    }

    #[test]
    fn a_member_deleting_a_step_lowering_a_budget_replacing_a_command_or_setting_another_row_is_refused_with_who_may() {
        let (env, store, _) = world("set-refused", "", "exit 0\n");
        let seat = Setter::Member(Some("pj-001".into()));
        assert_eq!(set_by(&store, &env, &Setter::Person, "pj-001", BEFORE, None), 0);
        let why = Some("it passes now");
        for (bad, says) in [
            ("- tests (60s): ls .\n", "`build` is deleted"),
            ("- tests (30s): ls .\n- build (5m): true\n", "lowers its budget 60s → 30s"),
            ("- tests (60s): echo ok\n- build (5m): true\n", "replaces `ls .` with `echo ok`"),
            ("- tests (60s): ls README.md\n- build (5m): true\n", "replaces `ls .` with `ls README.md`"),
        ] {
            assert_eq!(set_by(&store, &env, &seat, "pj-001", bad, why), 2, "{bad}");
            assert_eq!(declared(&store).trim(), BEFORE.trim(), "{bad} was written");
            let e = edits(BEFORE, bad, &touched_by(&store, &store.find_task("pj-001").unwrap()));
            let refused: Vec<String> = e.iter().filter_map(|e| e.refused.clone()).collect();
            assert!(refused.iter().any(|r| r.contains(says)), "{bad}: {refused:?}");
            let no = checks_allowed(&seat, "pj-001", None, Some("pj"), &refused).unwrap_err();
            assert!(no.contains(says) && no.contains("only Ed, or the primary seat may"), "the sentence names who may: {no}");
        }
        let raise = "- tests (90s): ls .\n- build (5m): true\n";
        assert_eq!(set_by(&store, &env, &Setter::Member(Some("pj-002".into())), "pj-001", raise, why), 2, "another row's checks");
        assert_eq!(set_by(&store, &env, &Setter::Member(None), "pj-001", raise, why), 2, "a seat bound to no row");
        assert_eq!(set_by(&store, &env, &seat, "pj", raise, why), 2, "a project's checks");
        assert_eq!(declared(&store).trim(), BEFORE.trim());
        assert!(changes_logged(&store.find_task("pj-001").unwrap()).is_empty(), "a refusal logs no change");
    }

    #[test]
    fn a_governor_deletes_lowers_and_replaces_on_its_own_rows_and_on_no_other() {
        let (env, store, _) = world("set-governor", "", "exit 0\n");
        assert_eq!(set_by(&store, &env, &Setter::Person, "pj-001", BEFORE, None), 0);
        let after = "- tests (30s): echo ok\n";
        let other = Setter::Governor("elsewhere".into());
        assert_eq!(set_by(&store, &env, &other, "pj-001", after, Some("x")), 2, "not its list's row");
        let gov = Setter::Governor("pj".into());
        assert_eq!(set_by(&store, &env, &gov, "pj-001", after, Some("the build step is the barrier's")), 0);
        assert_eq!(declared(&store).trim(), after.trim());
        let logged = changes_logged(&store.find_task("pj-001").unwrap());
        assert_eq!(logged.len(), 2, "{logged:?}");
        assert!(logged.iter().all(|l| l.starts_with("the governor of pj:")), "{logged:?}");
        assert!(logged.iter().any(|l| l.contains("deletes `build`")) && logged.iter().any(|l| l.contains("lowers its budget")), "{logged:?}");
    }

    #[test]
    fn narrowing_is_to_the_files_the_row_touched_and_a_filter_names_their_module() {
        let touched = vec!["src/check.rs".to_string(), "web/src/pane/Pane.test.tsx".to_string(), "crates/trees/src/a.rs".to_string()];
        assert!(narrows("cargo test", "cargo test check::", None, &touched));
        assert!(narrows("cargo test", "cargo test check::tests::x", None, &touched));
        assert!(!narrows("cargo test", "cargo test cmd_role::", None, &touched), "a module the row did not touch");
        assert!(narrows("npx vitest run", "npx vitest run src/pane/Pane.test.tsx", Some("web"), &touched), "relative to its cd");
        assert!(narrows("npx vitest run web/src", "npx vitest run web/src/pane", None, &touched), "a path replaced by a narrower one");
        assert!(!narrows("npx vitest run web/src", "npx vitest run", None, &touched), "a path dropped is wider");
        assert!(narrows("cargo test", "cargo test -p wsp-trees", None, &touched));
        assert!(!narrows("cargo test", "true", None, &touched));
        assert!(!narrows("cargo test", "cargo test --no-fail-fast", None, &touched), "a flag that is no narrowing");
        assert_eq!(split_cd("cd web && npm test"), (Some("web"), "npm test"));
        assert_eq!(split_cd("npm test"), (None, "npm test"));
        let e = edits("- web (60s): cd web && npm test\n", "- web (60s): npm test\n", &touched);
        assert!(e[0].refused.as_deref().is_some_and(|r| r.contains("drops its `cd web &&`")), "{e:?}");
        let e = edits("- web (60s): cd web && npm test\n", "- web (60s): cd spikes/web && npm test\n", &touched);
        assert!(e[0].refused.is_none() && e[0].said.contains("corrects its cd web → spikes/web"), "{e:?}");
        assert!(edits(BEFORE, BEFORE, &touched).is_empty());
    }

    #[test]
    fn check_set_by_role_a_member_its_own_row_within_limits_a_governor_its_lists_rows() {
        let none: &[String] = &[];
        let deleted = vec!["`build` is deleted".to_string()];
        assert!(checks_allowed(&Setter::Person, "r-1", Some("fleet"), None, &deleted).is_ok());
        assert!(checks_allowed(&Setter::Primary, "r-1", Some("fleet"), None, &deleted).is_ok());
        let gov = Setter::Governor("fleet".into());
        assert!(checks_allowed(&gov, "r-1", Some("fleet"), None, &deleted).is_ok(), "a governor deletes on its list's row");
        assert!(checks_allowed(&gov, "r-1", None, Some("fleet"), &deleted).is_ok(), "and on its project's");
        let no = checks_allowed(&gov, "r-1", Some("other"), None, none).unwrap_err();
        assert!(no.contains("on list other") && no.contains("the governor of other, or Ed"), "{no}");
        let own = Setter::Member(Some("r-1".into()));
        assert!(checks_allowed(&own, "r-1", Some("fleet"), None, none).is_ok(), "a member, its own row");
        let no = checks_allowed(&Setter::Member(Some("r-2".into())), "r-1", Some("fleet"), None, none).unwrap_err();
        assert!(no.contains("its own row only (r-2)") && no.contains("the governor of fleet, or Ed"), "{no}");
        let no = checks_allowed(&own, "r-1", Some("fleet"), None, &deleted).unwrap_err();
        assert!(no.contains("`build` is deleted") && no.contains("only the governor of fleet, or Ed may"), "{no}");
        assert!(project_checks_allowed(&own, "pj").is_err());
        assert!(project_checks_allowed(&Setter::Governor("pj".into()), "pj").is_ok());
        assert!(project_checks_allowed(&Setter::Governor("x".into()), "pj").is_err());
        assert!(project_checks_allowed(&Setter::Person, "pj").is_ok());
    }

    #[test]
    fn a_caller_in_no_seat_sets_checks_as_a_person_and_a_spawned_seat_as_a_member_of_its_bound_row() {
        let env = util::isolated("check-setter");
        let _ = &env;
        let store = Store::open();
        store.ensure_dirs().unwrap();
        assert_eq!(setter(&store), Setter::Person);
        std::env::set_var(crate::base::seat::SEAT_ENV, "cpd-77");
        store.set_binding("cpd-77", serde_json::json!({"task_id": "pj-001"}));
        let got = setter(&store);
        std::env::remove_var(crate::base::seat::SEAT_ENV);
        assert_eq!(got, Setter::Member(Some("pj-001".into())));
    }
}
