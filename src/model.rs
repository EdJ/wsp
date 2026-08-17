//! The durable entities. Everything here round-trips through Markdown +
//! frontmatter, so field names are the on-disk contract.

use crate::fm::{self, Doc, Val};
use crate::util;

pub const SCHEMA: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Inbox,
    Todo,
    Doing,
    Blocked,
    Review,
    Done,
}

impl Status {
    pub fn parse(s: &str) -> Option<Status> {
        match s.trim().to_ascii_lowercase().as_str() {
            "inbox" => Some(Status::Inbox),
            "todo" | "open" => Some(Status::Todo),
            "doing" | "wip" | "started" => Some(Status::Doing),
            "blocked" => Some(Status::Blocked),
            "review" => Some(Status::Review),
            "done" | "closed" => Some(Status::Done),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Inbox => "inbox",
            Status::Todo => "todo",
            Status::Doing => "doing",
            Status::Blocked => "blocked",
            Status::Review => "review",
            Status::Done => "done",
        }
    }
    pub fn is_open(&self) -> bool {
        !matches!(self, Status::Done)
    }
    /// Sort weight for listings: attention first.
    pub fn rank(&self) -> u8 {
        match self {
            Status::Blocked => 0,
            Status::Review => 1,
            Status::Doing => 2,
            Status::Todo => 3,
            Status::Inbox => 4,
            Status::Done => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    High,
    Normal,
    Low,
}

impl Priority {
    pub fn parse(s: &str) -> Option<Priority> {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" | "h" => Some(Priority::High),
            "normal" | "n" | "" => Some(Priority::Normal),
            "low" | "l" => Some(Priority::Low),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Priority::High => "high",
            Priority::Normal => "normal",
            Priority::Low => "low",
        }
    }
    pub fn rank(&self) -> u8 {
        match self {
            Priority::High => 0,
            Priority::Normal => 1,
            Priority::Low => 2,
        }
    }

    /// The one column a list gives this, wherever the list is drawn.
    ///
    /// Here rather than at each of the four places that draw it, because the
    /// two surfaces had already drifted once: `ls` marked `low` and `brief`
    /// did not, so the same task read as ordinary in one and deferred in the
    /// other. Colour is each surface's own — `Paint` in a terminal, `Style` in
    /// the panel — but the glyph is one fact.
    ///
    /// `normal` is a space, not nothing: the mark is a column that lines up
    /// down a list, and a column that closes up when there is nothing in it
    /// moves every title on the rows around it.
    pub const fn mark(&self) -> &'static str {
        match self {
            Priority::High => "!",
            Priority::Normal => " ",
            Priority::Low => "↓",
        }
    }

    /// The order one key steps through, for the panel.
    ///
    /// Three values and one key, so the order is the whole design: `high`
    /// comes off `normal` because raising something is what a person reaching
    /// for that key nearly always means, and `low` comes next so the rarer
    /// deliberate demotion needs a second press rather than a second key.
    /// `normal` last makes the cycle its own undo — hold the key and you come
    /// back to where you started, which is what saves a blind cycle from being
    /// a trap.
    pub fn cycled(&self) -> Priority {
        match self {
            Priority::Normal => Priority::High,
            Priority::High => Priority::Low,
            Priority::Low => Priority::Normal,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub tags: Vec<String>,
    pub roots: Vec<String>,
    pub status: String,
    pub brief: String,
    /// The prefix this project's task ids carry, empty when it is just the
    /// slug. See [`Project::code`] for why it is stored empty rather than
    /// filled in.
    pub code_raw: String,
    /// Highest number this project has handed out — a hint, not the truth.
    ///
    /// [`crate::store::Store::alloc_task_id`] reads it to skip the directory
    /// scan and then settles the race with `O_EXCL` regardless, so a `seq` that
    /// is stale, missing, or wrong costs a scan and never a duplicate id. That
    /// is the whole reason it can live here, in a file two machines might both
    /// be writing, rather than under a lock.
    pub seq: usize,
    pub body: String,
}

impl Project {
    pub fn new(id: &str) -> Project {
        Project {
            id: id.to_string(),
            name: id.to_string(),
            status: "active".into(),
            ..Default::default()
        }
    }

    /// The prefix task ids in this project take: the code if one is set, and
    /// the slug otherwise.
    ///
    /// Defaulting here rather than at write time keeps "no code set" and "code
    /// set to the slug" the same state on disk. They behave identically, and a
    /// project that stored its slug in `code:` would silently stop tracking a
    /// slug that later changed — which is a thing that cannot happen today but
    /// is exactly what t-260817-025 exists to make possible.
    pub fn code(&self) -> &str {
        if self.code_raw.is_empty() {
            &self.id
        } else {
            &self.code_raw
        }
    }

    pub fn from_doc(doc: &Doc, fallback_id: &str) -> Project {
        let id = doc.opt("id").unwrap_or_else(|| fallback_id.to_string());
        Project {
            name: doc.opt("name").unwrap_or_else(|| id.clone()),
            id,
            parent: doc.opt("parent"),
            tags: doc.list("tags"),
            roots: doc.list("roots"),
            status: doc.opt("status").unwrap_or_else(|| "active".into()),
            brief: doc.str("brief"),
            code_raw: doc.opt("code").unwrap_or_default(),
            seq: doc.opt("seq").and_then(|s| s.parse().ok()).unwrap_or(0),
            body: doc.body.clone(),
        }
    }

    pub fn to_doc(&self) -> Doc {
        let mut d = Doc::default();
        d.set_str("id", &self.id);
        d.set_str("name", &self.name);
        d.set_str("parent", self.parent.as_deref().unwrap_or(""));
        d.set_list("tags", &self.tags);
        d.set_list("roots", &self.roots);
        d.set_str("status", &self.status);
        d.set_str("brief", &self.brief);
        d.set_str("code", &self.code_raw);
        d.set_str("seq", &self.seq.to_string());
        d.set_str("schema", SCHEMA);
        d.body = self.body.clone();
        d
    }

    pub fn section(&self, name: &str) -> Option<String> {
        section_of(&self.body, name)
    }

    pub fn render(&self) -> String {
        fm::emit(&self.to_doc())
    }
}

/// Split a body into `(heading, text)`, with `""` for anything before the
/// first heading.
fn split_sections(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut name = String::new();
    let mut buf = String::new();
    for line in body.lines() {
        match line.strip_prefix("## ") {
            Some(h) => {
                if !name.is_empty() || !buf.trim().is_empty() {
                    out.push((name, buf.trim_end().to_string()));
                }
                name = h.trim().to_string();
                buf = String::new();
            }
            None => {
                buf.push_str(line);
                buf.push('\n');
            }
        }
    }
    if !name.is_empty() || !buf.trim().is_empty() {
        out.push((name, buf.trim_end().to_string()));
    }
    out
}

pub fn section_of(body: &str, name: &str) -> Option<String> {
    split_sections(body)
        .into_iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, t)| t)
        .filter(|t| !t.trim().is_empty())
}

