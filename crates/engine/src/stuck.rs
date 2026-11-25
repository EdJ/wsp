//! A shell command that is waiting on itself, or on a terminal: `wsp-397`.
//!
//! `compounding-061` sat six minutes in a watcher of the shape
//! `until ! pgrep -f "cargo test"; do sleep 2; done`. The shell Claude Code runs
//! it in has the whole command on its own command line, so `pgrep -f` finds the
//! shell, which is running the loop, which is waiting for `pgrep` to find
//! nothing. A bare `cat` waits for a terminal that is never typed at. Neither
//! ends, and the seat reads `working` for as long as it is in the call.
//!
//! **Read from the seat's transcript**, which is the only place a call that has
//! not returned is written down with the minute it started. [`scan`] pairs each
//! Bash call with its result and says which of them were one of those two
//! shapes ([`classify`]) *and* held for at least [`MIN`] seconds: the shape
//! says what was wrong with it, and the seconds say it was not one that returned
//! at once (`until pgrep -f x` on a process that is already there). A call with
//! no result yet is open, and its seconds are to now.
//!
//! The daemon's pass ([`crate::repair::stuck_tick`]) reads the tail of a working
//! seat's transcript with [`in_tail`], keeps the open stall in
//! `stuck.json`, tells the seat once, and writes the seconds it held to
//! `cycle.log` when it ends. `wsp wip` draws the record. `wsp stuck FILE…`
//! reads whole transcripts, which is how the rate is counted.
//!
//! Two more shapes came from the first count over the fleet's transcripts: a
//! loop with a `sleep` whose only wait is a background task's `…/tasks/<id>.output`
//! (or a scratch file) turning up ([`Kind::WatchOutput`]), which is the commonest
//! stall by far, and any other foreground call held to the tool's timeout
//! ([`Kind::TimedOut`]).
//!
//! # What it cannot see
//!
//! A test hung at 0 % CPU inside a call that has not yet reached the timeout is
//! not named: that is a seat's processes, not its transcript. A watcher written
//! some other way (a `lsof` loop, a `curl` retry against a server that is never
//! up) is a stall this does not name until it reaches the timeout. The match of a
//! `pgrep` pattern against the command is a small matcher over the common
//! subset of regular expressions; a pattern outside it (a group) is read as not
//! matching, which is the direction that flags less.

use std::collections::HashMap;
use std::io::BufRead;

use serde_json::Value;

/// How long a call of one of these shapes must have been in flight to be a
/// stall, in seconds.
pub const MIN: i64 = 45;

/// The seconds a foreground call gets when it names no timeout of its own.
const TIMEOUT: i64 = 600;

/// How far short of its timeout a call may end and still be one the tool cut.
const SLACK: i64 = 10;

/// How much of the end of a transcript the daemon reads for an open call.
const TAIL: u64 = 2 * 1024 * 1024;

/// What is wrong with the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A loop on `pgrep -f` or `ps | grep` whose pattern is in the loop's own line.
    SelfWait,
    /// `cat` with no file, reading a terminal.
    BareCat,
    /// A loop with a `sleep` whose only wait is a file of a background task's
    /// output (or the seat's scratch) turning up or holding the line it wants.
    WatchOutput,
    /// Any other foreground call held to the tool's timeout.
    TimedOut,
}

impl Kind {
    /// Whether a command line is [`Kind::SelfWait`]: what the daemon's pass reads
    /// a seat's shell for without holding the seat. Here, on the type `repair`
    /// already names, so the engine does not reach up for a function of this layer.
    pub fn is_self_wait(command: &str) -> bool {
        classify(command) == Some(Kind::SelfWait)
    }

    pub fn word(self) -> &'static str {
        match self {
            Kind::SelfWait => "waiting on itself",
            Kind::BareCat => "bare cat on a terminal",
            Kind::WatchOutput => "watching output that may never come",
            Kind::TimedOut => "held to the tool timeout",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::SelfWait => "self",
            Kind::BareCat => "cat",
            Kind::WatchOutput => "watch",
            Kind::TimedOut => "timeout",
        }
    }

    pub fn of_key(k: &str) -> Option<Kind> {
        match k {
            "self" => Some(Kind::SelfWait),
            "cat" => Some(Kind::BareCat),
            "watch" => Some(Kind::WatchOutput),
            "timeout" => Some(Kind::TimedOut),
            _ => None,
        }
    }

    /// What the seat is told to do instead.
    pub fn advice(self) -> &'static str {
        match self {
            Kind::SelfWait => {
                "the `pgrep -f` pattern is on the command's own line, so it finds the shell running the loop and never comes back empty. \
                 Never wait on `pgrep -f \"<string>\"`: wait on a pid (`wait <pid>`, or `while kill -0 <pid>; do sleep 5; done`), \
                 or run the work with `run_in_background` and wait for its notification"
            }
            Kind::BareCat => "`cat` with no file reads a terminal nobody types at. Interrupt it, and name the file",
            Kind::WatchOutput => {
                "it polls a file that the command it watches may never write (it ended, failed, or was never started). \
                 Interrupt it, look at the task directly, and wait with a bound (`timeout 120 …`) or on the pid"
            }
            Kind::TimedOut => "a foreground call that runs to the tool's timeout is not waiting on anything it will get. Interrupt it, and run it in the background or with a bound",
        }
    }
}

