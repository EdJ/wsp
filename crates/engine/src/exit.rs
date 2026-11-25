//! The exit: what a seat writes on its way out, as one type (`wsp-612`, row E1
//! of docs/exit-packet.md). No verb and no IO here — `wsp submit` (E2) reads a
//! file, runs the validator and acts; this module is the contract they share.
//!
//! # One type, two forms
//!
//! [`Exit`] holds the four kinds' exits (member, verifier, barrier check, leg).
//! Its text form is the file a seat writes to `$WSP_EXIT`: fixed sections, in
//! this order, each starting at the beginning of a line —
//!
//! ```text
//! verdict: holds | blocks          (verifier; a check's is go | hold | followup)
//! summary: <one line>
//!
//! <the account, in prose>
//!
//! done-when:
//! - held: <item> — <where>
//! - not held: <item> — <why>
//!
//! checks:                          (a verifier's and a check's are `checked:`)
//! - <label> (<time>): <command>
//!
//! cause: work | check | tree | env — <one sentence>   (verifier, check)
//!
//! asks:
//! - governor: … / - person: … / - note: …
//!
//! owed: nothing                    (or `owed:` and one line per thing left)
//! ```
//!
//! [`Exit::parse`] reads it and [`Exit::render`] writes it back, so the two
//! round-trip. Its JSON form ([`Exit::json`], [`Exit::from_json`]) is the same
//! type, as wsp-483's `submit` message will carry it; reading JSON renders it
//! and parses the text again, so a payload obeys the file's grammar exactly.
//!
//! # Refused, never guessed
//!
//! Anything outside a known section is refused, and every refusal is returned
//! at once ([`Refusal`]): its line, its section, what was wrong and the text to
//! write instead. A check line is read by [`check::timed_line`], the one
//! grammar `## Checks`, `wsp check --set` and `--stop` lines use too; a line it
//! cannot read is refused here and never becomes a command.
//!
//! The kind is the caller's (`$WSP_SEAT_KIND`), never the file's, so a member
//! cannot write a verdict.

// E2 (`wsp submit`) is the first caller outside the tests.
#![cfg_attr(not(test), allow(dead_code))]

use crate::check;
use serde_json::{json, Value};
use std::fmt;

/// Whose exit it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Member,
    Verifier,
    Check,
    Leg,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Member => "member",
            Kind::Verifier => "verifier",
            Kind::Check => "check",
            Kind::Leg => "leg",
        }
    }

    pub fn of(word: &str) -> Option<Kind> {
        [Kind::Member, Kind::Verifier, Kind::Check, Kind::Leg].into_iter().find(|k| k.word() == word)
    }

    /// The verdicts this kind may write; none for a member or a leg.
    fn verdicts(self) -> &'static [Verdict] {
        match self {
            Kind::Verifier => &[Verdict::Holds, Verdict::Blocks],
            Kind::Check => &[Verdict::Go, Verdict::Hold, Verdict::Followup],
            Kind::Member | Kind::Leg => &[],
        }
    }

    /// `checks:` for a member or a leg, `checked:` for a verifier or a check.
    fn checks_word(self) -> &'static str {
        if self.judges() { "checked" } else { "checks" }
    }

    /// A verifier or a check: it writes a verdict, what it checked, and a cause.
    fn judges(self) -> bool {
        matches!(self, Kind::Verifier | Kind::Check)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Holds,
    Blocks,
    Go,
    Hold,
    Followup,
}

impl Verdict {
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Holds => "holds",
            Verdict::Blocks => "blocks",
            Verdict::Go => "go",
            Verdict::Hold => "hold",
            Verdict::Followup => "followup",
        }
    }
}

/// What a block or a hold is the fault of, which routes it (docs/exit-packet.md §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The change is wrong or incomplete: sent back with `owed:`.
    Work,
    /// A check failed, and `checked:` names it: sent back the same way.
    Check,
    /// Commits missing or the tree dirty: sent back, and a defect in the gate.
    Tree,
    /// The verifier could not verify: not a block of the work.
    Env,
}

const CAUSES: [Cause; 4] = [Cause::Work, Cause::Check, Cause::Tree, Cause::Env];

impl Cause {
    pub fn word(self) -> &'static str {
        match self {
            Cause::Work => "work",
            Cause::Check => "check",
            Cause::Tree => "tree",
            Cause::Env => "env",
        }
    }
}

/// One `done-when:` line: the item, whether it held, and where or why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    pub held: bool,
    pub item: String,
    pub reason: String,
}

/// What a verifier's or a check's run of a line came to: `pass` or `fail`,
/// and anything said after it (`reran once at load 9`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran {
    pub passed: bool,
    pub note: Option<String>,
}