/// Whether a body carries a `## <name>` heading at all, empty or not.
///
/// [`section_of`] cannot answer this — it reports an empty section as absent,
/// which is right for reading and wrong for writing an edit back. A heading the
/// buffer still shows with nothing under it means *clear this*; a heading the
/// buffer never carried means *this was not on screen, leave it alone*. Reading
/// both as "absent" is how a combined-buffer save wipes the section it was not
/// editing.
pub fn has_section(body: &str, name: &str) -> bool {
    split_sections(body).iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// Replace a section, or add it if absent. Sections are rewritten in the
/// canonical order and anything the schema does not know about is kept — the
/// body is the user's, and losing a heading nobody anticipated would be a
/// worse failure than any it prevents.
pub fn set_section_in(body: &mut String, name: &str, text: &str) {
    let mut secs = split_sections(body);
    let text = demote_headings(text.trim_end());
    match secs.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        Some(slot) => slot.1 = text,
        None if !text.trim().is_empty() => secs.push((name.to_string(), text)),
        None => {}
    }
    secs.retain(|(n, t)| !(n.is_empty() && t.trim().is_empty()));

    // `Log` last, whatever else the body is carrying. Everything that appends
    // to it writes to the end of the body, so a heading sorted after it stops
    // being a heading in a file and starts being a hole: three log lines on
    // t-260816-012 went under `## Not doing`, where nothing would ever read
    // them, and nothing failed while it happened. A heading the schema does not
    // know about is still kept — losing prose nobody anticipated would be a
    // worse failure — but it is kept *before* the log.
    let rank = |n: &str| -> usize {
        if n.is_empty() {
            return 0;
        }
        if n.eq_ignore_ascii_case("Log") {
            return SECTIONS.len() + 1;
        }
        SECTIONS
            .iter()
            .position(|s| s.eq_ignore_ascii_case(n))
            .map(|i| i + 1)
            .unwrap_or(SECTIONS.len())
    };
    secs.sort_by_key(|(n, _)| rank(n));

    let mut out = String::new();
    for (n, t) in secs {
        if t.trim().is_empty() && !n.is_empty() {
            continue;
        }
        if !n.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("## {n}\n"));
        }
        out.push_str(t.trim_end());
        out.push('\n');
    }
    *body = out;
}

/// The `## ` headings a chunk of markdown carries, in the order it carries
/// them.
pub fn headings(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("## "))
        .map(|h| h.trim().to_string())
        .collect()
}

/// The headings in a body that the schema does not know about.
///
/// What [`fold_stray_sections`] repairs, and what a writer is told it sent.
pub fn stray_sections(body: &str) -> Vec<String> {
    headings(body)
        .into_iter()
        .filter(|n| !SECTIONS.iter().any(|s| s.eq_ignore_ascii_case(n)))
        .collect()
}