/// A stalled call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stall {
    pub kind: Kind,
    /// The command, cut to a line.
    pub command: String,
    /// When the call started, epoch seconds.
    pub at: i64,
    /// Seconds it held: to its result, or to now while it is [`Stall::open`].
    pub secs: i64,
    /// No result yet.
    pub open: bool,
}

// ---- reading a transcript ---------------------------------------------------

/// The stalls in a transcript, oldest first. `now` is what an open call's
/// seconds are counted to; 0 leaves open calls out.
pub fn scan(src: &mut impl BufRead, now: i64) -> Vec<Stall> {
    // id -> (started, command, kind, foreground, limit): the limit is the
    // seconds the tool gives the call before it ends it.
    let mut pending: HashMap<String, (i64, String, Option<Kind>, bool, i64)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut out: Vec<Stall> = Vec::new();
    let mut buf = String::new();
    while {
        buf.clear();
        src.read_line(&mut buf).unwrap_or(0) > 0
    } {
        if buf.contains("\"tool_result\"") {
            // Only the id and the stamp, off the text: a result line can be
            // very large and nearly all of them are for calls nobody cares about.
            let Some(id) = field(&buf, "tool_use_id") else { continue };
            let Some((at, command, kind, foreground, limit)) = pending.remove(id) else { continue };
            order.retain(|o| o != id);
            let end = field(&buf, "timestamp").map(crate::util::epoch_of).unwrap_or(0);
            if end > 0 && at > 0 {
                let secs = end - at;
                if let Some(kind) = named(kind, foreground, limit, secs) {
                    out.push(Stall { kind, command, at, secs, open: false });
                }
            }
        } else if buf.contains("\"tool_use\"") && (buf.contains("\"Bash\"") || buf.contains("\"bash\"")) {
            let Ok(v) = serde_json::from_str::<Value>(&buf) else { continue };
            let at = v.get("timestamp").and_then(Value::as_str).map(crate::util::epoch_of).unwrap_or(0);
            let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else { continue };
            for b in blocks {
                if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                if !matches!(b.get("name").and_then(Value::as_str), Some("Bash" | "bash")) {
                    continue;
                }
                let (Some(id), Some(command)) = (b.get("id").and_then(Value::as_str), b.pointer("/input/command").and_then(Value::as_str))
                else {
                    continue;
                };
                // A background command returns at once, whatever it runs.
                let background = b.pointer("/input/run_in_background").and_then(Value::as_bool) == Some(true);
                let kind = if background { None } else { classify(command) };
                let limit = match b.pointer("/input/timeout").and_then(Value::as_i64) {
                    Some(ms) if ms > 0 => ms / 1000,
                    _ => TIMEOUT,
                };
                if pending.insert(id.to_string(), (at, command.to_string(), kind, !background, limit)).is_none() {
                    order.push(id.to_string());
                }
            }
        }
    }
    if now > 0 {
        for id in order {
            let Some((at, command, kind, foreground, limit)) = pending.remove(&id) else { continue };
            if at <= 0 {
                continue;
            }
            if let Some(kind) = named(kind, foreground, limit, now - at) {
                out.push(Stall { kind, command, at, secs: now - at, open: true });
            }
        }
    }
    for s in &mut out {
        s.command = one_line(&s.command);
    }
    out
}

/// What a call held for `secs` is, when it is a stall: its shape if it has one
/// and held [`MIN`] seconds, else a foreground call held to within [`SLACK`] of
/// the `limit` the tool gives it.
fn named(kind: Option<Kind>, foreground: bool, limit: i64, secs: i64) -> Option<Kind> {
    match kind {
        Some(k) if secs >= MIN => Some(k),
        Some(_) => None,
        None if foreground && secs >= MIN && secs >= limit - SLACK => Some(Kind::TimedOut),
        None => None,
    }
}

/// The stalls in the last [`TAIL`] bytes of a transcript file, open and closed.
pub fn in_tail(path: &std::path::Path, now: i64) -> Vec<Stall> {
    use crate::host::Host;
    let Some(text) = crate::host::LOCAL.transcript_tail(path, TAIL) else { return Vec::new() };
    scan(&mut text.as_bytes(), now)
}

