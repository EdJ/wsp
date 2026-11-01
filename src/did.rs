//! What a verifier did, as lines on its verdict: `wsp-325`.
//!
//! A verdict said the outcome, the commit read and the cost, and nothing about
//! the pass itself, so finding out whether verifiers run the tests, drive the
//! app or read the diff took a hand analysis of 405 transcripts. The entry now
//! carries the answer, in four kinds of line under the verdict:
//!
//! ```text
//!   - **ran:** cargo test check:: · passed
//!   - **ran:** wsp check wsp-324 · failed → passed
//!   - **drove:** ctl · 14 commands
//!   - **read:** src/check.rs:1-200 410-470, src/cycle.rs · diff 9e1c4a..55c4839
//!   - **checked:** the gate holds the verifier until a run passed · held
//!   - **checked:** compound has a declaration · not held
//! ```
//!
//! **`ran`, `drove` and `read` are derived** ([`of_transcript`]) from the
//! seat's transcript when wsp ends it, so no verifier is asked to remember
//! what it did. **`checked` is the verifier's** ([`take_checked`]): which
//! done-when item it looked at and whether it held, which no transcript can
//! say. The presence of a `ran` line is what says the transcript was read: a
//! verifier that ran nothing writes `ran: none`, and an entry with no `ran`
//! line is one that was never read, which the report counts apart rather than
//! as a verifier that did nothing.
//!
//! # Project-agnostic
//!
//! Nothing here knows a project. A command is *inspection* when it is on the
//! short list of programs that only look ([`INSPECT`], `git show`, `wsp show`)
//! and *ran* otherwise, so a project's own script or test runner counts without
//! being named. What is named is conventions that cross projects: the test
//! runners ([`is_test`]), and what counts as driving a running thing
//! ([`drove_by`], [`driving_server`]). Those are the tables to extend.
//!
//! # What it cannot see
//!
//! Only Claude Code's transcript is read; another kind's entry has no lines. A
//! pipeline's exit status is its last stage's, so a test run through `| grep`
//! is judged by the output as well ([`failed_output`]). A test the verifier
//! reached through a script is a command that ran, not a test that ran.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

/// How a command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Passed,
    Failed,
    /// No result came back: the transcript ends first, or it ran in the background.
    Unfinished,
}

impl Outcome {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Unfinished => "unfinished",
        }
    }

    fn of_word(w: &str) -> Option<Outcome> {
        Some(match w {
            "passed" => Outcome::Passed,
            "failed" => Outcome::Failed,
            "unfinished" => Outcome::Unfinished,
            _ => return None,
        })
    }
}

/// One command, however many times it was run: the outcomes in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Command {
    pub text: String,
    pub outcomes: Vec<Outcome>,
}

impl Command {
    /// The last word on it: a test that failed and then passed passed.
    pub(crate) fn last(&self) -> Outcome {
        self.outcomes.last().copied().unwrap_or(Outcome::Unfinished)
    }

    pub(crate) fn is_test(&self) -> bool {
        is_test(&self.text)
    }

    /// `failed → passed ×2`: runs of the same outcome folded.
    fn outcomes_text(&self) -> String {
        let mut runs: Vec<(Outcome, usize)> = Vec::new();
        for o in &self.outcomes {
            match runs.last_mut() {
                Some((last, n)) if last == o => *n += 1,
                _ => runs.push((*o, 1)),
            }
        }
        runs.iter()
            .map(|(o, n)| if *n > 1 { format!("{} ×{n}", o.word()) } else { o.word().to_string() })
            .collect::<Vec<_>>()
            .join(" → ")
    }

    fn parse(body: &str) -> Option<Command> {
        let (text, outcomes) = body.rsplit_once(" · ")?;
        let mut out = Vec::new();
        for step in outcomes.split(" → ") {
            let (word, n) = match step.split_once(" ×") {
                Some((w, n)) => (w, n.trim().parse::<usize>().ok()?),
                None => (step, 1),
            };
            let o = Outcome::of_word(word.trim())?;
            out.extend(std::iter::repeat(o).take(n));
        }
        Some(Command { text: text.trim().to_string(), outcomes: out })
    }
}

/// A file looked at, and which lines of it when not all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileRead {
    pub path: String,
    /// Merged, ascending. Empty means the whole file.
    pub lines: Vec<(u64, u64)>,
}

impl FileRead {
    pub(crate) fn text(&self) -> String {
        if self.lines.is_empty() {
            return self.path.clone();
        }
        if self.lines == [PARTIAL] {
            return format!("{}:~", self.path);
        }
        let ranges: Vec<String> = self.lines.iter().map(|(a, b)| if *b == u64::MAX { format!("{a}-") } else { format!("{a}-{b}") }).collect();
        format!("{}:{}", self.path, ranges.join(" "))
    }

    fn parse(s: &str) -> FileRead {
        let whole = |s: &str| FileRead { path: s.to_string(), lines: Vec::new() };
        let s = s.trim();
        let Some((path, rest)) = s.rsplit_once(':') else { return whole(s) };
        if rest == "~" {
            return FileRead { path: path.to_string(), lines: vec![PARTIAL] };
        }
        let mut lines = Vec::new();
        for r in rest.split(' ') {
            let Some((a, b)) = r.split_once('-') else { return whole(s) };
            let Ok(a) = a.parse::<u64>() else { return whole(s) };
            let b = if b.is_empty() {
                u64::MAX
            } else {
                match b.parse::<u64>() {
                    Ok(b) => b,
                    Err(_) => return whole(s),
                }
            };
            lines.push((a, b));
        }
        FileRead { path: path.to_string(), lines }
    }
}

/// What was read: files, and the diffs and commits looked at.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Reading {
    pub files: Vec<FileRead>,
    pub diffs: Vec<String>,
    /// Files and diffs the line leaves out to stay one line.
    pub more_files: usize,
    pub more_diffs: usize,
}

/// The mark in a file's ranges, while reading, for "all of it".
const WHOLE: (u64, u64) = (0, 0);

/// The mark for "some of it, and which is not known": a `sed` addressed by a
/// pattern or by a substitution. Shown `file:~`; a known range outranks it.
const PARTIAL: (u64, u64) = (u64::MAX, u64::MAX);

impl Reading {
    pub(crate) fn file_count(&self) -> usize {
        self.files.len() + self.more_files
    }

    pub(crate) fn diff_count(&self) -> usize {
        self.diffs.len() + self.more_diffs
    }

    fn add_file(&mut self, path: &str, range: Option<(u64, u64)>) {
        if is_scratch(path) {
            return;
        }
        let at = match self.files.iter().position(|f| f.path == path) {
            Some(i) => i,
            None => {
                self.files.push(FileRead { path: path.to_string(), lines: Vec::new() });
                self.files.len() - 1
            }
        };
        let f = &mut self.files[at];
        if f.lines.first() == Some(&WHOLE) {
            return;
        }
        match range {
            None => f.lines = vec![WHOLE],
            Some(r) => f.lines.push(r),
        }
    }