/// Demote `## ` headings inside a section's own text to `###`.
///
/// The payload handed to [`set_section_in`] is the *inside* of one section, so
/// a `## ` line in it is not a heading in that section — it is a sibling
/// section, and that is how it comes back out of [`split_sections`] next time
/// the file is read. A long brief written into `--overview` therefore came
/// apart into six top-level sections, and because `split_sections` rightly
/// keeps every heading it does not recognise, rewriting the same brief added a
/// second copy of all of them rather than replacing them.
///
/// Demoting rather than refusing, because `## ` is simply what you type when
/// you are writing a section of prose, and the person writing it is thinking
/// about the content and not about this parser. Knowing about the defect was
/// not enough to avoid it: the agent that filed it hit it again the same day.
/// The prose and its structure both survive; only the level changes.
///
/// Fenced code is not special-cased, deliberately. `split_sections` does not
/// look at fences either, so demoting exactly what it would read as a heading
/// is what keeps a body round-tripping — a rule that disagreed with the parser
/// would leave the damage it is here to prevent.
pub fn demote_headings(text: &str) -> String {
    if !text.lines().any(|l| l.starts_with("## ")) {
        return text.to_string();
    }
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with("## ") {
            out.push('#');
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Fold headings the schema does not know about back into the prose they were
/// written as part of, and put `Log` back last. `None` when there is nothing
/// to repair.
///
/// For bodies written before [`set_section_in`] demoted them. There is no other
/// way back out: no command deletes a section it does not know about, so a task
/// damaged this way could only be repaired through `--raw` and an editor.
///
/// Which section a stray belonged to is not recoverable from the file — the
/// canonical sort has already moved it — so it is read off document order, and
/// only from the two sections a payload is ever written into. A stray sitting
/// after `Decisions` or `Log` is there because the sort put it there, not
/// because anyone wrote it there, and the brief it came from was the last
/// `Overview` or `Details` above it.
pub fn fold_stray_sections(body: &str) -> Option<String> {
    if stray_sections(body).is_empty() {
        return None;
    }
    let secs = split_sections(body);
    let known = |n: &str| SECTIONS.iter().any(|s| s.eq_ignore_ascii_case(n));
    let last = secs.len() - 1;
    let logged_before_end = secs
        .iter()
        .position(|(n, _)| n.eq_ignore_ascii_case("Log"))
        .is_some_and(|i| i < last);

    let mut kept: Vec<(String, String)> = Vec::new();
    let mut host: Option<usize> = None;
    for (i, (name, text)) in secs.into_iter().enumerate() {
        if name.is_empty() || known(&name) {
            kept.push((name.clone(), text));
            if ["Overview", "Details"].iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                host = Some(kept.len() - 1);
            }
            continue;
        }
        // Only the last section can have swallowed log lines: `Task::log`
        // appends to the end of the body, and only ever the end.
        let (text, dated) = match i == last && logged_before_end {
            true => peel_trailing_dated(&text),
            false => (text, Vec::new()),
        };
        if !dated.is_empty() {
            if let Some(log) = kept.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case("Log")) {
                log.1 = format!("{}\n{}", log.1.trim_end(), dated.join("\n"));
            }
        }
        // Blank lines off the front, and not a character more: the first line
        // of a section is as often an indented block as it is a sentence, and
        // `trim` would quietly unindent it.
        let folded = format!("### {name}\n\n{}", text.trim_start_matches('\n').trim_end());
        match host {
            Some(h) => {
                let into = &mut kept[h].1;
                *into = format!("{}\n\n{folded}", into.trim_end());
            }
            None => {
                kept.insert(0, ("Overview".to_string(), folded));
                host = Some(0);
            }
        }
    }

    let mut out = String::new();
    for (name, text) in kept {
        set_section_in(&mut out, &name, &text);
    }
    Some(out)
}

/// Split off the run of dated entries at the end of a section's text.
///
/// Everything that appends to the log writes `- <date> …` at the end of the
/// body, so when a stray heading was sorted after `## Log` the entries written
/// since are exactly that: dated lines, at the end, under prose that is not
/// dated. That is enough to give them back.
fn peel_trailing_dated(text: &str) -> (String, Vec<String>) {
    let dated = |l: &str| match l.trim().strip_prefix("- ").and_then(|r| r.split_once(' ')) {
        Some((d, _)) => d.len() == 10 && d.chars().all(|c| c.is_ascii_digit() || c == '-'),
        None => false,
    };
    let mut lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    while let Some(l) = lines.last() {
        if l.trim().is_empty() {
            lines.pop();
        } else if dated(l) {
            out.push(l.trim().to_string());
            lines.pop();
        } else {
            break;
        }
    }
    out.reverse();
    (lines.join("\n").trim_end().to_string(), out)
}

/// True when any written section carries anything — what the tree uses to mark
/// a row as having something worth reading on it.
///
/// `Decisions` counts. A row whose only prose is a decision is precisely the
/// row you want the mark on: somebody settled something here, and the next
/// person to pick it up needs to know before they start rather than after.
/// `Log` does not — every task acquires one by being worked, so it would mark
/// everything and therefore nothing.
pub fn has_prose(body: &str) -> bool {
    PROSE.iter().any(|s| section_of(body, s).is_some())
}

/// The sections a person writes on a **task**.
///
/// The list exists so that "which sections can be edited" is answered in one
/// place. It used to be answered in three, by three literals, and the third —
/// `edit_prose`'s — was still naming two of them after `Decisions` shipped.
/// `wsp edit --decisions` therefore fell through to the combined-buffer path
/// and overwrote `Overview` while reporting success. A vocabulary that lives in
/// one place cannot go stale in two others.
///
/// `Log` is not here. It is append-only, `wsp note` is how you add to it, and
/// editing history in place is how history stops being evidence.
///
/// `Handbook` is not here either, and that is the one place the two vocabularies
/// part: a handbook is what a project tells everybody who arrives, and a task
/// has no such thing. Offered on a task it would put an empty heading in the
/// combined edit buffer of every task in the store, for a section nobody could
/// sensibly fill.
pub const PROSE: [&str; 3] = ["Overview", "Details", "Decisions"];