/// One `checks:` or `checked:` line, `label (time): command`, with what it
/// came to on a `checked:` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub label: String,
    /// As written: `90s`, `5m`, `2h`.
    pub time: String,
    pub command: String,
    pub ran: Option<Ran>,
}

/// Who an ask is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To {
    /// A decision the list's governor can make.
    Governor,
    /// A decision only the person can make.
    Person,
    /// Worth reading; asks nothing.
    Note,
}

const TOS: [To; 3] = [To::Governor, To::Person, To::Note];

impl To {
    pub fn word(self) -> &'static str {
        match self {
            To::Governor => "governor",
            To::Person => "person",
            To::Note => "note",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub to: To,
    pub text: String,
}

impl Ask {
    /// A `governor:` or `person:` line: something somebody must decide.
    pub fn decides(&self) -> bool {
        self.to != To::Note
    }
}

/// A seat's exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub kind: Kind,
    /// A verifier's or a check's; never a member's or a leg's.
    pub verdict: Option<Verdict>,
    pub summary: String,
    /// The prose under the summary, blank lines at its ends taken off.
    pub account: String,
    /// `None` where the section is absent (a leg may leave it out).
    pub done_when: Option<Vec<Done>>,
    /// `checks:` for a member or a leg, `checked:` (with [`Line::ran`]) for a
    /// verifier or a check.
    pub checks: Vec<Line>,
    /// A verifier's or a check's, with its sentence.
    pub cause: Option<(Cause, String)>,
    pub asks: Vec<Ask>,
    /// `None` where the section is absent; empty for `owed: nothing`.
    pub owed: Option<Vec<String>>,
}

/// What a member's exit makes of its row (docs/exit-packet.md §4), read from
/// the file rather than chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every item held: to review, and the verifier runs.
    Ready,
    /// An item not held, and a decision asked: blocked on these asks.
    Blocked(Vec<Ask>),
    /// An item not held and only `owed:` for it: to review as partial.
    Partial,
    /// An item not held with neither a decision ask nor an `owed:` line, which
    /// the validator refuses (V7).
    Unowed,
}

impl Exit {
    /// A member's outcome by the §4 table.
    pub fn outcome(&self) -> Outcome {
        let not_held = self.done_when.iter().flatten().any(|d| !d.held);
        if !not_held {
            return Outcome::Ready;
        }
        let decisions: Vec<Ask> = self.asks.iter().filter(|a| a.decides()).cloned().collect();
        if !decisions.is_empty() {
            Outcome::Blocked(decisions)
        } else if self.owed.as_ref().is_some_and(|o| !o.is_empty()) {
            Outcome::Partial
        } else {
            Outcome::Unowed
        }
    }
}

// ---- refusals --------------------------------------------------------------

/// Why a line of an exit was refused: where, in which section, what was
/// wrong, and what to write instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// 1-based; the line after the last for something missing at the end.
    pub line: usize,
    pub section: String,
    pub said: String,
    pub write: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {} ({}): {} — write {}", self.line, self.section, self.said, self.write)
    }
}

// ---- sections --------------------------------------------------------------

/// The sections, in the order they must come.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Verdict,
    Summary,
    Account,
    DoneWhen,
    Checks,
    Cause,
    Asks,
    Owed,
}

impl Section {
    fn word(self, kind: Kind) -> &'static str {
        match self {
            Section::Verdict => "verdict",
            Section::Summary => "summary",
            Section::Account => "the account",
            Section::DoneWhen => "done-when",
            Section::Checks => kind.checks_word(),
            Section::Cause => "cause",
            Section::Asks => "asks",
            Section::Owed => "owed",
        }
    }

    /// The section a heading names, whichever kind may write it.
    fn named(name: &str) -> Option<Section> {
        Some(match name {
            "verdict" => Section::Verdict,
            "summary" => Section::Summary,
            "done-when" => Section::DoneWhen,
            "checks" | "checked" => Section::Checks,
            "cause" => Section::Cause,
            "asks" => Section::Asks,
            "owed" => Section::Owed,
            _ => return None,
        })
    }

    /// The sections a kind's exit is made of, as a refusal lists them.
    fn list(kind: Kind) -> String {
        let all = [Section::Verdict, Section::Summary, Section::DoneWhen, Section::Checks, Section::Cause, Section::Asks, Section::Owed];
        all.iter().filter(|s| s.allowed(kind)).map(|s| s.word(kind)).collect::<Vec<_>>().join(", ")
    }

    fn allowed(self, kind: Kind) -> bool {
        match self {
            Section::Verdict | Section::Cause => kind.judges(),
            _ => true,
        }
    }
}

/// A line that reads as a heading, `name:` or `name: value`, at the start of
/// the line: its name and what follows the colon.
fn heading(line: &str) -> Option<(&str, &str)> {
    if line.starts_with(char::is_whitespace) || line.starts_with('-') {
        return None;
    }
    let (name, rest) = line.split_once(':')?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let ok = name.chars().next().is_some_and(char::is_alphabetic)
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        && name.split_whitespace().count() <= 3;
    ok.then(|| (name.trim(), rest.trim()))
}