    /// Merge the ranges and turn the whole-file mark into no ranges.
    fn settle(&mut self) {
        for f in &mut self.files {
            if f.lines.first() == Some(&WHOLE) {
                f.lines.clear();
                continue;
            }
            if f.lines.iter().any(|r| *r != PARTIAL) {
                f.lines.retain(|r| *r != PARTIAL);
            } else {
                f.lines.dedup();
                continue;
            }
            f.lines.sort();
            let mut merged: Vec<(u64, u64)> = Vec::new();
            for (a, b) in f.lines.drain(..) {
                match merged.last_mut() {
                    Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
                    _ => merged.push((a, b)),
                }
            }
            f.lines = merged;
        }
    }

    fn is_empty(&self) -> bool {
        self.file_count() == 0 && self.diff_count() == 0
    }
}

/// What a verifier did, derived: the commands, the driving, the reading.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Did {
    pub ran: Vec<Command>,
    /// Commands past the ones listed.
    pub more_ran: usize,
    /// `kind · what`: `ctl · 14 commands`, `mcp claude-in-chrome · 3 calls`.
    pub drove: Vec<String>,
    pub read: Reading,
}

/// One of the verifier's own: a done-when item it looked at, and whether it held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Checked {
    pub item: String,
    pub held: bool,
}

const MAX_RAN: usize = 30;
const MAX_READ: usize = 25;
const MAX_DIFFS: usize = 10;
const MAX_TEXT: usize = 110;

/// The line kinds, as they sit after `- **` in an entry.
pub(crate) const KEYS: [&str; 4] = ["ran", "drove", "read", "checked"];

impl Did {
    /// The lines of an entry: all three kinds always, so that an entry whose
    /// transcript was read says so even when the verifier did nothing.
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.ran.is_empty() && self.more_ran == 0 {
            out.push(line("ran", "none"));
        }
        for c in &self.ran {
            out.push(line("ran", &format!("{} · {}", c.text, c.outcomes_text())));
        }
        if self.more_ran > 0 {
            out.push(line("ran", &format!("({} more not listed)", self.more_ran)));
        }
        if self.drove.is_empty() {
            out.push(line("drove", "none"));
        }
        for d in &self.drove {
            out.push(line("drove", d));
        }
        out.push(line("read", &self.read_text()));
        out
    }

    fn read_text(&self) -> String {
        if self.read.is_empty() {
            return "none".into();
        }
        let mut files: Vec<String> = self.read.files.iter().map(FileRead::text).collect();
        if self.read.more_files > 0 {
            files.push(format!("+{} more", self.read.more_files));
        }
        let mut out = files.join(", ");
        if self.read.diff_count() > 0 {
            let mut diffs = self.read.diffs.clone();
            if self.read.more_diffs > 0 {
                diffs.push(format!("+{} more", self.read.more_diffs));
            }
            if !out.is_empty() {
                out.push_str(" · ");
            }
            out.push_str(&format!("diff {}", diffs.join(", ")));
        }
        out
    }

    /// Take one entry line back in.
    pub(crate) fn absorb(&mut self, key: &str, body: &str) {
        match key {
            "ran" if body == "none" => {}
            "ran" => {
                match body.strip_prefix('(').and_then(|b| b.strip_suffix(" more not listed)")).and_then(|n| n.parse().ok()) {
                    Some(n) => self.more_ran = n,
                    None => self.ran.extend(Command::parse(body)),
                }
            }
            "drove" if body == "none" => {}
            "drove" => self.drove.push(body.to_string()),
            "read" if body == "none" => {}
            "read" => {
                let (files, diffs) = match body.split_once(" · diff ") {
                    Some((f, d)) => (f, Some(d)),
                    None => match body.strip_prefix("diff ") {
                        Some(d) => ("", Some(d)),
                        None => (body, None),
                    },
                };
                for f in files.split(", ").filter(|f| !f.is_empty()) {
                    match more_of(f) {
                        Some(n) => self.read.more_files = n,
                        None => self.read.files.push(FileRead::parse(f)),
                    }
                }
                for d in diffs.into_iter().flat_map(|d| d.split(", ")) {
                    match more_of(d) {
                        Some(n) => self.read.more_diffs = n,
                        None => self.read.diffs.push(d.to_string()),
                    }
                }
            }
            _ => {}
        }
    }

    /// The commands that were test runs.
    pub(crate) fn tests(&self) -> Vec<&Command> {
        self.ran.iter().filter(|c| c.is_test()).collect()
    }

    pub(crate) fn drove(&self) -> bool {
        !self.drove.is_empty()
    }
}

/// `+12 more`.
fn more_of(s: &str) -> Option<usize> {
    s.strip_prefix('+')?.strip_suffix(" more")?.parse().ok()
}

pub(crate) fn line(key: &str, body: &str) -> String {
    format!("- **{key}:** {body}")
}

/// `- **ran:** …` as `("ran", "…")`.
pub(crate) fn split_line(l: &str) -> Option<(&'static str, &str)> {
    let rest = l.trim_end().strip_prefix("- **")?;
    let (key, body) = rest.split_once(":** ")?;
    let key = KEYS.iter().find(|k| **k == key)?;
    Some((key, body))
}

impl Checked {
    pub(crate) fn text(&self) -> String {
        format!("{} · {}", self.item, if self.held { "held" } else { "not held" })
    }

    pub(crate) fn parse(body: &str) -> Option<Checked> {
        let (item, word) = body.rsplit_once(" · ")?;
        let held = match word.trim() {
            "held" => true,
            "not held" => false,
            _ => return None,
        };
        Some(Checked { item: item.trim().to_string(), held })
    }
}

/// The verifier's `checked:` lines taken out of its verdict: the verdict
/// without them, and each as a [`Checked`].
///
/// Written the way an agent writes a line — `checked: the gate holds the
/// verifier — held`, with or without a bullet or bold — and refused, naming the
/// line, when it does not end in `held` or `not held`: a line nobody can count
/// is worse than none.
pub(crate) fn take_checked(text: &str) -> Result<(String, Vec<Checked>), String> {
    let mut kept: Vec<&str> = Vec::new();
    let mut checked = Vec::new();
    for l in text.lines() {
        let Some(rest) = checked_rest(l) else {
            kept.push(l);
            continue;
        };
        let rest = rest.trim().trim_end_matches(['.', ')', '*', ' ']);
        let (item, held) = if let Some(i) = rest.strip_suffix("not held") {
            (i, false)
        } else if let Some(i) = rest.strip_suffix("held") {
            (i, true)
        } else {
            return Err(format!(
                "a `checked:` line ends in `held` or `not held`, and this one does not: {}",
                crate::util::truncate(l.trim(), 100)
            ));
        };
        let item = item.trim_end_matches(['—', '–', '-', '·', ':', '(', ',', ' ']).trim();
        if item.is_empty() {
            return Err(format!(
                "a `checked:` line names the done-when item it checked before `held`: {}",
                crate::util::truncate(l.trim(), 100)
            ));
        }
        checked.push(Checked { item: crate::cmd_task::fold(item), held });
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    Ok((kept.join("\n"), checked))
}

/// What follows `checked:` on a line written as an agent writes one.
fn checked_rest(l: &str) -> Option<&str> {
    let t = l.trim_start();
    let t = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).unwrap_or(t).trim_start();
    let t = t.strip_prefix("**").unwrap_or(t);
    if !t.get(..8)?.eq_ignore_ascii_case("checked:") {
        return None;
    }
    let t = &t[8..];
    Some(t.strip_prefix("**").unwrap_or(t))
}