/// The sections a person writes on a **project**: [`PROSE`] plus `Handbook`.
///
/// Two lists rather than one because the kinds genuinely differ, and one list
/// with a "not for tasks" rule beside it would be the same drift by another
/// name. Everything that renders or edits a project's prose reads this — the
/// detail pane included, which was carrying a fourth literal copy of the task
/// list and would otherwise have gone stale the moment this landed.
pub const PROJECT_PROSE: [&str; 4] = ["Overview", "Details", "Handbook", "Decisions"];

/// The headings a task or project body carries, in the order they are written.
///
/// `Overview` is what the thing is — written once, read to re-enter it later.
/// `Details` is working material: criteria, links, whatever the work needs.
/// `Handbook` is a project's standing answer to "I have just arrived here":
/// what the work is for, how it is done, and — the part that keeps it short —
/// *where the code's own documentation lives and what is in it*. A technical
/// map of a tree does not belong here. It belongs in the tree, versioned with
/// the code it describes, reviewed in the same diff, and put in front of
/// whoever changes that code; the same content held here drifts the moment
/// somebody refactors and nothing makes them look at it. So the handbook names
/// that file and says what it holds, and an agent that needs the map goes and
/// reads it. Projects only — see [`PROJECT_PROSE`].
/// `Decisions` is what was settled and now binds: dated, append-only, and not
/// to be edited, because a decision that can be quietly rewritten is not a
/// record of anything.
/// `Log` is the dated, append-only trail and is never edited by hand.
///
/// `Decisions` sits before `Log` deliberately. The log is where a thing has
/// been; a decision is a constraint on where it can go, which is worth reading
/// before the history rather than after it — and it keeps `Log` last, which is
/// what lets [`Task::log`] go on appending to the end of the body.
pub const SECTIONS: [&str; 5] = ["Overview", "Details", "Handbook", "Decisions", "Log"];

/// Append a dated line under `## <section>`, adding the section if it is not
/// there. Written through [`set_section_in`], so the body comes back in
/// canonical order however out of order it was.
pub fn append_dated(body: &mut String, section: &str, line: &str) {
    let mut text = section_of(body, section).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("- {} {}\n", util::today_ymd(), line));
    set_section_in(body, section, &text);
}