/// The text after `- ` on an item line.
fn item(line: &str) -> Option<&str> {
    let l = line.trim_start();
    l.strip_prefix("- ").map(str::trim).or_else(|| (l == "-").then_some(""))
}

/// A refusal at `n` in `section`.
fn refuse(n: usize, section: &str, said: impl Into<String>, write: impl Into<String>) -> Refusal {
    Refusal { line: n, section: section.to_string(), said: said.into(), write: write.into() }
}

const ASK_SHAPE: &str = "`- governor: …`, `- person: …` or `- note: …`";
const DONE_SHAPE: &str = "`- held: <item> — <where>` or `- not held: <item> — <why>`";

impl Exit {
    /// Read an exit of `kind` from its text, or every refusal in it at once.
    pub fn parse(kind: Kind, text: &str) -> Result<Exit, Vec<Refusal>> {
        let mut exit = Exit {
            kind,
            verdict: None,
            summary: String::new(),
            account: String::new(),
            done_when: None,
            checks: Vec::new(),
            cause: None,
            asks: Vec::new(),
            owed: None,
        };
        let mut out: Vec<Refusal> = Vec::new();
        let mut at: Option<Section> = None;
        let mut seen: Vec<Section> = Vec::new();
        let mut summary_at = None;
        let mut account: Vec<&str> = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        for (i, raw) in lines.iter().enumerate() {
            let n = i + 1;
            let line = raw.trim_end();
            let here = at.map_or("before summary", |s| s.word(kind));
            if let Some((name, rest)) = heading(line) {
                if let Some(s) = Section::named(name) {
                    if let Some(r) = exit.open(s, name, rest, n, at, &seen) {
                        out.push(r);
                        // A refused heading still opens its section, so its items are read and
                        // refused by their own lines rather than as stray text.
                    }
                    if s == Section::Summary {
                        summary_at = Some(n);
                    }
                    at = Some(if s == Section::Summary { Section::Account } else { s });
                    seen.push(s);
                    continue;
                }
                if rest.is_empty() || at != Some(Section::Account) {
                    out.push(refuse(
                        n,
                        here,
                        format!("\"{name}:\" is not a section of the exit"),
                        format!("it inside a section — the sections are {}", Section::list(kind)),
                    ));
                    continue;
                }
            }
            match at {
                Some(Section::Account) => account.push(line),
                _ if line.trim().is_empty() => {}
                Some(s @ (Section::DoneWhen | Section::Checks | Section::Asks | Section::Owed)) => match item(line) {
                    Some(it) => {
                        if let Err(r) = exit.item(s, it, n) {
                            out.push(r);
                        }
                    }
                    None => out.push(refuse(n, s.word(kind), format!("\"{}\" is not an item", line.trim()), format!("it as `- …` under {}:", s.word(kind)))),
                },
                _ => out.push(refuse(
                    n,
                    here,
                    format!("\"{}\" is outside a section", line.trim()),
                    format!("it in the account, under `summary:` — the sections are {}", Section::list(kind)),
                )),
            }
        }
        let end = lines.len() + 1;
        if summary_at.is_none() {
            let n = seen.first().and(lines.iter().position(|l| heading(l.trim_end()).is_some()).map(|i| i + 1)).unwrap_or(1);
            out.push(refuse(n, "summary", "the exit has no `summary:`", "`summary: <one line: what is true now>` first (after `verdict:`)"));
        }
        if kind.judges() && !seen.contains(&Section::Verdict) {
            let words: Vec<_> = kind.verdicts().iter().map(|v| v.word()).collect();
            out.push(refuse(1, "verdict", format!("a {}'s exit has no `verdict:`", kind.word()), format!("`verdict: {}` on the first line", words.join(" | "))));
        }
        if kind == Kind::Member && !seen.contains(&Section::DoneWhen) {
            out.push(refuse(end, "done-when", "a member's exit has no `done-when:`", format!("`done-when:` and one {DONE_SHAPE} per item of the packet's Done when")));
        }
        while account.first().is_some_and(|l| l.trim().is_empty()) {
            account.remove(0);
        }
        while account.last().is_some_and(|l| l.trim().is_empty()) {
            account.pop();
        }
        exit.account = account.join("\n");
        if out.is_empty() {
            Ok(exit)
        } else {
            out.sort_by_key(|r| r.line);
            Err(out)
        }
    }