/// A string field written as `"key":"value"` with no escape in the value: the
/// ids and stamps a transcript line carries.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":\"");
    let i = line.find(&needle)? + needle.len();
    let rest = &line[i..];
    Some(&rest[..rest.find('"')?])
}

fn one_line(command: &str) -> String {
    let first: String = command.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ; ");
    match first.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", &first[..i]),
        None => first,
    }
}

// ---- the shapes -------------------------------------------------------------

/// Whether a command line is a loop whose search finds the loop: the one
/// [`Kind`] a seat's shell is read for without holding the seat.
pub fn waits_on_itself_line(command: &str) -> bool {
    classify(command) == Some(Kind::SelfWait)
}

/// Which of the shapes a command is, when it is one.
pub fn classify(command: &str) -> Option<Kind> {
    let simple = simple_commands(command);
    if waits_on_itself(command, &simple) {
        return Some(Kind::SelfWait);
    }
    if simple.iter().any(|c| bare_cat(&c.words, c.after_pipe)) {
        return Some(Kind::BareCat);
    }
    if watches_output(&simple) {
        return Some(Kind::WatchOutput);
    }
    None
}

/// A loop with a `sleep` whose wait is a file of a background task's output
/// (`…/tasks/<id>.output`) or of the seat's scratch: `grep -q` on it, or
/// `[ -s … ]` / `test -s …`. Nothing else in the loop can end it (no pid, no
/// process search), so it ends when that file is written and not otherwise.
fn watches_output(simple: &[Simple]) -> bool {
    let looped = simple.iter().any(|c| c.words.iter().any(|w| matches!(w.as_str(), "while" | "until")));
    let mut sleeps = false;
    let mut waits = false;
    for c in simple {
        let w = program(&c.words);
        let Some(prog) = w.first().map(|a| name_of(a)) else { continue };
        match prog {
            "sleep" => sleeps = true,
            "kill" | "pgrep" | "ps" | "pidof" | "wait" | "lsof" | "curl" => return false,
            "grep" | "egrep" | "fgrep" => {
                let quiet = w[1..].iter().any(|a| a == "--quiet" || a == "--silent" || (a.starts_with('-') && !a.starts_with("--") && a.contains('q')))
                    || w[1..].iter().any(|a| a.contains("/dev/null"));
                if quiet && w[1..].iter().any(|a| is_output_path(a)) {
                    waits = true;
                }
            }
            "[" | "[[" | "test" => {
                let by_file = w[1..].iter().any(|a| matches!(a.as_str(), "-s" | "-f" | "-e"));
                if by_file && w[1..].iter().any(|a| is_output_path(a)) {
                    waits = true;
                }
            }
            _ => {}
        }
    }
    looped && sleeps && waits
}

/// A task's output file, or something in the seat's scratch.
fn is_output_path(w: &str) -> bool {
    (w.contains("/tasks/") && w.contains(".output")) || w.contains("WSP_SCRATCH") || w.contains("/scratch/")
}

#[derive(Debug)]
struct Simple {
    words: Vec<String>,
    /// What stands before it is `|`, so its stdin is the pipe.
    after_pipe: bool,
    /// The index of the pipeline it is in.
    pipeline: usize,
}

/// The command split into simple commands, honouring quotes. Not a shell
/// parser: a `$(…)` or a heredoc body is read as words, which errs toward
/// finding a `pgrep` that is not there only when the text names one.
fn simple_commands(command: &str) -> Vec<Simple> {
    let mut out: Vec<Simple> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let mut after_pipe = false;
    let mut pipeline = 0;
    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;
    macro_rules! end_word {
        () => {
            if has_word {
                words.push(std::mem::take(&mut word));
                has_word = false;
            }
        };
    }
    macro_rules! end_command {
        ($pipe:expr) => {{
            end_word!();
            if !words.is_empty() {
                out.push(Simple { words: std::mem::take(&mut words), after_pipe, pipeline });
            }
            after_pipe = $pipe;
            pipeline += usize::from(!$pipe);
        }};
    }
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                has_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    word.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                has_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() && matches!(chars[i + 1], '"' | '\\' | '$' | '`') {
                        i += 1;
                    }
                    word.push(chars[i]);
                    i += 1;
                }
            }
            '\\' if i + 1 < chars.len() => {
                has_word = true;
                i += 1;
                word.push(chars[i]);
            }
            ' ' | '\t' => end_word!(),
            '\n' | ';' | '(' | ')' => end_command!(false),
            '&' => {
                // `&&` and `&` end a command; `2>&1` and `>&2` are a redirection.
                if word.ends_with('>') || word.ends_with('<') {
                    word.push(c);
                } else {
                    if chars.get(i + 1) == Some(&'&') {
                        i += 1;
                    }
                    end_command!(false);
                }
            }
            '|' => {
                if chars.get(i + 1) == Some(&'|') {
                    i += 1;
                    end_command!(false);
                } else {
                    end_command!(true);
                }
            }
            _ => {
                has_word = true;
                word.push(c);
            }
        }
        i += 1;
    }
    if has_word {
        words.push(word);
    }
    if !words.is_empty() {
        out.push(Simple { words, after_pipe, pipeline });
    }
    out
}