/// The `## Decisions` entries, oldest first, as `(date, text)`.
///
/// Split here rather than in the renderers: every one of them wants to dim the
/// date, or truncate the text to a width, or both, and a caller handed a whole
/// line would have to take it apart again to do either. Same bargain as
/// `worked_line` — the record goes in, the sentence comes out where it is
/// shown.
///
/// A line with no leading date is not dropped. It comes back with an empty
/// date, because a decision somebody wrote by hand is still a decision and
/// losing it to a formatting rule would be the worst thing this could do.
pub fn decisions(body: &str) -> Vec<(String, String)> {
    let ymd = |s: &str| {
        s.len() == 10 && s.chars().all(|c| c.is_ascii_digit() || c == '-')
    };
    section_of(body, "Decisions")
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().trim_start_matches("- ").trim().to_string())
        .filter(|l| !l.is_empty())
        .map(|l| match l.split_once(' ') {
            Some((d, rest)) if ymd(d) => (d.to_string(), rest.trim().to_string()),
            _ => (String::new(), l),
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub status_raw: String,
    pub priority_raw: String,
    pub parent: Option<String>,
    pub created: String,
    pub updated: String,
    pub refs: Vec<String>,
    pub body: String,
}

impl Task {
    pub fn new(title: &str, id: &str) -> Task {
        let now = util::now_iso();
        Task {
            id: id.to_string(),
            title: title.to_string(),
            status_raw: "todo".into(),
            priority_raw: "normal".into(),
            created: now.clone(),
            updated: now,
            ..Default::default()
        }
    }

    pub fn status(&self) -> Status {
        Status::parse(&self.status_raw).unwrap_or(Status::Todo)
    }

    pub fn priority(&self) -> Priority {
        Priority::parse(&self.priority_raw).unwrap_or(Priority::Normal)
    }

    pub fn set_status(&mut self, s: Status) {
        self.status_raw = s.as_str().to_string();
        self.touch();
    }

    pub fn touch(&mut self) {
        self.updated = util::now_iso();
    }

    /// Append a timestamped line under a `## Log` heading.
    ///
    /// Through [`append_dated`] rather than by writing to the end of the body,
    /// which is the same thing only while `Log` is last. In a body carrying a
    /// heading the schema does not know about it was not, and every entry
    /// written went under that heading instead — a log that had stopped being
    /// the log without anything failing.
    pub fn log(&mut self, line: &str) {
        append_dated(&mut self.body, "Log", line);
    }

    /// The text under `## <name>`, heading excluded.
    pub fn section(&self, name: &str) -> Option<String> {
        section_of(&self.body, name)
    }

    /// Whether a half-remembered phrase is anywhere in this task: its title,
    /// its id, or its prose.
    ///
    /// The prose is not an extra — it is where most of what distinguishes one
    /// task from another lives. Titles here run to a clause; the paragraph
    /// under `## Overview` is what says which reverb, which pane, which of the
    /// four things called "sync". A search over titles alone would answer
    /// confidently and wrongly, which is worse than not searching.
    ///
    /// One phrase, matched whole, rather than words matched separately.
    /// `wsp find tuning table` means the two words in that order, which is how
    /// a phrase you half-remember comes back to you. Splitting them would make
    /// this a query language with an implied `AND` nobody typed, and the word
    /// Ed used was "basic".
    ///
    /// Case-folded ASCII-only, like the tag picker: the store's vocabulary is
    /// English and a `to_lowercase` per task per keystroke is the panel's live
    /// filter paying for Turkish dotless i.
    pub fn matches(&self, needle: &str) -> bool {
        let n = needle.trim().to_ascii_lowercase();
        if n.is_empty() {
            return true;
        }
        self.title.to_ascii_lowercase().contains(&n)
            || self.id.to_ascii_lowercase().contains(&n)
            || self.body.to_ascii_lowercase().contains(&n)
    }

    /// The prose that put this row in the list, for a hit whose title does not
    /// say why it is one.
    ///
    /// A list of titles that match nothing you typed is a list you have to open
    /// one by one. This is the sentence the search actually landed in, which is
    /// usually the whole of the answer to "which of these is the one".
    ///
    /// A window around the match rather than the head of the line, and that is
    /// the whole of what is fiddly here. Prose in this store is hard-wrapped at
    /// about eighty columns, so the phrase is as likely to be at the end of the
    /// line as the start — printing the first seventy characters cut off the
    /// word the reader typed on four of the first five hits it was tried on.
    /// The window slides just far enough, and only to a space, so it opens on a
    /// word rather than mid-syllable.
    pub fn prose_line(&self, needle: &str, width: usize) -> Option<String> {
        let n = needle.trim().to_ascii_lowercase();
        if n.is_empty() {
            return None;
        }
        let line = self
            .body
            .lines()
            .map(|l| l.trim_start_matches(['#', '-', '*', ' ']).trim_end())
            .find(|l| l.to_ascii_lowercase().contains(&n))?;

        let chars: Vec<char> = line.chars().collect();
        if chars.len() <= width {
            return Some(line.to_string());
        }
        // Where the match sits, in characters rather than bytes: the window is
        // measured in what a terminal draws.
        let at = line.to_ascii_lowercase().find(&n).unwrap_or(0);
        let at = line[..at].chars().count();
        // Room for the match and a little of what leads into it. `end` first,
        // so a match near the end of the line pulls the window rather than
        // being clipped by it.
        let end = (at + n.chars().count() + width / 3).min(chars.len());
        let mut start = end.saturating_sub(width);
        if start > 0 {
            // To the next space, so the snippet opens on a whole word.
            start += chars[start..at.max(start)].iter().position(|c| *c == ' ').map_or(0, |i| i + 1);
        }
        let mut out = String::new();
        if start > 0 {
            out.push('…');
        }
        out.extend(chars[start..end].iter());
        if end < chars.len() {
            out.push('…');
        }
        Some(out)
    }

    pub fn from_doc(doc: &Doc, fallback_id: &str) -> Task {
        Task {
            id: doc.opt("id").unwrap_or_else(|| fallback_id.to_string()),
            title: doc.str("title"),
            project: doc.opt("project"),
            tags: doc.list("tags"),
            status_raw: doc.opt("status").unwrap_or_else(|| "todo".into()),
            priority_raw: doc.opt("priority").unwrap_or_else(|| "normal".into()),
            parent: doc.opt("parent"),
            created: doc.str("created"),
            updated: doc.str("updated"),
            refs: doc.list("refs"),
            body: doc.body.clone(),
        }
    }

    pub fn to_doc(&self) -> Doc {
        let mut d = Doc::default();
        d.set_str("id", &self.id);
        d.set_str("title", &self.title);
        d.set_str("project", self.project.as_deref().unwrap_or(""));
        d.set_list("tags", &self.tags);
        d.set_str("status", &self.status_raw);
        d.set_str("priority", &self.priority_raw);
        if let Some(p) = &self.parent {
            d.set_str("parent", p);
        }
        d.set_str("created", &self.created);
        d.set_str("updated", &self.updated);
        d.set(fm_key_refs(), Val::L(self.refs.clone()));
        d.set_str("schema", SCHEMA);
        d.body = self.body.clone();
        d
    }

    pub fn render(&self) -> String {
        fm::emit(&self.to_doc())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "title": self.title,
            "project": self.project,
            "tags": self.tags,
            "status": self.status().as_str(),
            "priority": self.priority().as_str(),
            "parent": self.parent,
            "created": self.created,
            "updated": self.updated,
            "refs": self.refs,
        })
    }
}

fn fm_key_refs() -> &'static str {
    "refs"
}