    /// Open section `s` at line `n` from its heading `name: rest`, or say why not.
    fn open(&mut self, s: Section, name: &str, rest: &str, n: usize, at: Option<Section>, seen: &[Section]) -> Option<Refusal> {
        let kind = self.kind;
        let word = s.word(kind);
        if !s.allowed(kind) {
            let w = if s == Section::Verdict { "nothing: the verdict is the verifier's" } else { "nothing: a cause is a verifier's or a check's" };
            return Some(refuse(n, word, format!("a {}'s exit has no `{name}:`", kind.word()), w));
        }
        if s == Section::Checks && name != kind.checks_word() {
            return Some(refuse(n, word, format!("a {}'s checks are `{}:`, not `{name}:`", kind.word(), kind.checks_word()), format!("`{}:`", kind.checks_word())));
        }
        if seen.contains(&s) {
            return Some(refuse(n, word, format!("`{name}:` comes twice"), format!("one `{name}:`, with every item under it")));
        }
        let last = at.map(|a| if a == Section::Account { Section::Summary } else { a });
        if let Some(last) = last.filter(|l| *l > s) {
            return Some(refuse(
                n,
                word,
                format!("`{name}:` comes after `{}:`", last.word(kind)),
                format!("the sections in order — {}", Section::list(kind)),
            ));
        }
        match s {
            Section::Verdict => {
                let words: Vec<_> = kind.verdicts().iter().map(|v| v.word()).collect();
                match kind.verdicts().iter().find(|v| v.word() == rest) {
                    Some(v) => self.verdict = Some(*v),
                    None => return Some(refuse(n, word, format!("\"{rest}\" is not a verdict"), format!("`verdict: {}`", words.join(" | ")))),
                }
            }
            Section::Summary => {
                if rest.is_empty() {
                    return Some(refuse(n, word, "the summary is empty", "`summary: <one line: what is true now>`"));
                }
                self.summary = rest.to_string();
            }
            Section::Cause => {
                let all: Vec<_> = CAUSES.iter().map(|c| c.word()).collect();
                let shape = format!("`cause: {} — <one sentence>`", all.join(" | "));
                let (c, why) = rest.split_once(" — ").map_or((rest, ""), |(c, w)| (c.trim(), w.trim()));
                match CAUSES.iter().find(|x| x.word() == c) {
                    Some(c) if !why.is_empty() => self.cause = Some((*c, why.to_string())),
                    Some(_) => return Some(refuse(n, word, "the cause carries no sentence after ` — `", shape)),
                    None => return Some(refuse(n, word, format!("\"{c}\" is not a cause"), shape)),
                }
            }
            Section::Owed => {
                self.owed = Some(Vec::new());
                if !rest.is_empty() && rest != "nothing" {
                    return Some(refuse(n, word, format!("\"{rest}\" on the `owed:` line"), "`owed: nothing`, or `owed:` and `- <what is left>` on the lines under it"));
                }
            }
            Section::DoneWhen | Section::Checks | Section::Asks => {
                if s == Section::DoneWhen {
                    self.done_when = Some(Vec::new());
                }
                if !rest.is_empty() {
                    return Some(refuse(n, word, format!("\"{rest}\" on the `{name}:` line"), format!("`{name}:` alone, and each item as `- …` on a line under it")));
                }
            }
            Section::Account => {}
        }
        None
    }

    /// Read one `- …` item of section `s`, at line `n`.
    fn item(&mut self, s: Section, it: &str, n: usize) -> Result<(), Refusal> {
        let kind = self.kind;
        let word = s.word(kind);
        match s {
            Section::DoneWhen => {
                let (held, rest) = if let Some(r) = it.strip_prefix("held:") {
                    (true, r)
                } else if let Some(r) = it.strip_prefix("not held:") {
                    (false, r)
                } else {
                    return Err(refuse(n, word, format!("\"{it}\" opens with neither `held:` nor `not held:`"), DONE_SHAPE));
                };
                let Some((item, reason)) = rest.split_once(" — ").map(|(a, b)| (a.trim(), b.trim())).filter(|(a, b)| !a.is_empty() && !b.is_empty()) else {
                    return Err(refuse(n, word, format!("\"{it}\" carries no item and reason either side of ` — `"), DONE_SHAPE));
                };
                self.done_when.get_or_insert_with(Vec::new).push(Done { held, item: item.to_string(), reason: reason.to_string() });
            }
            Section::Checks => {
                let (line, ran) = if kind.judges() {
                    match ran_of(it) {
                        Some(split) => split,
                        None => {
                            return Err(refuse(
                                n,
                                word,
                                format!("\"{it}\" does not say what it came to"),
                                "`- <label> (<time>): <command> — pass` or `… — fail [— <note>]`",
                            ))
                        }
                    }
                } else {
                    (it, None)
                };
                if line.starts_with("full-suite:") {
                    return Err(refuse(
                        n,
                        word,
                        "the full-suite override is not an exit's line",
                        "the narrow command for what you touched; when the full suite may run is the project's to say, beside its `## Checks`",
                    ));
                }
                let (label, time, command) = check::timed_line(line).map_err(|why| refuse(n, word, why, "`- <label> (<time>): <command>`"))?;
                self.checks.push(Line { label, time, command, ran });
            }
            Section::Asks => {
                let typed = TOS.iter().find_map(|t| it.strip_prefix(t.word()).and_then(|r| r.strip_prefix(':')).map(|r| (*t, r.trim())));
                match typed {
                    Some((to, text)) if !text.is_empty() => self.asks.push(Ask { to, text: text.to_string() }),
                    Some((to, _)) => return Err(refuse(n, word, format!("the `{}:` ask is empty", to.word()), ASK_SHAPE)),
                    None => return Err(refuse(n, word, format!("\"{it}\" is not typed"), ASK_SHAPE)),
                }
            }
            Section::Owed => {
                let owed = self.owed.get_or_insert_with(Vec::new);
                if it.is_empty() {
                    return Err(refuse(n, word, "an empty `-`", "`- <what is left>`, or `owed: nothing`"));
                }
                if it == "nothing" {
                    return Ok(());
                }
                owed.push(it.to_string());
            }
            _ => {}
        }
        Ok(())
    }