/// Whether the last line of a verdict would be read back as one of ours.
pub(crate) fn ends_like_ours(text: &str) -> bool {
    text.lines().last().is_some_and(|l| split_line(l.trim()).is_some())
}

// ---- the transcript ---------------------------------------------------------

/// One transcript, reduced to what the seat did. `None` for one with no
/// assistant turn at all.
///
/// The same filters [`crate::agent_commands`]' tally uses, for the same
/// reasons: a sidechain is another agent, a line before `since` belongs to an
/// earlier attempt of the same session, and a tool call written twice (a fork
/// replays what came before it) is one call, keyed by its id.
pub(crate) fn of_transcript(src: &mut impl std::io::BufRead, since: i64) -> Option<Did> {
    let mut calls: Vec<Call> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    // Where the session started: every path is named from here, because the
    // shell keeps its directory between calls and a line's `cwd` moves with it.
    let mut root = String::new();
    let mut saw = false;
    let mut buf = String::new();
    while {
        buf.clear();
        src.read_line(&mut buf).unwrap_or(0) > 0
    } {
        saw = saw || buf.contains("\"assistant\"");
        if !buf.contains("tool_use") && !buf.contains("tool_result") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&buf) else { continue };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let cwd = v.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
        if root.is_empty() {
            root = cwd.clone();
        }
        let stamp = v.get("timestamp").and_then(Value::as_str).map(crate::util::epoch_of).unwrap_or(0);
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else { continue };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    if since > 0 && stamp > 0 && stamp < since {
                        continue;
                    }
                    let Some(id) = b.get("id").and_then(Value::as_str) else { continue };
                    if at.contains_key(id) {
                        continue;
                    }
                    at.insert(id.to_string(), calls.len());
                    calls.push(Call {
                        tool: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        input: b.get("input").cloned().unwrap_or(Value::Null),
                        cwd: cwd.clone(),
                        output: None,
                    });
                }
                Some("tool_result") => {
                    let Some(i) = b.get("tool_use_id").and_then(Value::as_str).and_then(|id| at.get(id)) else { continue };
                    if calls[*i].output.is_some() {
                        continue;
                    }
                    let failed = b.get("is_error").and_then(Value::as_bool) == Some(true);
                    calls[*i].output = Some((failed, result_text(b.get("content"))));
                }
                _ => {}
            }
        }
    }
    (saw || !calls.is_empty()).then(|| did_of(&calls, &root))
}

struct Call {
    tool: String,
    input: Value,
    cwd: String,
    /// `(the tool reported an error, what came back)`; `None` while no result came.
    output: Option<(bool, String)>,
}