/// A machine wsp can run agents on — an executor, reached over SSH from the
/// seat you are sitting at.
///
/// Durable and committed, one file per machine exactly as a project is,
/// because which machines exist is a fact about the setup rather than about
/// this afternoon. What is *live* about a machine — reachable, last seen,
/// which socket the daemon forwarded it to — is state and lives in
/// `machines.json`, so a machine that has gone away is still a row with a
/// last-seen on it rather than a hole in the list.
///
/// [`Machine::ssh`] is a `Host` alias out of `~/.ssh/config` and never an
/// address. wsp does not parse ssh config, does not learn a hostname and has
/// no idea a tailnet exists; that ignorance is the seam Tailscale plugs into,
/// and keeping it is why "make a machine reachable" is somebody else's problem
/// and not a wsp feature.
#[derive(Debug, Clone, Default)]
pub struct Machine {
    /// The file stem, and the suffix on a host-qualified id: `w0:p3@mb2`.
    pub name: String,
    /// A `Host` alias, not a hostname. See the struct docs.
    pub ssh: String,
    /// Where this machine's backend listens, in whatever words that backend
    /// uses. [`crate::place`]'s term, and opaque here for the same reason a
    /// [`crate::place::Seat`] is: nothing in the model reads it, and the
    /// adapter is the only thing entitled to know what it means. For herdr it
    /// is an absolute path *on that machine* — what `ssh -L <here>:<there>`
    /// forwards — and for a backend reached over TCP it would be a host and a
    /// port without anything here changing.
    ///
    /// Written down rather than discovered, because `ssh -L` does not expand
    /// `~` on the remote side and asking the far machine for its `$HOME` would
    /// put a blocking round trip — to a machine that may be down — inside the
    /// daemon's tick.
    ///
    /// Empty means *the backend's own default*, and the backend supplies it:
    /// `place_herdr::mirrored_socket` is herdr's, the mirrored-path assumption
    /// that is right while paths mirror and is exactly what a Linux executor
    /// breaks. It used to be defaulted here, which put a `crate::herdr` call in
    /// the file that is meant to be nothing but durable entities (t-260816-064).
    pub backend_at: String,
    /// `darwin` | `linux`, and whatever comes next. Free text: nothing branches
    /// on it yet, and a machine you cannot describe is still a machine.
    pub os: String,
    pub arch: String,
    /// `active` | `retired`. A retired machine is not deleted, because the
    /// agents that ran on it are in the log and a row with a last-seen reads
    /// better than a missing one.
    pub status: String,
    pub added: String,
    pub body: String,
}

/// The characters a machine name may use, and why it is not just a filename.
///
/// The name is the `@mb2` suffix on a host-qualified id, so anything that
/// could be mistaken for part of an id makes routing ambiguous: `@` would
/// nest, `:` is already *inside* a pane id — `w0:p3` is one id, not two — and
/// whitespace would split an argument on its way to `ssh`. Lowercase
/// alphanumerics, `-` and `_`, and nothing else.
///
/// Rejected rather than slugified, unlike a project slug. A project slug is
/// typed once and read by people; a machine name is typed into `--on` and
/// pasted out of an id, and silently answering to a different name than the
/// one you gave is how you end up with two machines you believe are one.
pub fn valid_machine_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a machine needs a name".into());
    }
    if let Some(bad) = name.chars().find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_')) {
        return Err(match bad {
            '@' => "`@` separates a machine from an id (w0:p3@mb2) and cannot be in the name".into(),
            ':' => "`:` is already inside a pane id (w0:p3 is one id) and cannot be in the name".into(),
            c if c.is_ascii_uppercase() => format!("`{c}`: machine names are lowercase"),
            c => format!("`{c}`: machine names are lowercase letters, digits, `-` and `_`"),
        });
    }
    Ok(())
}

impl Machine {
    pub fn new(name: &str, ssh: &str) -> Machine {
        Machine {
            name: name.to_string(),
            ssh: ssh.to_string(),
            status: "active".into(),
            added: util::now_iso(),
            ..Default::default()
        }
    }

    pub fn from_doc(doc: &Doc, fallback_name: &str) -> Machine {
        let name = doc.opt("name").unwrap_or_else(|| fallback_name.to_string());
        Machine {
            // An `ssh` that was never written defaults to the name, which is
            // the case where the `Host` alias and the machine are called the
            // same thing — the common one, and the one worth not having to say
            // twice.
            ssh: doc.opt("ssh").unwrap_or_else(|| name.clone()),
            name,
            // No default, and no reading of the old `herdr_sock` key either: a
            // fallback here would be this file naming a backend again, which is
            // the whole of what t-260816-064 was about. Nothing on disk carries
            // the old key — the field was a day old and no store had a machines
            // directory in it — so the migration is a rename and not a read.
            backend_at: doc.str("backend_at"),
            os: doc.str("os"),
            arch: doc.str("arch"),
            status: doc.opt("status").unwrap_or_else(|| "active".into()),
            added: doc.str("added"),
            body: doc.body.clone(),
        }
    }

    pub fn to_doc(&self) -> Doc {
        let mut d = Doc::default();
        d.set_str("name", &self.name);
        d.set_str("ssh", &self.ssh);
        d.set_str("backend_at", &self.backend_at);
        d.set_str("os", &self.os);
        d.set_str("arch", &self.arch);
        d.set_str("status", &self.status);
        d.set_str("added", &self.added);
        d.set_str("schema", SCHEMA);
        d.body = self.body.clone();
        d
    }

    pub fn is_active(&self) -> bool {
        self.status != "retired"
    }