/// The words of a simple command past `!`, `time`, `env` and `VAR=x` prefixes
/// and `then`/`do`/`while`/`until`/`if` keywords.
fn program(words: &[String]) -> &[String] {
    let skip = words
        .iter()
        .take_while(|w| matches!(w.as_str(), "!" | "time" | "env" | "then" | "do" | "while" | "until" | "if" | "elif" | "else") || is_assignment(w))
        .count();
    &words[skip..]
}

fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

fn name_of(argv0: &str) -> &str {
    argv0.rsplit('/').next().unwrap_or(argv0)
}

/// `cat` with no file operand, and stdin neither a file nor a pipe.
fn bare_cat(words: &[String], after_pipe: bool) -> bool {
    let w = program(words);
    if after_pipe || w.first().map(|a| name_of(a)) != Some("cat") {
        return false;
    }
    let mut i = 1;
    while i < w.len() {
        let a = w[i].as_str();
        if a.starts_with('<') {
            return false;
        }
        let digits = a.trim_start_matches(|c: char| c.is_ascii_digit());
        if digits.starts_with('>') {
            // `> file` and `2> file` take the next word; `2>&1` and `>file` do not.
            if digits == ">" || digits == ">>" {
                i += 1;
            }
        } else if a == "-" {
            // stdin, named
        } else if !a.starts_with('-') {
            return false;
        }
        i += 1;
    }
    true
}

/// A loop whose search finds the loop.
fn waits_on_itself(command: &str, simple: &[Simple]) -> bool {
    let looped = simple.iter().any(|c| c.words.iter().any(|w| matches!(w.as_str(), "while" | "until")));
    if !looped {
        return false;
    }
    // **Only a search the loop runs again** (`wsp-611`): `pid=$(pgrep -f "tauri
    // build" | head -1); while kill -0 $pid` searched once, before the loop, and
    // the loop waits on the pid it found, which was the real build. A search
    // the loop repeats is one between a `while`/`until` and its `done`.
    let mut depth = 0usize;
    for c in simple {
        if c.words.iter().any(|w| matches!(w.as_str(), "while" | "until")) {
            depth += 1;
        }
        let in_loop = depth > 0;
        if c.words.first().is_some_and(|w| w == "done") {
            depth = depth.saturating_sub(1);
        }
        if !in_loop {
            continue;
        }
        let w = program(&c.words);
        let Some(prog) = w.first().map(|a| name_of(a)) else { continue };
        match prog {
            "pgrep" => {
                let (flags, operands) = split_args(&w[1..], "uUgGPsStTF");
                let full = flags.iter().any(|f| f == "--full" || (!f.starts_with("--") && f.contains('f')));
                if full && operands.first().is_some_and(|p| matches_itself(p, command, false)) {
                    return true;
                }
            }
            "grep" | "egrep" | "fgrep" if c.after_pipe => {
                // `ps … | grep -v grep` drops the shell too: its line spells "grep".
                let pipeline: Vec<&Simple> = simple.iter().filter(|s| s.pipeline == c.pipeline).collect();
                let inverted = pipeline.iter().any(|s| {
                    let w = program(&s.words);
                    matches!(w.first().map(|a| name_of(a)), Some("grep" | "egrep" | "fgrep"))
                        && split_args(&w[1..], "ABCDefm").0.iter().any(|f| !f.starts_with("--") && f.contains('v'))
                });
                let fed_by_ps = pipeline.iter().any(|s| matches!(program(&s.words).first().map(|a| name_of(a)), Some("ps" | "pgrep")));
                let (flags, operands) = split_args(&w[1..], "ABCDm");
                // `-e PATTERN` names the pattern; otherwise it is the first operand.
                let pattern = pattern_flag(&w[1..]).or_else(|| operands.first().cloned());
                let fixed = prog == "fgrep" || flags.iter().any(|f| !f.starts_with("--") && f.contains('F'));
                if fed_by_ps && !inverted && pattern.is_some_and(|p| matches_itself(&p, command, fixed)) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn pattern_flag(args: &[String]) -> Option<String> {
    args.iter().position(|a| a == "-e").and_then(|i| args.get(i + 1).cloned())
}

/// Flags and operands. `takes` are the single-letter flags that take a value
/// as the next word.
fn split_args(args: &[String], takes: &str) -> (Vec<String>, Vec<String>) {
    let (mut flags, mut operands) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.len() > 1 && a.starts_with('-') && !a.starts_with("--") {
            // `-e` takes the pattern, which is not an operand.
            let last = a.chars().last().unwrap_or(' ');
            if a.chars().count() == 2 && (takes.contains(last) || last == 'e') {
                i += 1;
            }
            flags.push(a.clone());
        } else if a.starts_with("--") {
            flags.push(a.clone());
        } else {
            operands.push(a.clone());
        }
        i += 1;
    }
    (flags, operands)
}

// ---- does a pattern find its own command ------------------------------------

/// Whether `pattern`, as `pgrep -f` or `grep -E` reads it, matches somewhere in
/// `text`. A pattern with a group or an anchor is read as not matching.
fn matches_itself(pattern: &str, text: &str, fixed: bool) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if fixed {
        return text.contains(pattern);
    }
    let text: Vec<char> = text.chars().collect();
    split_alternatives(pattern).iter().any(|alt| match parse(alt) {
        Some(atoms) => (0..=text.len()).any(|i| step(&atoms, &text[i..])),
        None => false,
    })
}

fn split_alternatives(p: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut in_class = false;
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let cur = out.last_mut().expect("one at least");
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '[' => {
                in_class = true;
                out.last_mut().expect("one at least").push(c);
            }
            ']' => {
                in_class = false;
                out.last_mut().expect("one at least").push(c);
            }
            '|' if !in_class => out.push(String::new()),
            _ => out.last_mut().expect("one at least").push(c),
        }
    }
    out
}