fn result_text(c: Option<&Value>) -> String {
    match c {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn did_of(calls: &[Call], root: &str) -> Did {
    let mut did = Did::default();
    let mut order: Vec<String> = Vec::new();
    let mut ran: BTreeMap<String, Vec<Outcome>> = BTreeMap::new();
    let mut drove: BTreeMap<String, usize> = BTreeMap::new();
    for c in calls {
        match c.tool.as_str() {
            "Read" => {
                let Some(p) = c.input.get("file_path").and_then(Value::as_str) else { continue };
                let from = c.input.get("offset").and_then(Value::as_u64);
                let n = c.input.get("limit").and_then(Value::as_u64);
                let range = match (from, n) {
                    (Some(a), Some(n)) => Some((a, a + n.max(1) - 1)),
                    (Some(a), None) => Some((a, u64::MAX)),
                    (None, Some(n)) => Some((1, n)),
                    (None, None) => None,
                };
                did.read.add_file(&relative(p, root), range);
            }
            "Bash" | "bash" => {
                let Some(cmd) = c.input.get("command").and_then(Value::as_str) else { continue };
                let background = c.input.get("run_in_background").and_then(Value::as_bool) == Some(true);
                let seen = bash(cmd, &c.cwd);
                for (path, range) in &seen.reads {
                    did.read.add_file(&relative(path, root), *range);
                }
                for d in seen.diffs {
                    if !did.read.diffs.contains(&d) {
                        did.read.diffs.push(d);
                    }
                }
                for k in seen.drove {
                    *drove.entry(k).or_default() += 1;
                }
                if !seen.ran.is_empty() {
                    let text = crate::util::truncate(&seen.ran.join(" && "), MAX_TEXT);
                    let outcome = match (&c.output, background) {
                        (_, true) | (None, _) => Outcome::Unfinished,
                        (Some((err, out)), _) if *err || not_run(out) || (seen.test && failed_output(out)) => Outcome::Failed,
                        _ => Outcome::Passed,
                    };
                    if !ran.contains_key(&text) {
                        order.push(text.clone());
                    }
                    ran.entry(text).or_default().push(outcome);
                }
            }
            t if t.starts_with("mcp__") => {
                if let Some(server) = driving_server(t) {
                    *drove.entry(format!("mcp {server}")).or_default() += 1;
                }
            }
            _ => {}
        }
    }
    did.read.settle();
    for text in order {
        let outcomes = ran.remove(&text).unwrap_or_default();
        if did.ran.len() < MAX_RAN {
            did.ran.push(Command { text, outcomes });
        } else {
            did.more_ran += 1;
        }
    }
    for (kind, n) in drove {
        let what = if kind.starts_with("mcp ") { "call" } else { "command" };
        did.drove.push(format!("{kind} · {n} {what}{}", if n == 1 { "" } else { "s" }));
    }
    if did.read.files.len() > MAX_READ {
        did.read.more_files = did.read.files.len() - MAX_READ;
        did.read.files.truncate(MAX_READ);
    }
    if did.read.diffs.len() > MAX_DIFFS {
        did.read.more_diffs = did.read.diffs.len() - MAX_DIFFS;
        did.read.diffs.truncate(MAX_DIFFS);
    }
    did
}

/// A path as typed in `here`, made absolute: `../x` and `./x` settled. A home
/// directory stays `~`, which is not ours to expand.
fn at(path: &str, here: &str) -> String {
    if path.starts_with('/') || path.starts_with('~') || here.is_empty() {
        return path.to_string();
    }
    let (lead, rest) = match here.strip_prefix('~') {
        Some(r) => ("~", r),
        None => ("", here),
    };
    let mut parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("{lead}/{}", parts.join("/"))
}

/// A path that is the session's own working paper — a persisted tool output, a
/// temp file, the verdict being written — and not part of the work.
fn is_scratch(path: &str) -> bool {
    ["/tmp/", "/private/tmp/", "/var/folders/", "/.claude/projects/", "/scratchpad/"].iter().any(|d| path.contains(d))
}

/// The path as the seat's own tree spells it: under its cwd, relative to it.
fn relative(p: &str, cwd: &str) -> String {
    match p.strip_prefix(cwd) {
        Some(r) if !cwd.is_empty() && r.starts_with('/') => r[1..].to_string(),
        _ => p.to_string(),
    }
}

/// Output that says a test run failed whatever its exit status was. A pipeline
/// reports its last stage, so `cargo test | grep` is green about a red suite.
pub(crate) fn failed_output(out: &str) -> bool {
    out.lines().any(|l| {
        let t = l.trim();
        t.starts_with("test result: FAILED")
            || t.starts_with("error: could not compile")
            || t.starts_with("FAIL ")
            || t.starts_with("--- FAIL")
            || (t.starts_with("test ") && t.ends_with("FAILED"))
            || (has_count_before(t, "failed") && (t.contains("passed") || t.contains(" failed in ")))
            || (has_count_before(t, "failing") && t.contains("passing"))
    })
}

/// A shell that could not start the command says so, whatever a pipe after it
/// reports.
fn not_run(out: &str) -> bool {
    out.lines().any(|l| {
        let l = l.trim_end();
        l.ends_with(": command not found") || ((l.starts_with("(eval):") || l.starts_with("zsh:")) && l.contains("command not found"))
    })
}

/// `3 failed`, and not `0 failed`.
fn has_count_before(t: &str, word: &str) -> bool {
    let w: Vec<&str> = t.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    w.windows(2).any(|p| p[1] == word && p[0].parse::<u64>().is_ok_and(|n| n > 0))
}

/// The MCP server a tool name belongs to, when it drives something: a browser,
/// a desktop, a simulator.
fn driving_server(tool: &str) -> Option<String> {
    let server = tool.strip_prefix("mcp__")?.split("__").next()?.to_string();
    let l = server.to_ascii_lowercase();
    const DRIVES: [&str; 8] = ["browser", "chrome", "playwright", "puppeteer", "computer", "simulator", "screen", "desktop"];
    DRIVES.iter().any(|d| l.contains(d)).then_some(server)
}

// ---- a shell command --------------------------------------------------------

#[derive(Default, Debug)]
struct Seen {
    /// The commands that did something, as they would be typed.
    ran: Vec<String>,
    /// Whether any of them was a test run.
    test: bool,
    reads: Vec<(String, Option<(u64, u64)>)>,
    diffs: Vec<String>,
    drove: Vec<String>,
}

/// Programs that only look, or only move the shell about: not a thing the
/// verifier ran. The other half of the rule is [`inspects`], for the programs
/// that look in some forms and act in others.
const INSPECT: &[&str] = &[
    "ls", "grep", "egrep", "fgrep", "rg", "ag", "find", "fd", "wc", "echo", "printf", "cut", "sort", "uniq", "tr", "tee", "awk", "xargs", "jq", "diff", "cmp",
    "uptime", "sysctl", "date", "pwd", "cd", "pushd", "popd", "sleep", "which", "whereis", "type", "test", "[", "[[", "true", "false", "export", "set", "unset",
    "source", ".", "alias", "mkdir", "rmdir", "touch", "rm", "mv", "cp", "ln", "chmod", "stat", "file", "basename", "dirname", "realpath", "readlink", "ps",
    "pgrep", "pkill", "kill", "killall", "lsof", "top", "df", "du", "id", "whoami", "hostname", "uname", "env", "printenv", "launchctl", "tput", "clear", ":",
    "open", "wait", "read", "return", "exit", "head", "tail", "cat", "sed", "nl", "bat", "less", "more", "git", "trap", "ulimit", "umask", "hash", "history",
    "column", "paste", "od", "xxd", "hexdump", "strings", "shasum", "md5", "md5sum", "sha256sum", "base64", "seq", "yes", "tty", "stty", "mktemp", "tar", "unzip",
    "gunzip", "zcat", "curl", "continue", "break", "local", "declare", "typeset", "let", "shift", "getopts", "comm", "join", "nproc", "sw_vers",
];

/// `wsp` subcommands that read or speak and change nothing the verifier is
/// answerable for.
const WSP_LOOKS: &[&str] = &[
    "show", "cat", "ls", "list", "help", "wip", "brief", "version", "yours", "attempts", "cost", "burn", "doctor", "say", "note", "verified", "flag", "ask",
    "commit-help", "stage", "inbox", "next", "log", "history", "verifications", "--version", "-h", "--help",
];

fn inspects(words: &[String]) -> bool {
    let name = base(&words[0]);
    if words.iter().skip(1).any(|w| matches!(w.as_str(), "--help" | "-h" | "--version" | "-V")) || (name == "wsp" && words.iter().any(|w| w == "--status")) {
        return true;
    }
    match name.as_str() {
        "wsp" => {
            let mut rest = words.iter().skip(1).filter(|w| !w.starts_with('-')).map(String::as_str);
            let sub = rest.next().unwrap_or("help");
            match sub {
                "worklist" | "project" => matches!(rest.next().unwrap_or("show"), "show" | "ls" | "list" | "log" | "help" | "next"),
                s => WSP_LOOKS.contains(&s),
            }
        }
        "cargo" => {
            let sub = words.iter().skip(1).find(|w| !w.starts_with('-') && !w.starts_with('+')).map(String::as_str).unwrap_or("");
            matches!(sub, "metadata" | "tree" | "version" | "locate-project" | "help" | "search" | "")
        }
        n => INSPECT.contains(&n),
    }
}

fn base(w: &str) -> String {
    w.rsplit('/').next().unwrap_or(w).to_string()
}

fn bash(cmd: &str, cwd: &str) -> Seen {
    let mut seen = Seen::default();
    // Where a `cd` in this command has taken it.
    let mut here = cwd.to_string();
    for s in simple_commands(cmd) {
        let piped = s.piped;
        let words = strip_prefixes(s.words);
        if words.is_empty() {
            continue;
        }
        let name = base(&words[0]);
        // What is left of a `xargs -n 10` or a loop's header is not a program.
        if name.starts_with('-') {
            continue;
        }
        if name == "cd" {
            if let Some(to) = words.get(1).filter(|t| !t.contains('$') && !t.starts_with('-') && t.as_str() != "~") {
                // The shell keeps its directory between calls, so a `cd` the
                // verifier typed from the root can fail from where it is — and
                // `cat` after it reads the right file regardless.
                if to.starts_with('/') || !(here == *to || here.ends_with(&format!("/{to}"))) {
                    here = at(to, &here);
                }
            }
            continue;
        }
        reads_of(&name, &words, s.writes, &here, &mut seen);
        if inspects(&words) {
            continue;
        }
        if let Some(k) = drove_by(&words) {
            seen.drove.push(k);
        }
        let inline = inline_code(&words);
        // Reading a pipe's output with a one-liner is looking, not running.
        if inline.is_some() && piped {
            continue;
        }
        let text = match inline {
            Some(at) => format!("{} …", words[..=at].join(" ")),
            None => words.join(" "),
        };
        seen.test = seen.test || is_test(&text);
        seen.ran.push(text);
    }
    seen
}

/// Where an interpreter is handed its program on the command line: `python3 -c
/// "…"`, `node -e "…"`. The program is not the command.
fn inline_code(words: &[String]) -> Option<usize> {
    let name = base(&words[0]);
    let flag = match name.as_str() {
        "python" | "python3" | "sh" | "bash" | "zsh" => "-c",
        "node" | "ruby" | "perl" => "-e",
        _ => return None,
    };
    words.iter().position(|w| w == flag || (w.starts_with('-') && !w.starts_with("--") && w.ends_with(flag.trim_start_matches('-')) && w.len() <= 4))
}

/// The kind of thing a command drives, when it drives one: a screen capture, a
/// GUI scripter, a `ctl` binary, a sandboxed instance, a headless browser, the
/// built program itself.
fn drove_by(words: &[String]) -> Option<String> {
    let name = base(&words[0]);
    let second = words.get(1).map(String::as_str);
    let kind = match name.as_str() {
        "screencapture" | "screenshot" => "screenshot",
        "osascript" | "cliclick" | "xdotool" | "playwright" | "puppeteer" => "gui",
        "ctl" => "ctl",
        n if n.ends_with("-ctl") => "ctl",
        "wsp" if second == Some("sandbox") => "sandbox",
        _ if second == Some("ctl") => "ctl",
        _ if words.iter().any(|w| w.starts_with("--headless")) => "browser",
        _ if words.iter().any(|w| matches!(w.as_str(), "--drive" | "--png" | "--screenshot")) => "app",
        _ if words[0].contains("/target/debug/") || words[0].contains("/target/release/") || words[0].starts_with("target/") => "app",
        "cargo" if second == Some("run") => "app",
        _ => return None,
    };
    Some(kind.to_string())
}

/// A command that is a test run: the runners every project's own commands
/// reach, by their words.
pub(crate) fn is_test(text: &str) -> bool {
    let w: Vec<&str> = text.split_whitespace().collect();
    w.iter().enumerate().any(|(i, x)| {
        let next = w.get(i + 1).copied().unwrap_or("");
        let after = w.get(i + 2).copied().unwrap_or("");
        let name = base(x);
        match name.as_str() {
            "cargo" => matches!(next, "test" | "nextest") || (next.starts_with('+') && matches!(after, "test" | "nextest")),
            "pytest" | "py.test" | "jest" | "vitest" | "rspec" | "phpunit" | "ctest" | "tox" => true,
            "python" | "python3" => next == "-m" && matches!(after, "pytest" | "unittest"),
            "npm" | "yarn" | "pnpm" | "bun" | "deno" => next == "test" || (next == "run" && after.starts_with("test")),
            "go" | "swift" | "dotnet" | "mix" | "mvn" | "gradle" | "gradlew" => next == "test",
            "make" => matches!(next, "test" | "check"),
            "wsp" => matches!(next, "check" | "verify"),
            _ => false,
        }
    })
}

/// `FOO=1 env -u X time cargo test` as `cargo test`; a shell keyword's line
/// as nothing.
fn strip_prefixes(mut w: Vec<String>) -> Vec<String> {
    loop {
        let Some(first) = w.first() else { return w };
        let name = base(first);
        let assignment = first.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !k.starts_with(|c: char| c.is_ascii_digit())
        });
        if assignment {
            w.remove(0);
        } else if name == "env" && w.len() > 1 {
            w.remove(0);
            while w.first().is_some_and(|x| x.starts_with('-') || x.contains('=')) {
                let flag = w.remove(0);
                if flag == "-u" && !w.is_empty() {
                    w.remove(0);
                }
            }
        } else if matches!(name.as_str(), "time" | "command" | "nohup" | "exec" | "sudo" | "builtin" | "!" | "then" | "do" | "else") {
            w.remove(0);
        } else if name == "timeout" && w.len() > 2 {
            w.drain(..2);
        } else if name == "nice" {
            w.remove(0);
            if w.first().is_some_and(|x| x.starts_with('-')) {
                let flag = w.remove(0);
                if flag == "-n" && !w.is_empty() {
                    w.remove(0);
                }
            }
        } else if matches!(name.as_str(), "for" | "while" | "until" | "if" | "elif" | "case" | "done" | "fi" | "esac" | "in") {
            return Vec::new();
        } else {
            return w;
        }
    }
}