    pub fn render(&self) -> String {
        fm::emit(&self.to_doc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason a search over titles alone would have been a waste of the
    /// afternoon: the title says "basic search" and everything that tells you
    /// which search, and why, is in the paragraph under it.
    #[test]
    fn a_word_only_the_prose_holds_still_finds_the_task() {
        let mut t = Task::new("basic search", "wsp-032");
        t.body = "## Overview\nEd is struggling to find issues in a list of\n\
literally hundreds.\n"
            .into();

        assert!(t.matches("hundreds"), "the prose is searched, not only the title");
        assert!(t.matches("BASIC"), "case is not something anyone remembers");
        assert!(t.matches("wsp-032"), "the id is searchable — it is on the row you read");
        assert!(!t.matches("kalimba"));
        // Two words are one phrase in that order, not two searches joined by an
        // `AND` nobody typed.
        assert!(t.matches("find issues"));
        assert!(!t.matches("issues find"));
        // An empty needle is not a filter. The panel holds one while `/` is
        // open and nothing has been typed yet, and the tree must be whole.
        assert!(t.matches("  "));
    }

    /// Prose here is hard-wrapped at about eighty columns, so the word you
    /// typed is as likely to be at the end of its line as the start. A snippet
    /// taken from the head of the line cut the match off four hits out of five
    /// — a second line under the row that does not contain the search term
    /// reads as the wrong task, which is exactly the doubt this is meant to
    /// settle.
    #[test]
    fn the_snippet_shows_the_word_you_typed() {
        let mut t = Task::new("One working tree per agent", "robustness-010");
        t.body = "## Overview\nThe tree is reset to HEAD and patched with that agent's own diff \
before each build, with a persistent CARGO_TARGET_DIR beside it.\n"
            .into();

        let snip = t.prose_line("persistent", 40).expect("a line to show");
        assert!(snip.contains("persistent"), "the snippet lost the match: {snip:?}");
        assert!(snip.chars().count() <= 42, "one line, not the paragraph: {snip:?}");
        assert!(snip.starts_with('…'), "a window that has slid says so: {snip:?}");
        assert!(!snip.contains("  "), "it opens on a word: {snip:?}");

        // A line that fits is left alone — no ellipsis on either end, and no
        // window arithmetic to get wrong.
        let mut short = Task::new("t", "x-001");
        short.body = "## Overview\nthe tail is right\n".into();
        assert_eq!(short.prose_line("tail", 40).as_deref(), Some("the tail is right"));
        assert_eq!(short.prose_line("nothing here", 40), None);
    }

    #[test]
    fn a_decision_lands_in_its_own_section_before_the_log() {
        let mut b = String::from("## Overview\nwhat this is\n\n## Log\n- 2026-08-14 claimed\n");
        append_dated(&mut b, "Decisions", "we are not doing worktrees yet");
        let heads: Vec<&str> = b.lines().filter(|l| l.starts_with("## ")).collect();
        assert_eq!(heads, ["## Overview", "## Decisions", "## Log"], "canonical order");
        // Log stays last, which is what lets Task::log go on appending to the end.
        assert!(b.trim_end().ends_with("- 2026-08-14 claimed"));
    }

    #[test]
    fn decisions_come_back_as_date_and_text() {
        let mut b = String::new();
        append_dated(&mut b, "Decisions", "render and data are separate sub-projects");
        let got = decisions(&b);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0.len(), 10, "a yyyy-mm-dd date");
        assert_eq!(got[0].1, "render and data are separate sub-projects");
    }

    #[test]
    fn they_accumulate_oldest_first() {
        let mut b = String::new();
        append_dated(&mut b, "Decisions", "first");
        append_dated(&mut b, "Decisions", "second");
        let texts: Vec<String> = decisions(&b).into_iter().map(|(_, t)| t).collect();
        assert_eq!(texts, ["first", "second"]);
    }

    /// Somebody will write one by hand. Losing it to a formatting rule would be
    /// the worst thing this code could do.
    #[test]
    fn an_undated_line_is_kept_rather_than_dropped() {
        let b = String::from("## Decisions\n- no date on this one\nnor a bullet here\n");
        let got = decisions(&b);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (String::new(), "no date on this one".to_string()));
        assert_eq!(got[1], (String::new(), "nor a bullet here".to_string()));
    }

    #[test]
    fn no_decisions_is_an_empty_list_not_an_empty_entry() {
        assert!(decisions("## Overview\nsomething\n").is_empty());
        assert!(decisions("").is_empty());
    }

    /// The mark in the tree means "something is written here". A decision is
    /// exactly that; a log is not, since every task grows one by being worked.
    #[test]
    fn a_decision_alone_marks_a_row_as_written_on() {
        assert!(has_prose("## Decisions\n- 2026-08-15 settled\n"));
        assert!(!has_prose("## Log\n- 2026-08-15 claimed\n"));
    }

    /// The distinction a combined-buffer save turns on. `section_of` reports an
    /// emptied section as absent, which is right for reading and would, on the
    /// way back, delete a section that was never on screen.
    #[test]
    fn an_emptied_section_is_present_even_though_it_reads_as_absent() {
        let b = "## Overview\nkept\n\n## Details\n\n";
        assert_eq!(section_of(b, "Details"), None, "empty reads as absent");
        assert!(has_section(b, "Details"), "…but the heading is still there");
        assert!(!has_section(b, "Decisions"), "and one never written is not");
    }

    /// A machine name is not a filename. It is the `@mb2` on the end of a pane
    /// id, so the two characters that already mean something inside an id have
    /// to be refused rather than tidied away — and refused with a message that
    /// says which one, because "invalid name" tells you nothing you can act on.
    #[test]
    fn a_machine_name_may_not_contain_what_an_id_already_uses() {
        assert!(valid_machine_name("mb2").is_ok());
        assert!(valid_machine_name("linux-box_2").is_ok());

        assert!(valid_machine_name("w0:p3@mb2").unwrap_err().contains(':'), "the colon is caught first");
        assert!(valid_machine_name("mb2@home").unwrap_err().contains('@'));
        assert!(valid_machine_name("MB2").unwrap_err().contains("lowercase"));
        assert!(valid_machine_name("mb 2").is_err(), "whitespace would split an ssh argument");
        assert!(valid_machine_name("").is_err());
    }

