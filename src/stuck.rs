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
//! # What it cannot see
//!
//! A watcher written some other way (a `lsof` loop, a `curl` retry against a
//! server that is never up) is a stall this does not name. The match of a
//! `pgrep` pattern against the command is a small matcher over the common
//! subset of regular expressions; a pattern outside it (a group) is read as not
//! matching, which is the direction that flags less.

use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom};

use serde_json::Value;

/// How long a call of one of these shapes must have been in flight to be a
/// stall, in seconds.
pub(crate) const MIN: i64 = 45;

/// How much of the end of a transcript the daemon reads for an open call.
const TAIL: u64 = 2 * 1024 * 1024;

/// What is wrong with the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A loop on `pgrep -f` or `ps | grep` whose pattern is in the loop's own line.
    SelfWait,
    /// `cat` with no file, reading a terminal.
    BareCat,
}

impl Kind {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Kind::SelfWait => "waiting on itself",
            Kind::BareCat => "bare cat on a terminal",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Kind::SelfWait => "self",
            Kind::BareCat => "cat",
        }
    }

    pub(crate) fn of_key(k: &str) -> Option<Kind> {
        match k {
            "self" => Some(Kind::SelfWait),
            "cat" => Some(Kind::BareCat),
            _ => None,
        }
    }

    /// What the seat is told to do instead.
    pub(crate) fn advice(self) -> &'static str {
        match self {
            Kind::SelfWait => {
                "its pattern is on the command's own line, so the search finds the shell running it and never comes back empty. \
                 Interrupt it, and match a pattern the line does not spell (`[c]argo test`), or wait on the pid"
            }
            Kind::BareCat => "`cat` with no file reads a terminal nobody types at. Interrupt it, and name the file",
        }
    }
}

/// A stalled call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stall {
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
pub(crate) fn scan(src: &mut impl BufRead, now: i64) -> Vec<Stall> {
    // id -> (started, command, kind)
    let mut pending: HashMap<String, (i64, String, Option<Kind>)> = HashMap::new();
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
            let Some((at, command, kind)) = pending.remove(id) else { continue };
            order.retain(|o| o != id);
            let end = field(&buf, "timestamp").map(crate::util::epoch_of).unwrap_or(0);
            if let (Some(kind), true) = (kind, end > 0) {
                let secs = end - at;
                if secs >= MIN {
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
                if pending.insert(id.to_string(), (at, command.to_string(), kind)).is_none() {
                    order.push(id.to_string());
                }
            }
        }
    }
    if now > 0 {
        for id in order {
            let Some((at, command, Some(kind))) = pending.remove(&id) else { continue };
            if at > 0 && now - at >= MIN {
                out.push(Stall { kind, command, at, secs: now - at, open: true });
            }
        }
    }
    for s in &mut out {
        s.command = one_line(&s.command);
    }
    out
}

/// The stalls in the last [`TAIL`] bytes of a transcript file, open and closed.
pub(crate) fn in_tail(path: &std::path::Path, now: i64) -> Vec<Stall> {
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(TAIL);
    if f.seek(SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if f.take(TAIL).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    // The first line is cut in the middle when the read started inside the file.
    if from > 0 {
        match text.find('\n') {
            Some(i) => text.drain(..=i),
            None => return Vec::new(),
        };
    }
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

// ---- the two shapes ---------------------------------------------------------

/// Which of the two shapes a command is, when it is one.
pub(crate) fn classify(command: &str) -> Option<Kind> {
    let simple = simple_commands(command);
    if waits_on_itself(command, &simple) {
        return Some(Kind::SelfWait);
    }
    if simple.iter().any(|c| bare_cat(&c.words, c.after_pipe)) {
        return Some(Kind::BareCat);
    }
    None
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
    end_command!(false);
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
    for c in simple {
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

const USAGE: &str = "usage: wsp stuck <transcript.jsonl>…   the commands in these transcripts that waited on themselves or on a bare cat, with the seconds each held";

/// `wsp stuck FILE…`: the stalls in whole transcripts, one line each and a
/// total, which is how the rate is counted (`wsp-397`).
pub fn stuck(args: &crate::Args) -> i32 {
    if args.rest.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
    let (mut files, mut hit, mut secs) = (0, 0, 0);
    let mut lines = Vec::new();
    for path in &args.rest {
        let Ok(f) = std::fs::File::open(path) else {
            eprintln!("wsp: cannot read {path}");
            continue;
        };
        files += 1;
        let stalls = scan(&mut std::io::BufReader::new(f), 0);
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