/// What `cat`, `sed -n`, `head`, `tail` and `git show` read.
fn reads_of(name: &str, w: &[String], writes: bool, here: &str, seen: &mut Seen) {
    let args: Vec<&String> = w.iter().skip(1).collect();
    let files = |skip_values: &[&str]| -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if skip_values.contains(&a) {
                i += 2;
                continue;
            }
            if !a.starts_with('-') && !a.contains('$') && !a.is_empty() {
                out.push(at(a, here));
            }
            i += 1;
        }
        out
    };
    match name {
        "cat" | "bat" | "nl" | "less" | "more" if !writes => {
            for f in files(&[]) {
                seen.reads.push((f, None));
            }
        }
        "head" | "tail" => {
            let mut n = None;
            let mut valued = false;
            let mut bytes = false;
            for (i, a) in args.iter().enumerate() {
                if a.as_str() == "-n" || a.as_str() == "-c" {
                    bytes = bytes || a.as_str() == "-c";
                    n = args.get(i + 1).and_then(|v| v.trim_start_matches('+').parse::<u64>().ok());
                    valued = true;
                } else if let Some(v) = a.strip_prefix("-n").or_else(|| a.strip_prefix('-')).and_then(|v| v.parse::<u64>().ok()) {
                    n = Some(v);
                }
            }
            let skip: &[&str] = if valued { &["-n", "-c"] } else { &[] };
            let range = if name == "head" && !bytes { Some((1, n.unwrap_or(10))) } else { Some(PARTIAL) };
            for f in files(skip) {
                seen.reads.push((f, range));
            }
        }
        "sed" => {
            let quiet = args.iter().any(|a| matches!(a.as_str(), "-n" | "-nE" | "-En"));
            if !quiet || args.iter().any(|a| a.starts_with("-i")) {
                return;
            }
            let rest: Vec<&&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
            let Some((script, files)) = rest.split_first() else { return };
            let range = sed_range(script).or(Some(PARTIAL));
            for f in files.iter().filter(|f| !f.contains('$')) {
                seen.reads.push((at(f, here), range));
            }
        }
        "git" => git_reads(&args, seen),
        _ => {}
    }
}

fn git_reads(args: &[&String], seen: &mut Seen) {
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        i += if matches!(args[i].as_str(), "-C" | "-c") { 2 } else { 1 };
    }
    let Some(sub) = args.get(i).map(|s| s.as_str()) else { return };
    let rest = &args[i + 1..];
    let (before, after) = match rest.iter().position(|a| a.as_str() == "--") {
        Some(p) => (&rest[..p], &rest[p + 1..]),
        None => (rest, &rest[..0]),
    };
    let revs: Vec<&str> = before.iter().map(|s| s.as_str()).filter(|a| !a.starts_with('-') && !a.contains('$')).collect();
    match sub {
        "show" | "diff" => {
            let mut named = Vec::new();
            for r in &revs {
                match r.split_once(':') {
                    Some((_, path)) if sub == "show" && !path.is_empty() => seen.reads.push((path.to_string(), None)),
                    _ => named.push(*r),
                }
            }
            if named.is_empty() && after.is_empty() && revs.is_empty() && !before.iter().any(|a| a.contains('$')) {
                named.push(if sub == "show" { "HEAD" } else { "(worktree)" });
            }
            if !named.is_empty() || !after.is_empty() {
                let mut d = named.join(" ");
                if !after.is_empty() {
                    d.push_str(" -- ");
                    d.push_str(&after.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "));
                }
                seen.diffs.push(crate::util::truncate(d.trim(), 70));
            }
        }
        "log" if before.iter().any(|a| matches!(a.as_str(), "-p" | "--patch")) => {
            seen.diffs.push(crate::util::truncate(format!("log {}", revs.join(" ")).trim(), 70));
        }
        _ => {}
    }
}