#[derive(Debug)]
enum Atom {
    Any,
    Lit(char),
    Class(Vec<(char, char)>, bool),
}

#[derive(Debug)]
struct Item {
    atom: Atom,
    min: usize,
    max: usize,
}

fn parse(p: &str) -> Option<Vec<Item>> {
    let chars: Vec<char> = p.chars().collect();
    let mut items: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let atom = match chars[i] {
            '^' | '$' | '(' | ')' | '{' => return None,
            '.' => Atom::Any,
            '\\' => {
                i += 1;
                Atom::Lit(*chars.get(i)?)
            }
            '[' => {
                i += 1;
                let negate = chars.get(i) == Some(&'^');
                if negate {
                    i += 1;
                }
                let mut ranges = Vec::new();
                let mut first = true;
                loop {
                    let c = *chars.get(i)?;
                    if c == ']' && !first {
                        break;
                    }
                    first = false;
                    if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|e| *e != ']') {
                        ranges.push((c, chars[i + 2]));
                        i += 3;
                    } else {
                        ranges.push((c, c));
                        i += 1;
                    }
                }
                Atom::Class(ranges, negate)
            }
            '*' | '+' | '?' => return None,
            c => Atom::Lit(c),
        };
        i += 1;
        let (min, max) = match chars.get(i) {
            Some('*') => (0, usize::MAX),
            Some('+') => (1, usize::MAX),
            Some('?') => (0, 1),
            _ => (1, 1),
        };
        if (min, max) != (1, 1) {
            i += 1;
        }
        items.push(Item { atom, min, max });
    }
    Some(items)
}

fn one(atom: &Atom, c: char) -> bool {
    match atom {
        Atom::Any => true,
        Atom::Lit(l) => *l == c,
        Atom::Class(ranges, negate) => ranges.iter().any(|(a, b)| (*a..=*b).contains(&c)) != *negate,
    }
}

/// Whether the items match a prefix of `text`.
fn step(items: &[Item], text: &[char]) -> bool {
    let Some((item, rest)) = items.split_first() else { return true };
    let mut n = 0;
    while n < item.max && n < text.len() && one(&item.atom, text[n]) {
        n += 1;
    }
    (item.min..=n).rev().any(|k| step(rest, &text[k..]))
}

// ---- the verb ---------------------------------------------------------------

const USAGE: &str = "usage: wsp stuck <transcript.jsonl>…   the commands in these transcripts that waited on themselves, on a bare cat or on output that did not come, or were held to the tool timeout, with the seconds each held";