    /// The exit as its file: what [`Exit::parse`] reads back to the same value.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(v) = self.verdict {
            out.push_str(&format!("verdict: {}\n", v.word()));
        }
        out.push_str(&format!("summary: {}\n", self.summary));
        if !self.account.is_empty() {
            out.push_str(&format!("\n{}\n", self.account));
        }
        if let Some(done) = &self.done_when {
            out.push_str("\ndone-when:\n");
            for d in done {
                out.push_str(&format!("- {}: {} — {}\n", if d.held { "held" } else { "not held" }, d.item, d.reason));
            }
        }
        if !self.checks.is_empty() {
            out.push_str(&format!("\n{}:\n", self.kind.checks_word()));
            for l in &self.checks {
                out.push_str(&format!("- {} ({}): {}", l.label, l.time, l.command));
                if let Some(r) = &l.ran {
                    out.push_str(if r.passed { " — pass" } else { " — fail" });
                    if let Some(note) = &r.note {
                        out.push_str(&format!(" — {note}"));
                    }
                }
                out.push('\n');
            }
        }
        if let Some((c, why)) = &self.cause {
            out.push_str(&format!("\ncause: {} — {why}\n", c.word()));
        }
        if !self.asks.is_empty() {
            out.push_str("\nasks:\n");
            for a in &self.asks {
                out.push_str(&format!("- {}: {}\n", a.to.word(), a.text));
            }
        }
        match &self.owed {
            Some(o) if o.is_empty() => out.push_str("\nowed: nothing\n"),
            Some(o) => {
                out.push_str("\nowed:\n");
                for x in o {
                    out.push_str(&format!("- {x}\n"));
                }
            }
            None => {}
        }
        out
    }

    /// The exit as JSON: the same type, as a `submit` payload carries it.
    pub fn json(&self) -> Value {
        json!({
            "kind": self.kind.word(),
            "verdict": self.verdict.map(Verdict::word),
            "summary": self.summary,
            "account": self.account,
            "done_when": self.done_when.as_ref().map(|d| d.iter().map(|d| json!({"held": d.held, "item": d.item, "reason": d.reason})).collect::<Vec<_>>()),
            "checks": self.checks.iter().map(|l| json!({
                "label": l.label,
                "time": l.time,
                "command": l.command,
                "ran": l.ran.as_ref().map(|r| json!({"passed": r.passed, "note": r.note})),
            })).collect::<Vec<_>>(),
            "cause": self.cause.as_ref().map(|(c, why)| json!({"cause": c.word(), "why": why})),
            "asks": self.asks.iter().map(|a| json!({"to": a.to.word(), "text": a.text})).collect::<Vec<_>>(),
            "owed": self.owed,
        })
    }

    /// Read an exit from its JSON. The value is rendered and parsed again, so
    /// it is refused exactly where the same file would be (a check line with
    /// no time, an empty summary); a field of the wrong shape is a refusal at
    /// line 0, naming the field.
    pub fn from_json(v: &Value) -> Result<Exit, Vec<Refusal>> {
        let shape = |field: &str, write: &str| vec![refuse(0, field, format!("`{field}` is missing or not the right shape"), write.to_string())];
        let s = |v: &Value, f: &str| v.get(f).and_then(Value::as_str).map(str::to_string);
        let kind = s(v, "kind").and_then(|k| Kind::of(&k)).ok_or_else(|| shape("kind", "\"member\", \"verifier\", \"check\" or \"leg\""))?;
        let verdict = match v.get("verdict") {
            None | Some(Value::Null) => None,
            Some(x) => Some(
                [Verdict::Holds, Verdict::Blocks, Verdict::Go, Verdict::Hold, Verdict::Followup]
                    .into_iter()
                    .find(|w| x.as_str() == Some(w.word()))
                    .ok_or_else(|| shape("verdict", "a verdict word, or null"))?,
            ),
        };
        let done_when = match v.get("done_when") {
            None | Some(Value::Null) => None,
            Some(Value::Array(d)) => Some(
                d.iter()
                    .map(|d| Some(Done { held: d.get("held")?.as_bool()?, item: s(d, "item")?, reason: s(d, "reason")? }))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| shape("done_when", "[{\"held\": bool, \"item\": \"…\", \"reason\": \"…\"}]"))?,
            ),
            Some(_) => return Err(shape("done_when", "an array, or null")),
        };
        let arr = |f: &str| v.get(f).and_then(Value::as_array).cloned().unwrap_or_default();
        let checks = arr("checks")
            .iter()
            .map(|l| {
                let ran = match l.get("ran") {
                    None | Some(Value::Null) => None,
                    Some(r) => Some(Ran { passed: r.get("passed")?.as_bool()?, note: s(r, "note") }),
                };
                Some(Line { label: s(l, "label")?, time: s(l, "time")?, command: s(l, "command")?, ran })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| shape("checks", "[{\"label\": \"…\", \"time\": \"5m\", \"command\": \"…\", \"ran\": null}]"))?;
        let cause = match v.get("cause") {
            None | Some(Value::Null) => None,
            Some(c) => Some(
                s(c, "cause")
                    .and_then(|w| CAUSES.into_iter().find(|x| x.word() == w))
                    .zip(s(c, "why"))
                    .ok_or_else(|| shape("cause", "{\"cause\": \"work|check|tree|env\", \"why\": \"…\"}, or null"))?,
            ),
        };
        let asks = arr("asks")
            .iter()
            .map(|a| Some(Ask { to: TOS.into_iter().find(|t| s(a, "to").as_deref() == Some(t.word()))?, text: s(a, "text")? }))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| shape("asks", "[{\"to\": \"governor|person|note\", \"text\": \"…\"}]"))?;
        let owed = match v.get("owed") {
            None | Some(Value::Null) => None,
            Some(Value::Array(o)) => Some(o.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>().ok_or_else(|| shape("owed", "[\"…\"], or null"))?),
            Some(_) => return Err(shape("owed", "an array of strings, or null")),
        };
        let exit = Exit {
            kind,
            verdict,
            summary: s(v, "summary").unwrap_or_default(),
            account: s(v, "account").unwrap_or_default(),
            done_when,
            checks,
            cause,
            asks,
            owed,
        };
        let back = Exit::parse(kind, &exit.render())?;
        if back != exit {
            return Err(vec![refuse(0, "exit", "the payload does not read back as itself as a file (a line break inside a one-line field?)", "each one-line field on one line")]);
        }
        Ok(exit)
    }
}