/// `40,90p`, `12p`, `5,$p` as lines.
fn sed_range(script: &str) -> Option<(u64, u64)> {
    let s = script.trim().strip_suffix('p')?;
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, if b == "$" { u64::MAX } else { b.parse().ok()? })),
        None => s.parse().ok().map(|a| (a, a)),
    }
}

struct Simple {
    words: Vec<String>,
    /// Whether the command redirects its output to a file.
    writes: bool,
    /// Whether it reads another command's output.
    piped: bool,
}

/// A command line as the simple commands it is made of, each as words: quotes
/// taken off, heredoc bodies dropped, redirections taken out. Not a shell —
/// enough of one to tell `cargo test` from the `grep` after it.
fn simple_commands(cmd: &str) -> Vec<Simple> {
    struct Cur {
        out: Vec<Simple>,
        words: Vec<String>,
        word: String,
        in_word: bool,
        writes: bool,
        skip_next: bool,
        piped: bool,
    }
    impl Cur {
        fn end_word(&mut self) {
            if !self.in_word {
                return;
            }
            let w = std::mem::take(&mut self.word);
            self.in_word = false;
            if std::mem::take(&mut self.skip_next) {
                return;
            }
            let digits = w.trim_start_matches(|c: char| c.is_ascii_digit());
            if digits.starts_with('>') || digits.starts_with('<') || digits.starts_with("&>") {
                let target = digits.trim_start_matches(['>', '<', '&']);
                if !digits.starts_with('<') && !target.starts_with('&') && target != "/dev/null" && !target.is_empty() {
                    self.writes = true;
                }
                if target.is_empty() {
                    self.skip_next = true;
                    // `> file` writes; `2>&1` is a word of its own and never reaches here empty.
                    self.writes = self.writes || digits.starts_with('>');
                }
                return;
            }
            self.words.push(w);
        }

        fn end_command(&mut self) {
            self.end_word();
            if !self.words.is_empty() {
                self.out.push(Simple { words: std::mem::take(&mut self.words), writes: self.writes, piped: self.piped });
            }
            self.writes = false;
            self.skip_next = false;
            self.piped = false;
        }
    }

    let text = without_heredocs(cmd);
    let chars: Vec<char> = text.chars().collect();
    let mut cur = Cur { out: Vec::new(), words: Vec::new(), word: String::new(), in_word: false, writes: false, skip_next: false, piped: false };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                cur.in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    cur.word.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                cur.in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    cur.word.push(chars[i]);
                    i += 1;
                }
            }
            '\\' if i + 1 < chars.len() => {
                cur.in_word = true;
                i += 1;
                if chars[i] != '\n' {
                    cur.word.push(chars[i]);
                }
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                cur.in_word = true;
                let mut depth = 0;
                while i < chars.len() {
                    cur.word.push(chars[i]);
                    if chars[i] == '(' {
                        depth += 1;
                    } else if chars[i] == ')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
            }
            '#' if !cur.in_word => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            ' ' | '\t' => cur.end_word(),
            '&' if cur.word.ends_with('>') || chars.get(i + 1) == Some(&'>') => {
                cur.in_word = true;
                cur.word.push(c);
            }
            '{' | '}' if cur.in_word => cur.word.push(c),
            // `name() {` defines a function: the name is not a command.
            '(' if cur.in_word && chars.get(i + 1) == Some(&')') => {
                cur.word.clear();
                cur.in_word = false;
                cur.words.clear();
                i += 1;
            }
            ';' | '\n' | '|' | '(' | ')' | '&' | '{' | '}' => {
                let pipe = c == '|' && chars.get(i + 1) != Some(&'|');
                cur.end_command();
                cur.piped = pipe;
                if matches!((c, chars.get(i + 1)), ('&', Some('&')) | ('|', Some('|'))) {
                    i += 1;
                }
            }
            _ => {
                cur.in_word = true;
                cur.word.push(c);
            }
        }
        i += 1;
    }
    cur.end_command();
    cur.out
}