    /// The alias and the machine are usually called the same thing, and a
    /// record written before `ssh` existed as a field must not come back with
    /// nothing to dial.
    #[test]
    fn a_machine_with_no_ssh_alias_answers_to_its_own_name() {
        let m = Machine::from_doc(&fm::parse("---\nname: mb2\n---\n"), "mb2");
        assert_eq!(m.ssh, "mb2");
        assert_eq!(m.status, "active", "and is active until something says otherwise");

        let round = Machine::from_doc(&fm::parse(&Machine::new("mb2", "mac-mini").render()), "ignored");
        assert_eq!(round.ssh, "mac-mini", "an alias that was given is kept");
        assert_eq!(round.name, "mb2");
    }

    /// The defect this file was fixed for. A brief with its own `## ` headings
    /// is one section's text, and storing it verbatim makes it six.
    #[test]
    fn a_headings_payload_stays_one_section_however_often_it_is_written() {
        let brief = "what this is\n\n## The shape\n\nlike so\n\n## Open\n\nnot yet";
        let mut b = String::new();
        set_section_in(&mut b, "Overview", brief);
        assert_eq!(headings(&b), ["Overview"], "one section, not three");
        assert!(b.contains("### The shape"), "the structure is kept, one level down");

        // Rewriting the same brief replaces it. Before, the second write left
        // the sections of the first behind and added its own on the end.
        let once = b.clone();
        set_section_in(&mut b, "Overview", brief);
        assert_eq!(b, once, "no second copy");
    }

    /// Everything that appends to the log writes to the end of the body, so a
    /// heading sorted after it silently takes the entries.
    #[test]
    fn the_log_stays_last_whatever_else_the_body_carries() {
        let mut b = String::from("## Overview\nwhat this is\n\n## Not doing\nthe other thing\n");
        append_dated(&mut b, "Log", "claimed");
        assert_eq!(headings(&b).last().unwrap(), "Log");

        // And an entry lands in it even when it arrives out of order.
        let mut t = Task::new("t", "t-1");
        t.body = String::from("## Overview\nwhat this is\n\n## Log\n- 2026-08-15 claimed\n\n## Not doing\nthe other thing\n");
        t.log("blocked");
        assert!(section_of(&t.body, "Log").unwrap().contains("blocked"));
        assert!(!section_of(&t.body, "Not doing").unwrap().contains("blocked"));
    }

    /// The repair. Which section a stray belonged to is read off document
    /// order, from the sections a payload is written into — `Details` here.
    #[test]
    fn a_stray_section_folds_back_into_the_prose_above_it() {
        let b = "## Overview\nshort\n\n## Details\nthe question\n\n## Answer\nno\n\n## Log\n- 2026-08-15 claimed\n";
        let got = fold_stray_sections(b).expect("something to fold");
        assert_eq!(headings(&got), ["Overview", "Details", "Log"]);
        assert!(section_of(&got, "Details").unwrap().contains("### Answer"));
        assert!(section_of(&got, "Overview").unwrap().trim() == "short", "not the nearest, the right one");
        assert!(fold_stray_sections(&got).is_none(), "and it is idempotent");
    }

    /// A stray sorted after `## Log` is there because the sort put it there, so
    /// it belongs to `Overview` — and the dated lines appended into it since
    /// belong back in the log.
    #[test]
    fn entries_swallowed_by_a_displaced_heading_come_back() {
        let b = "## Overview\nthe brief\n\n## Log\n- 2026-08-15 claimed\n\n## The shape\nlike so\n- 2026-08-16 → todo\n";
        let got = fold_stray_sections(b).expect("something to fold");
        assert_eq!(headings(&got), ["Overview", "Log"]);
        let log = section_of(&got, "Log").unwrap();
        assert!(log.contains("2026-08-15 claimed") && log.contains("2026-08-16 → todo"));
        let overview = section_of(&got, "Overview").unwrap();
        assert!(overview.contains("### The shape") && overview.contains("like so"));
        assert!(!overview.contains("→ todo"), "the entry is not left in the prose as well");
    }

    /// `Log` is append-only and belongs to `wsp note`, so it is not offered to
    /// an editor. Everything else in the vocabulary is, and the lists must not
    /// drift — `edit_prose` naming its own pair is what let `--decisions` fall
    /// through and overwrite `Overview`.
    ///
    /// A project's list is the whole body minus the log; a task's is that minus
    /// `Handbook`, which is the one section only a project has. Stated as two
    /// assertions over `SECTIONS` rather than two literals, so adding a section
    /// cannot leave one of them behind.
    #[test]
    fn the_editable_sections_are_the_body_minus_the_log() {
        let editable: Vec<&str> = SECTIONS.iter().copied().filter(|s| *s != "Log").collect();
        assert_eq!(PROJECT_PROSE.to_vec(), editable);
        assert_eq!(
            PROSE.to_vec(),
            editable.iter().copied().filter(|s| *s != "Handbook").collect::<Vec<_>>(),
        );
        assert!(!PROSE.contains(&"Log") && !PROJECT_PROSE.contains(&"Log"));
        // Order is the order a body is written in, so the two agree wherever
        // they overlap — a section list that sorted differently would rewrite
        // every body it touched.
        assert!(PROSE.iter().all(|s| PROJECT_PROSE.contains(s)));
    }
}