/// A `checked:` item split at what it came to: `… — pass` or `… — fail [— note]`.
fn ran_of(it: &str) -> Option<(&str, Option<Ran>)> {
    let mut best = None;
    for (word, passed) in [(" — pass", true), (" — fail", false)] {
        for (i, _) in it.match_indices(word) {
            let rest = &it[i + word.len()..];
            let note = if rest.is_empty() {
                None
            } else if let Some(n) = rest.strip_prefix(" — ").map(str::trim).filter(|n| !n.is_empty()) {
                Some(n.to_string())
            } else {
                continue;
            };
            if best.as_ref().is_none_or(|b: &(usize, bool, Option<String>)| i > b.0) {
                best = Some((i, passed, note));
            }
        }
    }
    best.map(|(i, passed, note)| (it[..i].trim(), Some(Ran { passed, note })))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMBER: &str = "\
summary: the exit is one type, parsed and rendered

src/exit.rs holds the four kinds' exits.
Its check lines go through check::timed_line.

done-when:
- held: each kind round-trips — exit::tests::each_kind_round_trips_through_text_and_json
- not held: E2's validator — another row's

checks:
- exit (5m): cargo test exit::
- web-chrome ctl and workspace (5m): cd spikes/web-chrome && npx tsc --noEmit

asks:
- governor: may E2 start before the barrier?
- note: wsp-609's parser is the one used

owed:
- E2: the validator and `wsp submit`
";

    const VERIFIER: &str = "\
verdict: blocks
summary: the summary line is not checked for length

I read src/exit.rs and ran the module's tests.

done-when:
- held: round trip — ran exit::
- not held: a missing summary fails with its line — src/exit.rs:310

checked:
- exit (5m): cargo test exit:: — fail — reran once at load 9
- check (10m): cargo test check:: — pass

cause: work — the summary's rule is missing

owed:
- refuse a missing summary with its line number
";

    const CHECK: &str = "\
verdict: hold
summary: the barrier's suite failed on one test

checked:
- full suite (30m): cargo nextest run --workspace — fail

cause: check — cycle::a_test failed twice

owed: nothing
";

    const LEG: &str = "\
summary: the parser is in; the render is the next leg's

Leg one of two.

checks:
- exit parse (2m): cargo test exit::parse

owed:
- the render
";

    #[test]
    fn each_kind_round_trips_through_text_and_json() {
        for (kind, text) in [(Kind::Member, MEMBER), (Kind::Verifier, VERIFIER), (Kind::Check, CHECK), (Kind::Leg, LEG)] {
            let exit = Exit::parse(kind, text).unwrap_or_else(|r| panic!("{kind:?}: {r:#?}"));
            assert_eq!(exit.render(), text, "{kind:?}: the text renders back as written");
            assert_eq!(Exit::parse(kind, &exit.render()).unwrap(), exit);
            let json = exit.json();
            let back = Exit::from_json(&serde_json::from_str(&json.to_string()).unwrap()).unwrap_or_else(|r| panic!("{kind:?}: {r:#?}"));
            assert_eq!(back, exit, "{kind:?}: the JSON reads back as the same exit");
        }
        let v = Exit::parse(Kind::Verifier, VERIFIER).unwrap();
        assert_eq!(v.verdict, Some(Verdict::Blocks));
        assert_eq!(v.cause, Some((Cause::Work, "the summary's rule is missing".into())));
        assert_eq!(v.checks[0].ran, Some(Ran { passed: false, note: Some("reran once at load 9".into()) }));
        assert_eq!(v.checks[1].ran, Some(Ran { passed: true, note: None }));
        assert!(v.asks.is_empty());
        assert!(Exit::parse(Kind::Verifier, &VERIFIER.replace("\nowed:", "\nasks:\n\nowed:")).unwrap().asks.is_empty(), "an empty `asks:` asks nothing");
        assert_eq!(Exit::parse(Kind::Check, CHECK).unwrap().owed, Some(vec![]), "`owed: nothing` is nothing owed");
        assert_eq!(Exit::parse(Kind::Leg, LEG).unwrap().done_when, None, "a leg's done-when is optional");
    }

    #[test]
    fn a_label_of_several_words_parses_and_only_its_command_is_the_command() {
        // compounding-442: `web-chrome ctl and workspace: cd …` ran `web-chrome`.
        let exit = Exit::parse(Kind::Member, MEMBER).unwrap();
        let l = &exit.checks[1];
        assert_eq!((l.label.as_str(), l.time.as_str()), ("web-chrome ctl and workspace", "5m"));
        assert_eq!(l.command, "cd spikes/web-chrome && npx tsc --noEmit");
        let shellish = "summary: s\n\ndone-when:\n- held: a — b\n\nchecks:\n- lint & types (front) (90s): npm run lint\n";
        let l = &Exit::parse(Kind::Member, shellish).unwrap().checks[0];
        assert_eq!((l.label.as_str(), l.time.as_str(), l.command.as_str()), ("lint & types (front)", "90s", "npm run lint"), "the label is all before the last ` (time): `");
    }

    #[test]
    fn a_check_line_without_a_time_is_refused_never_a_command() {
        for bad in [
            "- web-chrome ctl and workspace: cd spikes/web-chrome && npx tsc --noEmit",
            "- cargo test exit::",
            "- slow (soon): cargo test",
            "- tests (5m) at barrier: cargo test",
            "- tests (café): cargo test",
            "- done (✓): true",
        ] {
            let text = format!("summary: s\n\ndone-when:\n- held: a — b\n\nchecks:\n{bad}\n");
            let r = Exit::parse(Kind::Member, &text).expect_err(bad);
            assert_eq!(r.len(), 1, "{r:#?}");
            assert_eq!((r[0].line, r[0].section.as_str()), (7, "checks"), "{bad}");
            assert!(r[0].write.contains("(<time>)"), "{}", r[0]);
        }
        let r = Exit::parse(Kind::Member, "summary: s\n\ndone-when:\n- held: a — b\n\nchecks:\n- full-suite: shared module\n").unwrap_err();
        assert!(r[0].write.contains("project"), "D4: the override is the project's: {}", r[0]);
        let mut j = Exit::parse(Kind::Member, MEMBER).unwrap().json();
        j["checks"][0]["time"] = json!("soon");
        let r = Exit::from_json(&j).unwrap_err();
        assert!(r.iter().any(|r| r.section == "checks"), "JSON is read by the same grammar: {r:#?}");
    }

    #[test]
    fn an_unknown_section_a_missing_summary_and_an_untyped_ask_fail_with_their_line() {
        let unknown = "summary: s\n\nbody\n\ndone-when:\n- held: a — b\n\nTests:\n- cargo test\n";
        let r = Exit::parse(Kind::Member, unknown).unwrap_err();
        assert_eq!((r[0].line, r[0].section.as_str()), (8, "done-when"), "{r:#?}");
        assert!(r[0].said.contains("\"Tests:\" is not a section") && r[0].write.contains("summary, done-when, checks, asks, owed"), "{}", r[0]);
        let r = Exit::parse(Kind::Member, "summary: s\n\nTests:\n").unwrap_err();
        assert_eq!(r[0].line, 3, "a bare heading in the account is a section that is not one: {r:#?}");
        assert!(Exit::parse(Kind::Member, "summary: s\n\nNote: prose may carry a colon.\n\ndone-when:\n- held: a — b\n").is_ok());

        let missing = "\nwhat I did\n\ndone-when:\n- held: a — b\n";
        let r = Exit::parse(Kind::Member, missing).unwrap_err();
        assert!(r.iter().any(|r| r.line == 2 && r.section == "before summary"), "the stray prose is refused at its line: {r:#?}");
        assert!(r.iter().any(|r| r.section == "summary" && r.line == 4 && r.write.starts_with("`summary:")), "{r:#?}");

        let untyped = "summary: s\n\ndone-when:\n- held: a — b\n\nasks:\n- note: fine\n- should E2 wait?\n";
        let r = Exit::parse(Kind::Member, untyped).unwrap_err();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].line, r[0].section.as_str()), (8, "asks"));
        assert!(r[0].said.contains("not typed") && r[0].write.contains("governor:"), "{}", r[0]);
        assert!(r[0].to_string().starts_with("line 8 (asks): "), "{}", r[0]);
    }

    #[test]
    fn every_refusal_is_returned_at_once_and_each_kind_keeps_its_own_sections() {
        let text = "summary: s\n\ndone-when:\n- held: a\n- maybe: b — c\n\nchecked:\n- t (1m): true — pass\n\nowed:\n- x\n\nasks:\n";
        let r = Exit::parse(Kind::Member, text).unwrap_err();
        let lines: Vec<_> = r.iter().map(|r| r.line).collect();
        assert_eq!(lines, [4, 5, 7, 13], "{r:#?}");
        assert!(r[2].write.contains("`checks:`") && r[3].said.contains("after `owed:`"), "{r:#?}");

        let r = Exit::parse(Kind::Member, "verdict: holds\nsummary: s\n\ndone-when:\n- held: a — b\n").unwrap_err();
        assert_eq!((r[0].line, r[0].section.as_str()), (1, "verdict"), "a member cannot write a verdict");
        let r = Exit::parse(Kind::Verifier, "summary: s\n").unwrap_err();
        assert!(r[0].said.contains("no `verdict:`") && r[0].write.contains("holds | blocks"), "{r:#?}");
        let r = Exit::parse(Kind::Check, "verdict: holds\nsummary: s\n").unwrap_err();
        assert!(r[0].write.contains("go | hold | followup"), "{r:#?}");
        let r = Exit::parse(Kind::Verifier, "verdict: blocks\nsummary: s\n\nchecked:\n- t (1m): true\n\ncause: flaky — x\n").unwrap_err();
        assert_eq!(r.iter().map(|r| r.line).collect::<Vec<_>>(), [5, 7], "{r:#?}");
        let r = Exit::parse(Kind::Member, "summary: s\n\nbody\n").unwrap_err();
        assert_eq!((r[0].line, r[0].section.as_str()), (4, "done-when"), "a member's done-when is required");
    }

    #[test]
    fn the_outcome_is_read_from_the_file() {
        let base = "summary: s\n\nbody\n\ndone-when:\n- held: a — t\n";
        let ready = Exit::parse(Kind::Member, &format!("{base}\nowed: nothing\n")).unwrap();
        assert_eq!(ready.outcome(), Outcome::Ready);
        let notes_only = Exit::parse(Kind::Member, &format!("{base}- not held: b — waits\n\nasks:\n- note: fyi\n\nowed:\n- b\n")).unwrap();
        assert_eq!(notes_only.outcome(), Outcome::Partial, "a note decides nothing; owed makes it partial");
        let blocked = Exit::parse(Kind::Member, &format!("{base}- not held: b — waits\n\nasks:\n- person: which store?\n- note: fyi\n\nowed:\n- b\n")).unwrap();
        assert_eq!(blocked.outcome(), Outcome::Blocked(vec![Ask { to: To::Person, text: "which store?".into() }]));
        let unowed = Exit::parse(Kind::Member, &format!("{base}- not held: b — waits\n\nowed: nothing\n")).unwrap();
        assert_eq!(unowed.outcome(), Outcome::Unowed);
    }
}