/// The command with each heredoc's body taken out, the line that opens it kept.
fn without_heredocs(cmd: &str) -> String {
    let mut out = String::new();
    let mut ends: Option<String> = None;
    for l in cmd.lines() {
        if let Some(tag) = &ends {
            if l.trim() == tag {
                ends = None;
            }
            continue;
        }
        out.push_str(l);
        out.push('\n');
        if let Some(p) = l.find("<<") {
            let rest = &l[p + 2..];
            if !rest.starts_with('<') {
                let tag: String = rest
                    .trim_start_matches('-')
                    .trim_start()
                    .trim_start_matches(['\'', '"'])
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !tag.is_empty() {
                    ends = Some(tag);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn transcript(events: &[Value]) -> String {
        events.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n")
    }

    fn use_(id: &str, name: &str, input: Value) -> Value {
        json!({"type": "assistant", "cwd": "/w/t", "timestamp": "2026-11-01T10:00:00Z", "message": {"id": format!("m-{id}"), "model": "claude-sonnet-5-5", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
    }

    fn result(id: &str, error: bool, text: &str) -> Value {
        json!({"type": "user", "cwd": "/w/t", "timestamp": "2026-11-01T10:00:01Z", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "is_error": error, "content": text}]}})
    }

    fn did(events: &[Value]) -> Did {
        of_transcript(&mut std::io::Cursor::new(transcript(events)), 0).unwrap()
    }

    fn ran(cmd: &str) -> Vec<String> {
        bash(cmd, "/w/t").ran
    }

    fn back(d: &Did) -> Did {
        let mut b = Did::default();
        for l in d.lines() {
            let (k, body) = split_line(&l).expect("ours");
            b.absorb(k, body);
        }
        b
    }

    /// A verifier's inspection is not a command it ran; a test, a script and a
    /// check are — and the shell around them (a `cd`, a pipe to `tail`, a
    /// heredoc that writes the verdict) is not part of the command.
    #[test]
    fn looking_is_not_running_and_the_shell_around_a_command_is_not_the_command() {
        assert_eq!(ran("cd /w/t && cargo test check:: 2>&1 | tail -5"), vec!["cargo test check::"]);
        assert!(ran("uptime; sysctl -n hw.ncpu; git log --oneline -1; git status --short | head -3").is_empty());
        assert_eq!(ran("wsp show wsp-1; wsp check wsp-1"), vec!["wsp check wsp-1"]);
        assert!(ran("F=/tmp/v.md\ncat > $F <<'EOF'\nran: cargo test\nEOF\nwsp verified wsp-1 --holds --from $F").is_empty());
        assert_eq!(ran("RUST_LOG=x env -u WSP_HOME python3 scripts/replay.py 20"), vec!["python3 scripts/replay.py 20"]);
        assert_eq!(ran("for c in a b; do cargo test -p $c; done"), vec!["cargo test -p $c"]);
        assert_eq!(ran("(time cargo build --release) 2>&1 | tail -3"), vec!["cargo build --release"]);
        assert!(ran("cargo metadata --no-deps | jq .packages").is_empty());
        assert!(ran("grep -rn 'cargo test' src | head").is_empty());
        assert_eq!(ran("sleep 1; ./slow.sh --x && echo done"), vec!["./slow.sh --x"]);
        assert!(ran("wsp worklist next l; wsp project show p; wsp watch --status").is_empty());
        assert_eq!(ran("nice -n 10 cargo test launchd:: 2>&1 | tail -25; nice cargo build"), vec!["cargo test launchd::", "cargo build"]);
    }

    #[test]
    fn a_test_run_is_recognised_by_its_runner_whatever_the_project() {
        for t in [
            "cargo test -p host", "cargo +nightly test", "pytest tests/", "python3 -m pytest -x", "npm test", "npm run test:unit", "go test ./...", "make test",
            "wsp check wsp-1", "swift test",
        ] {
            assert!(is_test(t), "{t}");
        }
        for t in ["cargo build", "python3 scripts/report.py", "npm run build", "go build ./..."] {
            assert!(!is_test(t), "{t}");
        }
    }

    /// The outcome comes from the result: an error flag, or a test run's own
    /// output when a pipe hid the status — and a test that failed then passed
    /// reads as both, in order.
    #[test]
    fn outcomes_follow_the_result_and_a_rerun_keeps_both() {
        let d = did(&[
            use_("a", "Bash", json!({"command": "cargo test x:: 2>&1 | grep -E 'test result|FAILED'"})),
            result("a", false, "test result: FAILED. 1 passed; 1 failed"),
            use_("b", "Bash", json!({"command": "cargo test x:: 2>&1 | grep -E 'test result|FAILED'"})),
            result("b", false, "test result: ok. 2 passed; 0 failed"),
            use_("c", "Bash", json!({"command": "cargo test x:: 2>&1 | grep -E 'test result|FAILED'"})),
            result("c", false, "test result: ok. 2 passed; 0 failed"),
            use_("d", "Bash", json!({"command": "python3 s.py"})),
            result("d", true, "Exit code 1\nboom"),
            use_("e", "Bash", json!({"command": "sleep 1; ./slow.sh"})),
        ]);
        let got: Vec<_> = d.ran.iter().map(|c| (c.text.as_str(), c.outcomes_text(), c.is_test())).collect();
        assert_eq!(
            got,
            vec![
                ("cargo test x::", "failed → passed ×2".to_string(), true),
                ("python3 s.py", "failed".to_string(), false),
                ("./slow.sh", "unfinished".to_string(), false),
            ]
        );
        assert!(!failed_output("test result: ok. 12 passed; 0 failed; 0 ignored"));
        assert!(failed_output("Tests:  2 failed, 5 passed"));
        assert!(failed_output("=== 1 failed in 0.12s ==="));
        assert!(!failed_output("246 |     failed.attempts.failed(\"x\");"), "a line number is not a count");
    }

    #[test]
    fn what_was_read_is_files_with_their_lines_and_diffs() {
        let d = did(&[
            use_("1", "Read", json!({"file_path": "/w/t/src/check.rs", "offset": 10, "limit": 31})),
            use_("2", "Read", json!({"file_path": "/w/t/src/check.rs", "offset": 30, "limit": 30})),
            use_("3", "Bash", json!({"command": "sed -n '100,120p' src/check.rs; sed -n '5p' src/cycle.rs; head -20 README.md"})),
            use_("4", "Bash", json!({"command": "git show 55c4839 -- src/check.rs | head -80; git diff 9e1c..55c4 --stat; git show master:src/main.rs"})),
            use_("5", "Bash", json!({"command": "cat src/legs.rs"})),
            use_("6", "Read", json!({"file_path": "/w/t/src/legs.rs", "offset": 3, "limit": 4})),
        ]);
        let by = |p: &str| d.read.files.iter().find(|f| f.path == p).cloned().unwrap();
        assert_eq!(by("src/check.rs").lines, vec![(10, 59), (100, 120)]);
        assert_eq!(by("src/cycle.rs").lines, vec![(5, 5)]);
        assert_eq!(by("README.md").lines, vec![(1, 20)]);
        assert_eq!(by("src/legs.rs").lines, Vec::<(u64, u64)>::new(), "a whole read swallows a part");
        assert!(d.read.files.iter().any(|f| f.path == "src/main.rs"), "git show REV:PATH reads the file");
        assert_eq!(d.read.diffs, vec!["55c4839 -- src/check.rs", "9e1c..55c4"]);
    }

    /// What it means to drive: a browser server's tools, a screen capture, a
    /// `ctl` binary, a sandbox — and not `systemctl`, not a test that merely
    /// names one.
    #[test]
    fn driving_is_a_running_thing_acted_on() {
        let d = did(&[
            use_("1", "mcp__claude-in-chrome__navigate", json!({"url": "x"})),
            use_("2", "mcp__claude-in-chrome__computer", json!({})),
            use_("3", "Bash", json!({"command": "compound ctl snapshot; target/debug/compound-ctl click a"})),
            use_("4", "Bash", json!({"command": "screencapture -x /tmp/a.png; systemctl status x; launchctl list"})),
            use_("5", "Bash", json!({"command": "wsp sandbox --fake --stage"})),
            use_("6", "mcp__github__get_issue", json!({})),
            use_("7", "Bash", json!({"command": "target/debug/host --state s.json; ./target/debug/gui --drive keys.txt; cargo run -p tool -- --png a.png"})),
            use_("8", "Bash", json!({"command": "chrome --headless=new --screenshot=a.png"})),
        ]);
        assert_eq!(
            d.drove,
            vec!["app · 3 commands", "browser · 1 command", "ctl · 2 commands", "mcp claude-in-chrome · 2 calls", "sandbox · 1 command", "screenshot · 1 command"]
        );
        assert!(!did(&[use_("1", "Bash", json!({"command": "cargo test window::"}))]).drove());
    }

    /// Entry lines read back as written, whatever they carry, and a line that
    /// is not one of the four is not taken.
    #[test]
    fn entry_lines_round_trip() {
        let d = did(&[
            use_("1", "Read", json!({"file_path": "/w/t/a b.rs", "offset": 2, "limit": 3})),
            use_("2", "Bash", json!({"command": "cargo test a:: | tail -2; wsp check m-1"})),
            result("2", false, "test result: FAILED. 0 passed; 1 failed"),
            use_("3", "Bash", json!({"command": "git diff A..B -- src"})),
            use_("4", "Bash", json!({"command": "screencapture x.png"})),
        ]);
        assert_eq!(back(&d), d, "{:#?}", d.lines());
        assert!(split_line("- **verdict:** x").is_none());
        let empty = Did::default().lines();
        assert_eq!(empty, vec!["- **ran:** none", "- **drove:** none", "- **read:** none"]);
        assert_eq!(back(&Did::default()), Did::default());
    }

    /// The misreadings a hand check of ten verifier transcripts found: a shell
    /// function's name, a one-liner that reads a pipe, `--help`, a sed the
    /// pattern addresses, a session's own scratch files, a shell that could
    /// not start the command.
    #[test]
    fn the_misreadings_a_hand_check_found() {
        assert_eq!(ran("run() { env -u X wsp \"$@\"; }\nrun govern wsp"), vec!["wsp $@", "run govern wsp"]);
        assert!(ran("wsp worklist show l --json | python3 -I -c 'import json,sys; print(1)'").is_empty());
        assert_eq!(ran("python3 -I -c 'print(1)'"), vec!["python3 -I -c …"]);
        assert!(ran("wsp sandbox --help | head -5; cargo build --version").is_empty());
        assert!(ran("for p in a; do [ -z $p ] && continue; done; comm -12 a b").is_empty());
        let d = did(&[
            use_("1", "Bash", json!({"command": "sed -n '/fn a/,/^}/p' src/a.rs; sed -n \"$(grep -n x y),+9p\" src/b.rs; sed -n 3,4p src/b.rs"})),
            use_("2", "Bash", json!({"command": "sed -n 1,400p /Users/e/.claude/projects/x/tool-results/b.txt; cat /private/tmp/claude-501/s/v.md"})),
            use_("3", "Bash", json!({"command": "timeout 5 cargo test a::"})),
            result("3", false, "(eval):1: command not found: timeout"),
        ]);
        let by = |p: &str| d.read.files.iter().find(|f| f.path == p).map(|f| f.lines.clone());
        assert_eq!(by("src/a.rs"), Some(vec![PARTIAL]));
        assert_eq!(by("src/b.rs"), Some(vec![(3, 4)]), "a known range outranks an unknown one");
        assert_eq!(d.read.files.len(), 2, "scratch files are not the work");
        assert_eq!(d.ran[0].outcomes, vec![Outcome::Failed]);
        assert_eq!(back(&d), d);
    }

    /// A `cd` in the command moves the paths after it, so one file is one file
    /// whether it was read from the tree's root or from below it.
    #[test]
    fn a_cd_names_where_the_path_after_it_is() {
        let d = did(&[
            use_("1", "Bash", json!({"command": "cd spikes/c; sed -n 1,80p src/lib.rs; cd ..; cat Cargo.toml"})),
            use_("2", "Bash", json!({"command": "cd /w/t/spikes/c && head -5 src/lib.rs; cd /elsewhere; cat x.md; tail -3 /w/t/README.md"})),
            use_("3", "Read", json!({"file_path": "/w/t/spikes/c/src/lib.rs", "offset": 70, "limit": 20})),
        ]);
        let got: Vec<_> = d.read.files.iter().map(FileRead::text).collect();
        assert_eq!(got, vec!["spikes/c/src/lib.rs:1-89", "spikes/Cargo.toml", "/elsewhere/x.md", "README.md:~"], "{got:?}");
        let home = did(&[use_("1", "Bash", json!({"command": "cd ~/.local/state/wsp; head -c 600 events.jsonl; sed -n 3,4p cycle.log"}))]);
        let got: Vec<_> = home.read.files.iter().map(FileRead::text).collect();
        assert_eq!(got, vec!["~/.local/state/wsp/events.jsonl:~", "~/.local/state/wsp/cycle.log:3-4"], "{got:?}");
    }

    /// A `cd` typed from the root, run where the shell already is, fails, and
    /// what follows still reads the right file: it is not a deeper directory.
    #[test]
    fn a_cd_that_could_only_have_failed_does_not_move_the_path() {
        let d = did(&[
            use_("1", "Bash", json!({"command": "cd crates/x/src; cat a.rs; git diff --stat; for c in 1 2; do git show --stat $c; done"})),
            json!({"type": "assistant", "cwd": "/w/t/crates/x/src", "timestamp": "2026-11-01T10:00:00Z", "message": {"id": "m-2", "content": [
                {"type": "tool_use", "id": "2", "name": "Bash", "input": {"command": "cd crates/x/src; cat b.rs"}}]}}),
        ]);
        let got: Vec<_> = d.read.files.iter().map(FileRead::text).collect();
        assert_eq!(got, vec!["crates/x/src/a.rs", "crates/x/src/b.rs"], "{got:?}");
        assert_eq!(d.read.diffs, vec!["(worktree)"], "no revision named is the working tree, and a loop's variable is no revision");
    }

    #[test]
    fn a_line_per_command_is_capped_and_says_how_many_it_left_out() {
        let events: Vec<Value> = (0..40).map(|i| use_(&format!("c{i}"), "Bash", json!({"command": format!("./step{i}.sh")}))).collect();
        let d = did(&events);
        assert_eq!(d.ran.len(), MAX_RAN);
        assert_eq!(d.more_ran, 10);
        assert_eq!(back(&d).more_ran, 10);
    }

    #[test]
    fn checked_lines_come_out_of_the_verdict_and_must_say_held_or_not() {
        let (text, c) = take_checked(
            "Blocks.\n\nchecked: the gate holds the verifier — held\n- **checked:** compound has a declaration (not held)\n* checked: replay on 20 rows - not held.\nprose\n",
        )
        .unwrap();
        assert_eq!(text, "Blocks.\n\nprose");
        assert_eq!(
            c,
            vec![
                Checked { item: "the gate holds the verifier".into(), held: true },
                Checked { item: "compound has a declaration".into(), held: false },
                Checked { item: "replay on 20 rows".into(), held: false },
            ]
        );
        assert_eq!(Checked::parse(&c[1].text()), Some(c[1].clone()));
        let why = take_checked("checked: the replay was run").unwrap_err();
        assert!(why.contains("held") && why.contains("the replay was run"), "{why}");
        assert!(take_checked("checked: held").is_err(), "an item is named");
        assert_eq!(take_checked("no lines\nhere").unwrap(), ("no lines\nhere".to_string(), vec![]));
    }

    #[test]
    fn a_transcript_with_nothing_in_it_is_not_a_verifier_that_did_nothing() {
        assert!(of_transcript(&mut std::io::Cursor::new(String::new()), 0).is_none());
        let d = did(&[json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "hi"}]}})]);
        assert_eq!(d, Did::default());
    }

    #[test]
    fn a_sidechain_a_replayed_call_and_an_earlier_attempt_are_not_this_pass() {
        let mut side = use_("s", "Bash", json!({"command": "./sub.sh"}));
        side["isSidechain"] = json!(true);
        let mut old = use_("o", "Bash", json!({"command": "./old.sh"}));
        old["timestamp"] = json!("2026-11-01T09:00:00Z");
        let again = use_("a", "Bash", json!({"command": "./once.sh"}));
        let src = transcript(&[side, old, again.clone(), again, result("a", false, "ok")]);
        let d = of_transcript(&mut std::io::Cursor::new(src), crate::util::epoch_of("2026-11-01T09:30:00Z")).unwrap();
        assert_eq!(d.ran, vec![Command { text: "./once.sh".into(), outcomes: vec![Outcome::Passed] }]);
    }
}