/// `wsp stuck FILE…`: the stalls in whole transcripts, one line each and a
/// total, which is how the rate is counted (`wsp-397`).
pub fn stuck(args: &crate::base::args::Args) -> i32 {
    use crate::host::Host;
    if args.rest.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
    let (mut files, mut hit, mut secs) = (0, 0, 0);
    let mut lines = Vec::new();
    for path in &args.rest {
        let Some(mut src) = crate::host::LOCAL.read_transcript(std::path::Path::new(path)) else {
            eprintln!("wsp: cannot read {path}");
            continue;
        };
        files += 1;
        let stalls = scan(&mut src, 0);
        if !stalls.is_empty() {
            hit += 1;
        }
        for s in stalls {
            secs += s.secs;
            lines.push(serde_json::json!({ "transcript": path, "kind": s.kind.key(), "secs": s.secs, "command": s.command }));
        }
    }
    if args.json() {
        println!("{}", serde_json::json!({ "transcripts": files, "with_stalls": hit, "stalls": lines.len(), "secs": secs, "found": lines }));
        return 0;
    }
    for l in &lines {
        println!("{:>6}s  {:<22} {}  {}", l["secs"], l["kind"].as_str().unwrap_or(""), l["transcript"].as_str().unwrap_or(""), l["command"].as_str().unwrap_or(""));
    }
    println!("{} stall(s) in {hit} of {files} transcript(s), {secs}s held", lines.len());
    0
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["stuck"],
        run: |_, args| stuck(args),
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 180,
                text: r##"  wsp stuck <transcript.jsonl>…     the commands in those transcripts that waited on
                                    themselves or a bare cat, and the seconds each held"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watcher_whose_pattern_is_on_its_own_line_waits_on_itself() {
        assert_eq!(classify(r#"until ! pgrep -f "cargo test"; do sleep 2; done"#), Some(Kind::SelfWait));
        assert_eq!(classify("while pgrep -f cargo.test >/dev/null; do sleep 1; done; echo done"), Some(Kind::SelfWait));
        assert_eq!(classify("while ps aux | grep 'cargo test' | grep -q .; do sleep 1; done"), Some(Kind::SelfWait));
    }

    #[test]
    fn a_pattern_the_line_does_not_spell_is_not_waiting_on_itself() {
        assert_eq!(classify(r#"until ! pgrep -f "[c]argo test"; do sleep 2; done"#), None, "the bracket keeps the line out of its own search");
        assert_eq!(classify("while pgrep cargo; do sleep 1; done"), None, "without -f it reads process names, and the shell is `zsh`");
        assert_eq!(classify("while ps aux | grep -v grep | grep 'cargo test'; do sleep 1; done"), None, "-v grep drops the shell's line too");
        assert_eq!(classify(r#"pgrep -f "cargo test""#), None, "no loop, no wait");
        assert_eq!(classify("while kill -0 123; do sleep 1; done"), None);
    }

    #[test]
    fn the_three_loops_of_wsp_611_wait_on_themselves_and_a_pid_found_before_the_loop_does_not() {
        assert_eq!(classify(r#"while pgrep -f "scripts/check.mjs" >/dev/null; do sleep 5; done"#), Some(Kind::SelfWait));
        assert_eq!(classify(r#"while kill -0 $(pgrep -f "cargo test turn_") 2>/dev/null; do sleep 5; done"#), Some(Kind::SelfWait));
        assert_eq!(classify(r#"while pgrep -f "cargo build" > /dev/null; do sleep 10; done; echo built"#), Some(Kind::SelfWait));
        // cpd-4876's loop waited on the real build's pid, found once.
        assert_eq!(classify(r#"pid=$(pgrep -f "tauri build" | head -1); while kill -0 $pid 2>/dev/null; do sleep 15; done"#), None);
        assert_eq!(classify(r#"while kill -0 $pid; do sleep 5; done; pgrep -f "cargo build""#), None, "the search after the loop is not waited on");
    }

    #[test]
    fn the_seat_is_told_why_and_to_wait_on_a_pid() {
        let advice = Kind::SelfWait.advice();
        for said in ["finds the shell running the loop", "Never wait on `pgrep -f", "wait <pid>", "while kill -0 <pid>", "run_in_background"] {
            assert!(advice.contains(said), "{said}: {advice}");
        }
    }

    #[test]
    fn a_cat_with_no_file_reads_a_terminal() {
        assert_eq!(classify("cat"), Some(Kind::BareCat));
        assert_eq!(classify("cat -"), Some(Kind::BareCat));
        assert_eq!(classify("echo hi; cat > out.txt"), Some(Kind::BareCat));
        assert_eq!(classify("cat src/main.rs"), None);
        assert_eq!(classify("cat <<'EOF' > f\nx\nEOF"), None);
        assert_eq!(classify("cat < in.txt"), None);
        assert_eq!(classify("git log | cat"), None, "the pipe is its input");
        assert_eq!(classify("cat -n a.txt b.txt 2>&1"), None);
    }

    #[test]
    fn the_matcher_reads_the_common_subset_of_a_regular_expression() {
        assert!(matches_itself("cargo.test", "run cargo test now", false));
        assert!(matches_itself("cargo (test", "x", false) == false);
        assert!(matches_itself("[a-z]+ test", "cargo test", false));
        assert!(matches_itself("nope|test", "cargo test", false));
        assert!(!matches_itself("[c]argo", "[c]argo", false));
        assert!(matches_itself("a.b", "a.b", true));
        assert!(!matches_itself("a.b", "axb", true));
    }

    fn line(kind: &str, at: &str, block: &str) -> String {
        format!(r#"{{"type":"{kind}","timestamp":"{at}","message":{{"content":[{block}]}}}}"#)
    }

    fn call(id: &str, command: &str, background: bool) -> String {
        let command = serde_json::to_string(command).unwrap();
        format!(r#"{{"type":"tool_use","id":"{id}","name":"Bash","input":{{"command":{command},"run_in_background":{background}}}}}"#)
    }

    fn result(id: &str) -> String {
        format!(r#"{{"type":"tool_result","tool_use_id":"{id}","content":"x"}}"#)
    }

    #[test]
    fn a_stall_shows_with_the_seconds_it_held() {
        let watcher = r#"until ! pgrep -f "cargo test"; do sleep 2; done"#;
        let t = [
            line("assistant", "2026-11-06T10:00:00.000Z", &call("a", watcher, false)),
            line("user", "2026-11-06T10:06:10.000Z", &result("a")),
            line("assistant", "2026-11-06T10:07:00.000Z", &call("b", "cat src/main.rs", false)),
            line("user", "2026-11-06T10:07:01.000Z", &result("b")),
            line("assistant", "2026-11-06T10:08:00.000Z", &call("c", watcher, true)),
            line("assistant", "2026-11-06T10:09:00.000Z", &call("d", "cat", false)),
        ]
        .join("\n");
        let closed = scan(&mut t.as_bytes(), 0);
        assert_eq!(closed.len(), 1, "{closed:?}");
        assert_eq!((closed[0].kind, closed[0].secs, closed[0].open), (Kind::SelfWait, 370, false));
        let now = crate::util::epoch_of("2026-11-06T10:12:00Z");
        let all = scan(&mut t.as_bytes(), now);
        assert_eq!(all.len(), 2, "a background call returns at once: {all:?}");
        assert_eq!((all[1].kind, all[1].secs, all[1].open), (Kind::BareCat, 180, true));
    }

    #[test]
    fn a_call_of_that_shape_that_came_back_at_once_is_not_a_stall() {
        let t = [
            line("assistant", "2026-11-06T10:00:00.000Z", &call("a", "cat", false)),
            line("user", "2026-11-06T10:00:02.000Z", &result("a")),
        ]
        .join("\n");
        assert!(scan(&mut t.as_bytes(), crate::util::epoch_of("2026-11-06T11:00:00Z")).is_empty());
    }

    fn call_timeout(id: &str, command: &str, timeout_ms: i64) -> String {
        let command = serde_json::to_string(command).unwrap();
        format!(r#"{{"type":"tool_use","id":"{id}","name":"Bash","input":{{"command":{command},"timeout":{timeout_ms}}}}}"#)
    }

    /// The seconds a call held, from its stamps, through `scan`.
    fn held(command: &str, timeout_ms: i64, secs: i64) -> Vec<Stall> {
        let end = crate::util::epoch_of("2026-11-06T10:00:00Z") + secs;
        let t = [
            line("assistant", "2026-11-06T10:00:00.000Z", &call_timeout("a", command, timeout_ms)),
            line("user", &crate::util::iso_at(end), &result("a")),
        ]
        .join("\n");
        scan(&mut t.as_bytes(), 0)
    }

    // The command lines below are the ones the seats ran, as the transcripts of
    // wsp-387, wsp-425 and wsp-430 (2026-11-06) hold them, with the seconds each held.
    const T387: &str = "/private/tmp/claude-501/-Users-edjames-claude-wsp--worktrees-wsp-387/4d79bb3c-6b71-45e9-8bf6-7601a3d2a5f0/tasks";
    const T425: &str = "/private/tmp/claude-501/-Users-edjames-claude-wsp--worktrees-wsp-425/0e60f7fa-d49c-49d0-8ea8-bcb597098462/tasks";
    const T430: &str = "/private/tmp/claude-501/-Users-edjames-claude-wsp--worktrees-wsp-430/602aac9d-d093-4a38-a91d-337d2229b3db/tasks";

    #[test]
    fn the_watchers_on_output_that_never_came_are_flagged_with_their_seconds() {
        let cases: Vec<(String, i64, i64, Kind)> = vec![
            // wsp-387
            (format!("until grep -q \"test result\" {T387}/bkksfjv7b.output || grep -q \"^error\" {T387}/bkksfjv7b.output; do sleep 3; done; tail -40 {T387}/bkksfjv7b.output"), 500_000, 501, Kind::WatchOutput),
            (format!("until [ -s {T387}/b9idacjuc.output ]; do sleep 5; done; cat {T387}/b9idacjuc.output"), 590_000, 591, Kind::WatchOutput),
            ("cargo test packet::tests::a_leg_ 2>&1 | tail -30".to_string(), 500_000, 502, Kind::TimedOut),
            // wsp-425
            (format!("until grep -q \"test result\\|error\" {T425}/bc9vo3wf0.output; do sleep 5; done; tail -30 {T425}/bc9vo3wf0.output"), 600_000, 602, Kind::WatchOutput),
            (format!("until [ -s {T425}/bc9vo3wf0.output ]; do sleep 10; done; tail -30 {T425}/bc9vo3wf0.output"), 590_000, 591, Kind::WatchOutput),
            (format!("F={T425}/bc9vo3wf0.output; while pgrep -f \"cargo test check::tests::a_verifiers_order\" >/dev/null; do sleep 10; done; tail -30 $F"), 590_000, 535, Kind::SelfWait),
            // wsp-430
            (format!("until grep -q \"test result\\|error\\[\" {T430}/bjxve98td.output 2>/dev/null; do sleep 5; done; tail -40 {T430}/bjxve98td.output"), 600_000, 602, Kind::WatchOutput),
            ("pgrep -lx cargo; cargo test ending:: 2>&1 | tail -30".to_string(), 600_000, 603, Kind::TimedOut),
        ];
        for (command, timeout, secs, kind) in cases {
            let got = held(&command, timeout, secs);
            assert_eq!(got.len(), 1, "{command}: {got:?}");
            assert_eq!((got[0].kind, got[0].secs, got[0].open), (kind, secs, false), "{command}");
        }
    }

    #[test]
    fn compounding_061_and_078s_commands_are_not_stalls() {
        // compounding-061: the same `while pgrep -f` watchers, but run in the
        // background, which returns at once (1.2 s in the transcript).
        let w = "while pgrep -f \"gate.sh\" >/dev/null; do sleep 15; done; cat /Users/edjames/claude/compounding/.worktrees/compounding-061/docs/workspace-port/both-ways-data/rust-landing/steps.tsv";
        let t = [
            line("assistant", "2026-11-06T10:00:00.000Z", &call("a", w, true)),
            line("user", "2026-11-06T10:00:01.000Z", &result("a")),
            line("assistant", "2026-11-06T10:00:02.000Z", &call("b", w, true)),
        ]
        .join("\n");
        assert!(scan(&mut t.as_bytes(), crate::util::epoch_of("2026-11-06T10:20:00Z")).is_empty(), "a background watcher is not in the seat's way");
        // compounding-078: a `pgrep -fl` with no loop, and a `cat > f <<EOF` with a body.
        assert_eq!(classify("ps -p 4257,34937 -o pid,pgid,command; pgrep -fl \"verifier-trial/run.py\" ; ls docs/verifier-trial/runs-data | head -100"), None);
        assert_eq!(classify("cd /x/scratchpad && mk() { cat > $1.md <<EOF\nRun part of the runs\nEOF\n}; mk a"), None);
        // A watcher that has a way out, a file that is not a task's, and a plain slow call.
        assert_eq!(classify(&format!("until [ -s {T387}/b.output ] || ! kill -0 123; do sleep 5; done")), None, "a pid ends it");
        assert_eq!(classify("until [ -s /tmp/out.txt ]; do sleep 5; done"), None, "not a task's output nor the scratch");
        assert!(held("cargo test cmd_spawn::", 600_000, 130).is_empty(), "130 s of 600 is a long call, not a held one");
        assert!(held("cargo test ending::", 120_000, 20).is_empty());
        assert_eq!(held("cargo test ending::", 120_000, 118).len(), 1, "held to a 120 s timeout of its own");
    }

    #[test]
    fn only_the_open_stalls_are_read_off_the_tail_of_a_file() {
        let dir = std::env::temp_dir().join(format!("wsp-stuck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let filler = "x".repeat(300_000);
        let mut text = format!("{{\"junk\":\"{filler}\"}}\n").repeat(10);
        text.push_str(&line("assistant", "2026-11-06T10:09:00.000Z", &call("d", "cat", false)));
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        let open = in_tail(&path, crate::util::epoch_of("2026-11-06T10:12:00Z"));
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].secs, 180);
    }
}
