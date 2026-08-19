//! The durable entities. Everything here round-trips through Markdown +
//! frontmatter, so field names are the on-disk contract.

use crate::fm::{self, Doc, Val};
use crate::util;

pub const SCHEMA: &str = "1";

/// Where a task is, and — for the two that are not moving — *why* it is not.
///
/// `blocked` and `parked` are both stopped work and they are not the same
/// thing, which is the distinction this vocabulary went a long time without.
/// `blocked` is addressed to a person: a decision is owed, and until it is
/// given nobody can proceed. `parked` is addressed to nobody: it is a judgement
/// that the moment is wrong, and what it wants written down is the *trigger*
/// that should bring it back. Conflating them let the second kind rot —
/// t-260816-022 sat red on the panel for a day, its three revisit conditions
/// all come true in the log, looking exactly like work that wanted an answer.
///
/// So they part everywhere the two readings differ: `blocked` is loud, first in
/// `rank`, counted in the panel footer and in `wip`'s list of work waiting on a
/// person; `parked` is quiet, last of the open statuses, dim wherever it draws,
/// and absent from every count of what wants attention. Both are open — `parked`
/// work is work, and a claim, a worktree and a suffix lookup all still find it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Inbox,
    Todo,
    Doing,
    Blocked,
    /// Deliberately not yet: nobody is owed anything, and the reason on it is a
    /// condition rather than a question.
    Parked,
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
            // `paused` and `later` because the thing has three names in the
            // wild and the store should not be the place you have to remember
            // which one it picked.
            "parked" | "park" | "paused" | "later" => Some(Status::Parked),
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
            Status::Parked => "parked",
            Status::Review => "review",
            Status::Done => "done",
        }
    }
    pub fn is_open(&self) -> bool {
        !matches!(self, Status::Done)
    }
    /// Sort weight for listings: attention first.
    ///
    /// `parked` sorts below `inbox` and above `done` — the far end of the open
    /// work, which is the whole of what parking a task buys you. Unfiled work
    /// is still work somebody may pick up today; parked work is work somebody
    /// has already decided not to.
    pub fn rank(&self) -> u8 {
        match self {
            Status::Blocked => 0,
            Status::Review => 1,
            Status::Doing => 2,
            Status::Todo => 3,
            Status::Inbox => 4,
            Status::Parked => 5,
            Status::Done => 6,
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
    set_section_in_order(body, name, text, &SECTIONS);
}

/// [`set_section_in`] against a record's own set of sections.
///
/// The order is a parameter because a worklist's body is not a task's:
/// `## Groups` is its structure and belongs second, where [`SECTIONS`] would
/// file it as a heading it has never heard of and sort it after `Decisions`.
/// Everything else here is the same rule for the same reason, `Log` last
/// included, so the two do not drift — which is what a second copy of this
/// function would have bought.
pub fn set_section_in_order(body: &mut String, name: &str, text: &str, order: &[&str]) {
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
            return order.len() + 1;
        }
        order
            .iter()
            .position(|s| s.eq_ignore_ascii_case(n))
            .map(|i| i + 1)
            .unwrap_or(order.len())
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
///
/// Both stamp shapes count — see [`util::is_stamp`]. A body that a stray
/// heading has been swallowing entries from will have some of each in it, and
/// recognising only the newer ones would fold half a log back and leave half.
fn peel_trailing_dated(text: &str) -> (String, Vec<String>) {
    let dated = |l: &str| match l.trim().strip_prefix("- ").and_then(|r| r.split_once(' ')) {
        Some((d, _)) => util::is_stamp(d),
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
///
/// The stamp is the *instant*, in UTC, and not the date. This is the single
/// writer for both `## Log` and `## Decisions`, and it used to bake
/// `util::today_ymd()` — a UTC date — into text that is then stored and read
/// back as written. Everything recorded in the hours between local midnight and
/// UTC midnight was therefore filed on yesterday: four decisions taken at 01:15
/// CEST came back stamped `2026-08-16`, next to prose that said 2026-08-17.
///
/// Storing the instant is what makes that a rendering question again. The date
/// a reader sees is computed where it is shown, by [`util::local_ymd`], from a
/// record that still knows what hour it was. Lines written before this change
/// cannot be recovered — the hour was thrown away at the point of writing — so
/// they stand as they are and every reader here takes both shapes.
pub fn append_dated(body: &mut String, section: &str, line: &str) {
    append_dated_in_order(body, section, line, &SECTIONS);
}

/// [`append_dated`] against a record's own set of sections. See
/// [`set_section_in_order`] for why the order is a parameter — a worklist that
/// logged through the task order would have its `## Groups` re-filed under
/// every note written on it.
pub fn append_dated_in_order(body: &mut String, section: &str, line: &str, order: &[&str]) {
    let mut text = section_of(body, section).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("- {} {}\n", util::now_iso(), line));
    set_section_in_order(body, section, &text, order);
}

/// A stored body as it is shown: every stamp written by [`append_dated`]
/// replaced by the date it falls on in the reader's own zone.
///
/// For the paths that print or draw a body whole rather than going through
/// [`decisions`] — `wsp show`, `wsp project show`, the detail pane's log. They
/// have no structure to hang a conversion on, so the conversion is by shape:
/// a bullet whose first word is a `…T…Z` instant. Nothing else writes that at
/// the head of a list item, and a bare date — every line in the store older
/// than this — passes through untouched, which is what makes this safe to run
/// over a whole body rather than over two named sections.
///
/// Display only. The editors deliberately do **not** call it: `wsp edit
/// --decisions` puts the section in a buffer and writes back what comes out, so
/// localising on the way in would launder every instant in it into a date and
/// lose the hour for good — the exact damage this change exists to undo.
pub fn localise_dates(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for line in body.lines() {
        let stamp = line
            .trim_start()
            .strip_prefix("- ")
            .and_then(|r| r.split_once(' '))
            .map(|(d, _)| d)
            .filter(|d| d.len() == 20 && util::is_stamp(d));
        match stamp {
            Some(d) => {
                let (a, b) = line.split_once(d).unwrap_or((line, ""));
                out.push_str(a);
                out.push_str(&util::local_ymd(d));
                out.push_str(b);
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    if !body.ends_with('\n') {
        out.pop();
    }
    out
}

/// One entry of `## Decisions`, taken apart.
///
/// The stored line is `- <instant> (<id>[ supersedes <id>…]) <text>`, and the
/// parenthesised head is the identity this type exists for. Before it there was
/// none: entries are dated bullets, several share a date, and a later entry
/// therefore had no way to name the earlier one it replaces. The index in
/// `project show` prints the first sentence of each entry — the rule — so a
/// superseded entry stated the *wrong* rule with the correction some lines
/// below and nothing joining them. The link has to be in the record, not
/// worked out by the reader.
///
/// Stored in the line rather than beside it, because the line is what is
/// committed, what `--decisions` prints as written, and what somebody reads in
/// the file with no wsp to hand. A sidecar would be a second truth to keep.
///
/// The head is parsed strictly — `d` and digits, then optionally the literal
/// `supersedes` and more of the same — so a decision that merely opens with a
/// parenthesis is text, not a malformed head.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    /// The date it was taken, in the reader's zone. See [`decisions`].
    pub when: String,
    /// `d4`, or empty for an entry written before ids existed and not yet
    /// touched by [`append_decision`].
    pub id: String,
    /// Ids this entry replaces. Almost always one.
    pub supersedes: Vec<String>,
    /// The decision itself, with the head taken off.
    pub text: String,
}

/// Is this an id we minted — `d` and at least one digit, nothing else?
fn is_decision_id(s: &str) -> bool {
    matches!(s.strip_prefix('d'), Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `d4` → `4`, for the max-so-far scan. Anything else → `None`.
fn id_number(s: &str) -> Option<u32> {
    s.strip_prefix('d').and_then(|n| n.parse().ok())
}

/// Read `<id>` back to the shape it is stored in, from whatever was typed:
/// `d4` and `4` are the same reference, and refusing the second would be a
/// rule with nothing behind it.
pub fn decision_ref(s: &str) -> Option<String> {
    let n: u32 = s.trim().trim_start_matches('d').parse().ok()?;
    Some(format!("d{n}"))
}

/// Split `(d4 supersedes d1) text` into its head and its text. Returns `None`
/// when there is no head, which is every line written before ids and every
/// line somebody wrote by hand.
fn split_head(s: &str) -> Option<(String, Vec<String>, &str)> {
    let (inner, rest) = s.strip_prefix('(')?.split_once(')')?;
    let mut words = inner.split_whitespace();
    let id = words.next().filter(|w| is_decision_id(w))?.to_string();
    let supersedes: Vec<String> = match words.next() {
        None => Vec::new(),
        Some("supersedes") => {
            let ids: Vec<String> = words.map(|w| w.to_string()).collect();
            if ids.is_empty() || !ids.iter().all(|w| is_decision_id(w)) {
                return None;
            }
            ids
        }
        Some(_) => return None,
    };
    Some((id, supersedes, rest.trim_start()))
}

/// The `## Decisions` entries, oldest first and taken apart. See [`Decision`].
///
/// The one parser: [`decisions`] is this with the head thrown away, so a caller
/// that only wants the sentence cannot see a stale second copy of the format.
/// `model::PROSE` is in this file for the same reason, and the comment on it
/// says what the third list cost.
pub fn decisions_of(body: &str) -> Vec<Decision> {
    section_of(body, "Decisions")
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().trim_start_matches("- ").trim().to_string())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (when, rest) = match l.split_once(' ') {
                Some((d, rest)) if util::is_stamp(d) => (util::local_ymd(d), rest.trim()),
                _ => (String::new(), l.as_str()),
            };
            match split_head(rest) {
                Some((id, supersedes, text)) => Decision {
                    when,
                    id,
                    supersedes,
                    text: text.to_string(),
                },
                None => Decision { when, text: rest.to_string(), ..Default::default() },
            }
        })
        .collect()
}

/// For each entry, the id of the entry that superseded it, if any — the
/// back-reference, which is the direction every reader needs and the one the
/// record does not store. Parallel to `all`.
///
/// The last superseder wins, and an entry that supersedes something already
/// superseded does not undo the mark: both lines are struck, which is the
/// truth. Self-reference is ignored rather than refused; the writer will not
/// produce it and a hand-edit that does should not make the block unreadable.
pub fn supersessions(all: &[Decision]) -> Vec<Option<String>> {
    all.iter()
        .map(|d| {
            if d.id.is_empty() {
                return None;
            }
            all.iter()
                .filter(|o| o.id != d.id && o.supersedes.iter().any(|s| *s == d.id))
                .next_back()
                .map(|o| o.id.clone())
        })
        .collect()
}

/// The entries that still bind: everything [`decisions_of`] returns that
/// nothing later replaced.
///
/// For the abridged views — the brief prints the four most recent as the rules
/// in force, and a rule that was withdrawn is not one of them. `project show`
/// deliberately does *not* use this: it is the place you go to read the record,
/// and a record with the withdrawn entries quietly removed is the tidied
/// conclusion the append-only rule exists to prevent.
pub fn live_decisions(body: &str) -> Vec<Decision> {
    let all = decisions_of(body);
    let over = supersessions(&all);
    all.into_iter().zip(over).filter(|(_, o)| o.is_none()).map(|(d, _)| d).collect()
}

/// Write ids onto the entries that have none, in place, and say whether
/// anything changed.
///
/// Called by [`append_decision`], so a file gets numbered the first time a
/// decision is taken on it after this shipped. That is the one edit to an
/// existing decision line this code makes, and it is defensible where a
/// rewrite of the text would not be: it adds the handle and changes not one
/// word of what was decided, it lands in the same commit as a decision the
/// author is making anyway, and it is the only thing that makes `--supersedes`
/// usable against the entries already in the store. The alternative — numbering
/// by position at read time — is an id that moves when a line above it is
/// edited away, which is a reference that silently points at the wrong
/// decision.
///
/// Conservative on purpose: the id is inserted after the stamp and nothing else
/// on the line is touched, so a bullet somebody wrote by hand comes back in the
/// shape they wrote it.
pub fn number_decisions(body: &mut String) -> bool {
    let Some(text) = section_of(body, "Decisions") else {
        return false;
    };
    let mut next = next_decision_number(&text);
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let bare = line.trim().trim_start_matches("- ").trim();
        let rest = match bare.split_once(' ') {
            Some((d, r)) if util::is_stamp(d) => r.trim_start(),
            _ => bare,
        };
        if bare.is_empty() || split_head(rest).is_some() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        // Split at the text, wherever it starts, and put the head in front of
        // it: whatever came before — the bullet, the stamp — is copied across
        // untouched.
        let at = line.len() - rest.len();
        out.push_str(&line[..at]);
        out.push_str(&format!("(d{next}) "));
        out.push_str(rest);
        out.push('\n');
        next += 1;
        changed = true;
    }
    if changed {
        set_section_in(body, "Decisions", &out);
    }
    changed
}

/// One past the highest id in a `## Decisions` section, which is the next one
/// to hand out. Reads the ids rather than counting the lines, so an entry
/// deleted by hand does not hand its id to something else.
fn next_decision_number(text: &str) -> u32 {
    text.lines()
        .filter_map(|l| {
            let bare = l.trim().trim_start_matches("- ").trim();
            let rest = match bare.split_once(' ') {
                Some((d, r)) if util::is_stamp(d) => r.trim_start(),
                _ => bare,
            };
            split_head(rest).and_then(|(id, _, _)| id_number(&id))
        })
        .max()
        .map_or(1, |n| n + 1)
}

/// Append a decision with an identity, and return the id it was given.
///
/// Two writers for one section would be two formats within the month, so this
/// is [`append_dated`] with a head on the front rather than a second path to
/// the file.
///
/// Nothing checks that `supersedes` names entries that exist — the caller does,
/// where it can say what is there instead. A duplicate id could only come from
/// two agents deciding on the same file at once, which is a lost update that
/// would already have cost one of the decisions entirely.
pub fn append_decision(body: &mut String, text: &str, supersedes: &[String]) -> String {
    number_decisions(body);
    let id = format!("d{}", next_decision_number(&section_of(body, "Decisions").unwrap_or_default()));
    let head = if supersedes.is_empty() {
        id.clone()
    } else {
        format!("{id} supersedes {}", supersedes.join(" "))
    };
    append_dated(body, "Decisions", &format!("({head}) {text}"));
    id
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
///
/// The date comes back **local**, and this split is why the conversion belongs
/// here rather than in the writers: the record keeps the instant it was taken
/// at, and the one place that already takes a stored line apart for display is
/// the one place that has to put it back together in the reader's calendar.
///
/// The identity head comes off with the date — a caller that wants the sentence
/// wants the sentence, and `(d4 supersedes d1)` in front of it would be four
/// tokens of bookkeeping in the brief. [`decisions_of`] is the same parse with
/// the head kept, and this is written in terms of it so there is one format.
pub fn decisions(body: &str) -> Vec<(String, String)> {
    decisions_of(body).into_iter().map(|d| (d.when, d.text)).collect()
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
    ///
    /// Through [`localise_dates`] because a hit can land in the log, and a
    /// window forty characters wide that opens with twenty of stored stamp is
    /// the search failing at the one job described above. One copy of the body
    /// per task searched, which is nothing beside the scan that found it.
    pub fn prose_line(&self, needle: &str, width: usize) -> Option<String> {
        let n = needle.trim().to_ascii_lowercase();
        if n.is_empty() {
            return None;
        }
        let body = localise_dates(&self.body);
        let line = body
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
    /// How many agents may be working on this machine at once. `None` is no
    /// number, which is what every machine has until somebody sets one.
    ///
    /// **It is here rather than on the worklist that wants it**, and that is
    /// the decision (`wsp-092` d2), not an accident of where the field fitted.
    /// Three reasons, all of them things that have already happened:
    ///
    /// 1. It is a fact about the *machine*. The `batch` and `fork` governors
    ///    were told *4 local, 4 remote, shared between two* by hand, and then
    ///    had to renegotiate it between themselves because nothing held it.
    ///    Two worklists each holding a number neither can honour is that same
    ///    renegotiation with a file behind it.
    /// 2. [`crate::sharing`] already measured what is scarce here and it is
    ///    not agents — *"four agents thinking cost nothing, and four agents
    ///    building is what saturates"* — and what it measured, it measured
    ///    per machine.
    /// 3. `data-018` is open and is exactly this question. A copy on the
    ///    worklist would be the second place it lives, which is the failure
    ///    the whole worklist design exists to stop.
    ///
    /// **What it does not do, said here so it is not assumed.** This counts
    /// agents, and `data-018` measured that the thing worth counting is
    /// *builds*: three heavy builds started independently on 2026-08-19 —
    /// two governors and one agent each deciding correctly, none able to see
    /// the other two — and `wsp spawn` began failing while it lasted. A cap
    /// on agents would not have stopped that, because the three were within
    /// any agent count anybody would have set. (The load averages recorded
    /// against that incident are load averages and not CPU: processes blocked
    /// on I/O are in them, every agent was still making progress, and the
    /// figure should not be requoted as saturation.) So this field is *where
    /// the number lives*, not the answer — the missing half is a governor
    /// being able to ask "may I build now" and be told no, and it stays open
    /// on `data-018`.
    ///
    /// The seat has no record here — see [`valid_machine_name`]'s caller in
    /// `cmd_machine::add` — so today the number can only be set for an
    /// executor. That is why every reader takes the cap as an
    /// [`Option<usize>`] rather than a [`Machine`]: wherever the seat's own
    /// number ends up living, [`Group::parallelism`] is unchanged.
    pub agents: Option<usize>,
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
            // Absent, empty and unreadable all mean *no cap*, which is the
            // state every machine is in until somebody decides a number. A
            // default here would be this file inventing a policy, and a
            // rejected file would be a hand-edit that stops the daemon
            // reaching a machine over a typo in a field nothing dials.
            agents: doc.opt("agents").and_then(|v| v.trim().parse().ok()).filter(|n| *n > 0),
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
        // Written only when there is one, unlike the fields above it: an
        // `agents:` on every machine file would say "somebody thought about
        // this and chose no limit" about records written before the field
        // existed.
        if let Some(n) = self.agents {
            d.set_str("agents", &n.to_string());
        }
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

// ---- worklists --------------------------------------------------------
//
// Every item below carries `#[allow(dead_code)]` and none of them will keep
// it. The record is built one group ahead of the verbs that read it —
// `worklist` group 1 against groups 2 to 5 — deliberately, because everything
// after this types against these shapes and a shape changed later is four
// rebuilds. The attribute is the marker for "its caller has not landed yet",
// and it comes off with the `use`, one item at a time, rather than a blanket
// allow over the whole record that would go on hiding a genuinely dead
// function years from now.

/// Where a worklist is, and it is the only state the file holds.
///
/// Four words, all of them a *decision* somebody took: to start it, to stop
/// starting things, to be finished with it. Where the run is up to is not here
/// and is never written — the position is the first group not finished, read
/// off the tasks and off git, and a status you can compute cannot go stale.
/// The `batch` handbook is the evidence: its written table disagreed with what
/// was actually happening inside an hour, while the membership stayed true.
///
/// `held` and `done` are not the same stop. `held` is this run halted with
/// groups still ahead of it; `done` is somebody saying there is nothing left
/// to want from it. Collapsing them would lose the only distinction a reader
/// looking at a stalled list actually needs.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorklistStatus {
    Draft,
    Running,
    Held,
    Done,
}

#[allow(dead_code)]
impl WorklistStatus {
    pub fn parse(s: &str) -> Option<WorklistStatus> {
        match s.trim().to_ascii_lowercase().as_str() {
            "draft" => Some(WorklistStatus::Draft),
            "running" => Some(WorklistStatus::Running),
            "held" | "hold" => Some(WorklistStatus::Held),
            "done" | "closed" => Some(WorklistStatus::Done),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            WorklistStatus::Draft => "draft",
            WorklistStatus::Running => "running",
            WorklistStatus::Held => "held",
            WorklistStatus::Done => "done",
        }
    }

    /// Running is the state everything else keys on: a task may be in at most
    /// one *running* worklist, routing tries a running worklist's seat first,
    /// and only a running list has a barrier to wait at. Draft, held and done
    /// lists are plans, and a task may be in as many of those as somebody
    /// cares to plan it into.
    pub fn is_running(&self) -> bool {
        *self == WorklistStatus::Running
    }
}

/// One group in the queue: what may run at the same time, and what has to be
/// read before the barrier behind it opens.
///
/// **The ordinal is not here**, because the ordinal is the position: it is the
/// index in the queue, rewritten on every write, and a group reference
/// (`--group 3`) is typed and used inside a minute. Storing a stable id would
/// buy a handle nothing keeps. What it costs is that a log line saying "group
/// 3 passed" reads wrong once a group is inserted above it, which is why the
/// log names the *members*.
///
/// Mutual exclusion is not a field either, and that is the concept this shape
/// removes: two tasks that touch the same file must not be in one group, which
/// is advice about how to compose a group rather than machinery. It is what
/// produced zero land-time conflicts across fifteen agents, and it needs no
/// primitive to keep saying so.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Group {
    /// Task ids, referenced and not owned: a member stays in whatever project
    /// it lives in, which is the whole point of a worklist sitting outside
    /// one. The `batch` moved 26 tasks into a project and back out again for
    /// want of this.
    pub members: Vec<String>,
    /// `xN`: a cap on *the work*, not a budget — "only two of these at once,
    /// they sit near each other". Absent means as many as the machine allows,
    /// which is what every group in all three hand-run lists actually wanted.
    /// The agent budget is a fact about the machine and lives there; the
    /// effective number is the smaller of the two.
    pub cap: Option<usize>,
    /// The prose read at the barrier after this group, empty where there is
    /// none. Not a predicate: `fork`'s real rule was *"if any of the three
    /// goes badly, flag and stop rather than push through"*, which no boolean
    /// expresses. What wsp contributes is the obligation to write a sentence,
    /// not the judgement in it.
    ///
    /// A start condition on the next group is this, read at the same barrier
    /// by the same reader, which is why there is one field and not two.
    pub stop: String,
}

#[allow(dead_code)]
impl Group {
    /// How many of this group may actually be running at once: **the smaller
    /// of the group's `xN` and the machine's cap**, and `None` when neither
    /// side states one — start the whole group, which is what every group in
    /// all three hand-run lists wanted.
    ///
    /// The two numbers are different claims and that is why the smaller wins
    /// rather than either overriding. `xN` is a cap on *the work* — "only two
    /// of these at once, they sit near each other" — and it is asked for by
    /// whoever composed the group. [`Machine::agents`] is what the box will
    /// bear, and it is not negotiable by a list: two worklists running at
    /// once have nothing to settle between them because neither can exceed
    /// it, which is the renegotiation-by-hand that d2 is about.
    ///
    /// What that costs, stated so it is chosen rather than discovered: **"run
    /// this list hard tonight" has no expression on the list.** The only way
    /// to run one worklist harder is to raise the machine, which raises it for
    /// everything on the machine. That is the right trade while one laptop is
    /// the whole estate, and it is the thing to revisit when a worklist is
    /// running on an executor nobody is sitting at.
    ///
    /// The machine's cap is passed in rather than read from a [`Machine`]
    /// because the machine everything runs on today — the seat — has no
    /// record to read it off. See [`Machine::agents`].
    pub fn parallelism(&self, machine_cap: Option<usize>) -> Option<usize> {
        match (self.cap, machine_cap) {
            (Some(group), Some(machine)) => Some(group.min(machine)),
            (group, machine) => group.or(machine),
        }
    }
}

/// The columns a wrapped `stop:` breaks to, and the indent its continuations
/// carry — chosen so a group and its prose sit inside 80 with the two-space
/// leader, because this is a file people read and hand-edit ahead of the work.
#[allow(dead_code)]
const STOP_INDENT: &str = "        ";
#[allow(dead_code)]
const STOP_WIDTH: usize = 70;

/// The `## Groups` section, parsed. Line order is the queue.
///
/// The written ordinal is *ignored*: it is a rendering of the position, so a
/// hand-edit that inserts a group in the middle without renumbering still
/// means what it looks like it means. [`render_groups`] puts the numbers back.
///
/// Forgiving in the two ways a hand-edit is actually wrong. A group line with
/// no ordinal is a group; indented lines before any `stop:` are more members,
/// because seven ids do not always fit on one line and somebody will wrap
/// them. What it is not forgiving about is prose: this section *is* the
/// structure — that is the whole reason it is safe in the body where the
/// `batch` handbook's table was not — so anything in it that is not a group
/// does not survive the next write, and there is nowhere in it for a comment
/// to live.
#[allow(dead_code)]
pub fn parse_groups(text: &str) -> Vec<Group> {
    let cap_of = |t: &str| -> Option<usize> {
        // `x` and `×` both, because the design wrote one and the keyboard has
        // the other, and being told your cap is a task id is a poor way to
        // learn which.
        let n = t.strip_prefix('x').or_else(|| t.strip_prefix('X')).or_else(|| t.strip_prefix('×'))?;
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        n.parse().ok()
    };

    let mut out: Vec<Group> = Vec::new();
    let mut in_stop = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            let mut g = Group::default();
            let mut toks = rest.split_whitespace().peekable();
            // The ordinal, if it is written. Unambiguous: a member is
            // `<project>-NNN` and never bare digits.
            if toks.peek().is_some_and(|t| t.bytes().all(|b| b.is_ascii_digit())) {
                toks.next();
            }
            if let Some(n) = toks.peek().and_then(|t| cap_of(t)) {
                g.cap = Some(n);
                toks.next();
            }
            g.members = toks.map(|t| t.to_string()).collect();
            out.push(g);
            in_stop = false;
            continue;
        }
        let Some(last) = out.last_mut() else { continue };
        if let Some(rest) = trimmed.strip_prefix("stop:") {
            last.stop = rest.trim().to_string();
            in_stop = true;
        } else if in_stop {
            if !last.stop.is_empty() {
                last.stop.push(' ');
            }
            last.stop.push_str(trimmed);
        } else {
            last.members.extend(trimmed.split_whitespace().map(|t| t.to_string()));
        }
    }
    out
}

/// The `## Groups` section, written. The inverse of [`parse_groups`], and the
/// pair round-trips: the ordinals are the positions, `xN` is emitted in ASCII
/// whatever it was read as, and a `stop` that wraps comes back as the one
/// paragraph it was.
#[allow(dead_code)]
pub fn render_groups(groups: &[Group]) -> String {
    let mut out = String::new();
    for (i, g) in groups.iter().enumerate() {
        out.push_str(&format!("- {}", i + 1));
        if let Some(n) = g.cap {
            out.push_str(&format!("  x{n}"));
        }
        for m in &g.members {
            out.push_str("  ");
            out.push_str(m);
        }
        out.push('\n');
        if !g.stop.trim().is_empty() {
            for (n, line) in util::wrap(g.stop.trim(), STOP_WIDTH).iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                out.push_str(if n == 0 { "  stop: " } else { STOP_INDENT });
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// A worklist's body sections. `Groups` is second because it is what the file
/// is for; `Overview` above it is where the first group's start condition
/// lives, there being no group before it to carry one.
#[allow(dead_code)]
pub const WORKLIST_SECTIONS: [&str; 4] = ["Overview", "Groups", "Decisions", "Log"];

/// A queue of groups of task references, run in order, with a barrier between
/// each group and the next.
///
/// Durable and committed, one file per list beside `tasks/`, `projects/` and
/// `machines/`, because a worklist is a **plan** and the history of a plan is
/// worth having. The seat running it is not: that goes in `governors.json`
/// with the others, on the same rule — the hierarchy is committed and the
/// agent standing in it is not.
///
/// It sits **outside** the projects its members live in and **references**
/// them. A project is backlog organisation; a worklist is a set of tasks to be
/// picked up, in a specific order. The `batch` ran as a project because that
/// was the only thing that could hold a set, and it cost 26 tasks a move into
/// it and a move back out.
///
/// Frontmatter is four scalars, which is all `fm` takes, and the groups are in
/// the body: a list of groups is richer than `key: scalar`, so this is forced
/// rather than chosen. It is safe there for the reason `## Decisions` is safe
/// and the `batch` handbook was not — that table described a structure held
/// somewhere else and went stale within the hour, and this section *is* the
/// structure, so it has nothing to disagree with.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct Worklist {
    /// The slug, and the file stem. It shares a key space with project ids,
    /// because `governors.json` is keyed on one and a worklist takes a seat of
    /// its own — see `Store::scope_taken`, which is where that is enforced.
    pub id: String,
    pub title: String,
    pub status_raw: String,
    pub created: String,
    pub body: String,
}

#[allow(dead_code)]
impl Worklist {
    pub fn new(id: &str, title: &str) -> Worklist {
        Worklist {
            id: id.to_string(),
            title: title.to_string(),
            status_raw: WorklistStatus::Draft.as_str().to_string(),
            created: util::now_iso(),
            body: String::new(),
        }
    }

    /// Stored raw and parsed on the way out, as a task's status is: a word on
    /// disk that this build does not know is a word to show somebody, not one
    /// to launder into `draft` on the next write.
    pub fn status(&self) -> WorklistStatus {
        WorklistStatus::parse(&self.status_raw).unwrap_or(WorklistStatus::Draft)
    }

    pub fn set_status(&mut self, s: WorklistStatus) {
        self.status_raw = s.as_str().to_string();
    }

    pub fn from_doc(doc: &Doc, fallback_id: &str) -> Worklist {
        let id = doc.opt("id").unwrap_or_else(|| fallback_id.to_string());
        Worklist {
            title: doc.opt("title").unwrap_or_else(|| id.clone()),
            id,
            status_raw: doc.opt("status").unwrap_or_else(|| "draft".into()),
            created: doc.str("created"),
            body: doc.body.clone(),
        }
    }

    pub fn to_doc(&self) -> Doc {
        let mut d = Doc::default();
        d.set_str("id", &self.id);
        d.set_str("title", &self.title);
        d.set_str("status", &self.status_raw);
        d.set_str("created", &self.created);
        d.set_str("schema", SCHEMA);
        d.body = self.body.clone();
        d
    }

    /// The queue. Read out of the body every time rather than held beside it,
    /// so there is one copy of the membership and a hand-edit ahead of the
    /// work is simply read.
    pub fn groups(&self) -> Vec<Group> {
        parse_groups(&self.section("Groups").unwrap_or_default())
    }

    /// Write the queue back, renumbering it. Whether an edit is *allowed* is
    /// not asked here: the edit window falls out of the derived position — a
    /// group at or behind it is frozen — and that is the verbs' question, at
    /// the moment a person can be told which group is running.
    pub fn set_groups(&mut self, groups: &[Group]) {
        set_section_in_order(&mut self.body, "Groups", &render_groups(groups), &WORKLIST_SECTIONS);
    }

    /// The text under `## <name>`, heading excluded.
    pub fn section(&self, name: &str) -> Option<String> {
        section_of(&self.body, name)
    }

    /// Append a timestamped line under `## Log`, in this record's own section
    /// order — see [`append_dated_in_order`].
    pub fn log(&mut self, line: &str) {
        append_dated_in_order(&mut self.body, "Log", line, &WORKLIST_SECTIONS);
    }

    pub fn render(&self) -> String {
        fm::emit(&self.to_doc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the vocabulary went without: `blocked` and `parked` are
    /// both stopped and they ask for opposite things. This pins the three
    /// facts every surface reads off the enum — that they are separate values,
    /// that `parked` is still open work, and that it sorts to the far end of
    /// the open statuses where nothing looking for something to do will find
    /// it first.
    #[test]
    fn parked_is_open_work_that_sorts_out_of_the_way() {
        assert_ne!(Status::parse("parked"), Status::parse("blocked"));
        assert_eq!(Status::parse("parked"), Some(Status::Parked));
        // Three names in the wild, one status. `wsp ls -s paused` is a
        // question somebody will ask, and being told there is no such status
        // teaches nothing.
        for spelling in ["park", "paused", "later", "PARKED", " parked "] {
            assert_eq!(Status::parse(spelling), Some(Status::Parked), "{spelling}");
        }
        assert_eq!(Status::Parked.as_str(), "parked", "the on-disk word");

        assert!(Status::Parked.is_open(), "parked work is unfinished work");
        assert!(
            Status::Parked.rank() > Status::Inbox.rank(),
            "below even unfiled work: nobody has decided not to do that yet"
        );
        assert!(Status::Parked.rank() < Status::Done.rank(), "and above what is over");
        assert!(
            Status::Blocked.rank() < Status::Parked.rank(),
            "blocked is first because somebody owes it an answer; parked is last because nobody does"
        );
    }

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

    /// The defect this whole shape exists for: `project show` abridges each
    /// decision to its first sentence, so an entry a later one supersedes
    /// states the withdrawn rule with nothing saying so. The link has to be in
    /// the record, which means the earlier entry needs a name.
    #[test]
    fn a_decision_can_name_the_one_it_supersedes() {
        let mut b = String::new();
        let first = append_decision(&mut b, "the store commits with git add -A", &[]);
        let second =
            append_decision(&mut b, "the commit is scoped to the paths a command wrote", &[first.clone()]);
        assert_eq!((first.as_str(), second.as_str()), ("d1", "d2"), "ids in the order taken");

        let all = decisions_of(&b);
        assert_eq!(all[1].supersedes, vec!["d1"]);
        assert_eq!(all[0].text, "the store commits with git add -A", "the head is not the text");
        assert_eq!(
            supersessions(&all),
            vec![Some("d2".to_string()), None],
            "the back-reference is what a reader needs, and only the renderer can work it out"
        );
        assert_eq!(live_decisions(&b).len(), 1, "one rule binds, and it is the later one");
        assert_eq!(live_decisions(&b)[0].id, "d2");
    }

    /// A decision taken before ids existed is still a decision, and the point
    /// of `--supersedes` is naming exactly those. So the first write after this
    /// shipped hands them their ids, and nothing else about the line moves.
    #[test]
    fn entries_written_before_ids_get_one_when_the_next_decision_is_taken() {
        let mut b = String::from(
            "## Decisions\n- 2026-08-15 an uncommitted file in ~/wsp is not yours\n\
             - 2026-08-16T09:00:00Z measure it, do not assume it\n",
        );
        let id = append_decision(&mut b, "the commit is scoped to what a command wrote", &["d1".into()]);
        assert_eq!(id, "d3");
        let all = decisions_of(&b);
        let ids: Vec<&str> = all.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["d1", "d2", "d3"], "numbered oldest first");
        assert_eq!(all[0].text, "an uncommitted file in ~/wsp is not yours", "text untouched");
        assert_eq!(all[0].when, "2026-08-15", "and so is a bare date from before instants");
        assert_eq!(supersessions(&all)[0], Some("d3".to_string()));
    }

    /// The head is `(d4 …)` and nothing else is. A decision that happens to
    /// open with a parenthesis is prose, and reading it as an identity would
    /// lose the front of the sentence.
    #[test]
    fn a_leading_parenthesis_is_not_an_identity() {
        let b = String::from(
            "## Decisions\n- 2026-08-15 (provisionally) we ship the smaller half first\n\
             - 2026-08-16 (see wsp-012) and then the rest\n",
        );
        let all = decisions_of(&b);
        assert_eq!(all[0].text, "(provisionally) we ship the smaller half first");
        assert_eq!(all[1].text, "(see wsp-012) and then the rest");
        assert!(all.iter().all(|d| d.id.is_empty()), "no head, so no id");
    }

    /// Ids are read back, not counted. An entry deleted by hand must not hand
    /// its number to the next decision, or a `supersedes` written yesterday
    /// starts pointing at something written today.
    #[test]
    fn a_deleted_entry_does_not_hand_its_id_on() {
        let mut b = String::from("## Decisions\n- 2026-08-15T09:00:00Z (d7) the one that is left\n");
        assert_eq!(append_decision(&mut b, "and the next", &[]), "d8");
    }

    /// `d4` and `4` are the same reference. The strictness that matters is on
    /// what goes *into* the file.
    #[test]
    fn a_reference_is_taken_in_either_shape_and_stored_in_one() {
        assert_eq!(decision_ref("4").as_deref(), Some("d4"));
        assert_eq!(decision_ref(" d4 ").as_deref(), Some("d4"));
        assert_eq!(decision_ref("d4x"), None);
        assert_eq!(decision_ref(""), None);
    }

    /// The hour is the whole fix. A dated line used to store a UTC *date*, so
    /// a decision taken at 01:15 CEST was filed on the previous day and there
    /// was nothing left in the record to tell anyone otherwise. Storing the
    /// instant is what makes the date a rendering question again.
    #[test]
    fn a_dated_line_stores_the_instant_it_was_written_at() {
        let mut b = String::new();
        append_dated(&mut b, "Log", "claimed by pane w3K:p1");
        let stored = section_of(&b, "Log").expect("a log");
        let stamp = stored.trim().trim_start_matches("- ").split(' ').next().unwrap();
        assert_eq!(stamp.len(), 20, "a date, not an instant, and the hour is gone: {stored:?}");
        assert!(util::is_stamp(stamp) && stamp.ends_with('Z'), "{stored:?}");
    }

    /// The two halves of the same rule, on one body: what was written today
    /// carries an instant and is converted; what was written before this
    /// landed carries a date and is left alone, because there is no hour in it
    /// to convert from and inventing one would be worse than the bug.
    #[test]
    fn a_body_is_shown_with_dates_in_it_whatever_shape_it_was_stored_in() {
        let body = "## Overview\nwhat this is\n\n\
                    ## Log\n- 2026-08-14 claimed by pane w2:p1\n\
                    - 2026-08-16T23:15:00Z blocked: which offset source\n";
        let shown = localise_dates(body);
        assert!(shown.contains("- 2026-08-14 claimed"), "an old line is not touched: {shown}");
        assert!(!shown.contains('Z'), "no stored stamp reaches a reader: {shown}");
        assert!(
            shown.contains(&format!("- {} blocked:", util::local_ymd("2026-08-16T23:15:00Z"))),
            "the new line reads in the reader's own calendar: {shown}"
        );
        assert_eq!(shown.lines().count(), body.lines().count(), "line for line");
    }

    /// Prose is prose. The conversion is by shape, over a whole body, so the
    /// thing it must never do is rewrite something that only looks like a
    /// stamp — or lose the text either side of one it does convert.
    #[test]
    fn text_that_is_not_a_stored_stamp_goes_out_as_written() {
        for s in [
            "## Overview\n- 2026-08-16 is when we noticed\n",
            "- not a date at all\n",
            "the instant 2026-08-16T23:15:00Z appears mid-sentence\n",
            "",
        ] {
            assert_eq!(localise_dates(s), s, "rewritten and it should not have been");
        }
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

    /// The cap is a number somebody decided, so "nobody has decided" has to
    /// survive the file. Absent, empty and zero all come back as no cap —
    /// zero because a record that said `agents: 0` would be read as either
    /// "no limit" or "run nothing here" depending on who was reading it, and
    /// the command refuses to write one for the same reason.
    #[test]
    fn a_machine_with_no_agent_cap_is_not_a_machine_capped_at_none() {
        let none = Machine::from_doc(&fm::parse("---\nname: mb2\n---\n"), "mb2");
        assert_eq!(none.agents, None, "a record written before the field is uncapped");
        assert!(!none.render().contains("agents"), "and does not gain a line saying so");

        for text in ["---\nname: mb2\nagents:\n---\n", "---\nname: mb2\nagents: 0\n---\n", "---\nname: mb2\nagents: four\n---\n"] {
            assert_eq!(Machine::from_doc(&fm::parse(text), "mb2").agents, None, "{text}");
        }

        let mut m = Machine::new("mb2", "mb2");
        m.agents = Some(4);
        let round = Machine::from_doc(&fm::parse(&m.render()), "ignored");
        assert_eq!(round.agents, Some(4), "a number that was decided round-trips");
    }

    /// The rule d2 turns on, in all four of its states. A group asks and the
    /// machine grants: neither number overrides the other, because they are
    /// different claims — `xN` is a cap on the work, the machine's is what the
    /// box will bear — and the smaller is the only answer that honours both.
    #[test]
    fn the_number_that_may_run_is_the_smaller_of_what_the_group_asks_and_the_machine_allows() {
        let uncapped = Group { members: vec!["a-001".into()], ..Default::default() };
        let asked = Group { cap: Some(2), ..uncapped.clone() };

        assert_eq!(uncapped.parallelism(None), None, "nobody stated a number: run the group");
        assert_eq!(uncapped.parallelism(Some(4)), Some(4), "the machine alone still binds it");
        assert_eq!(asked.parallelism(None), Some(2), "so does the group alone");
        assert_eq!(asked.parallelism(Some(4)), Some(2), "a group may ask for fewer than it is allowed");
        assert_eq!(
            Group { cap: Some(6), ..uncapped }.parallelism(Some(4)),
            Some(4),
            "and may not ask for more — this is the renegotiation d2 removes",
        );
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
        // One of each shape, which is what a body that has been swallowing
        // entries across this change actually looks like. Recognising only the
        // newer stamp would fold half a log back and leave half behind.
        let b = "## Overview\nthe brief\n\n## Log\n- 2026-08-15 claimed\n\n## The shape\nlike so\n- 2026-08-16 → todo\n- 2026-08-17T09:04:00Z → doing\n";
        let got = fold_stray_sections(b).expect("something to fold");
        assert_eq!(headings(&got), ["Overview", "Log"]);
        let log = section_of(&got, "Log").unwrap();
        assert!(log.contains("2026-08-15 claimed") && log.contains("2026-08-16 → todo"));
        assert!(log.contains("2026-08-17T09:04:00Z → doing"), "the instant-stamped entry too: {log}");
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

    // ---- the worklist record ---------------------------------------------

    /// The format, exactly as it is written in a file somebody hand-edits, and
    /// back out again. The line format is the shape everything downstream types
    /// against, so this asserts the reading of a written file rather than the
    /// symmetry of two functions written together.
    #[test]
    fn a_group_line_says_what_may_run_at_once_and_what_to_read_before_going_on() {
        let written = "\
- 1  robustness-069
  stop: this changes how spawn behaves for every agent after it. If it does
        not land clean, stop — the whole night's spawning depends on it.
- 2  render-071  render-022  data-006
- 3  x2  render-041  render-068  render-072
";
        let gs = parse_groups(written);
        assert_eq!(gs.len(), 3, "three lines, three groups");
        assert_eq!(gs[0].members, ["robustness-069"]);
        assert_eq!(gs[0].cap, None, "no cap is as many as the machine allows");
        assert!(
            gs[0].stop.starts_with("this changes how spawn behaves")
                && gs[0].stop.ends_with("depends on it."),
            "the wrapped prose is one paragraph again: {:?}",
            gs[0].stop,
        );
        assert_eq!(gs[1].members.len(), 3);
        assert_eq!(gs[2].cap, Some(2), "`x2` is a cap on the work");
        assert_eq!(gs[2].members, ["render-041", "render-068", "render-072"]);

        // Round trip. What the file said comes back as what the file said,
        // which is what makes the body safe to hold the structure in.
        assert_eq!(render_groups(&gs), written);
        assert_eq!(parse_groups(&render_groups(&gs)), gs);
    }

    /// The ordinal is a rendering of the position, not an id. So the queue is
    /// line order, a hand-edit that inserts a group without renumbering means
    /// what it looks like it means, and the next write puts the numbers right.
    #[test]
    fn the_ordinal_is_the_position_and_is_rewritten_rather_than_trusted() {
        let gs = parse_groups("- 9  a-001\n- 1  b-002\n- c-003\n");
        assert_eq!(gs.len(), 3);
        assert_eq!(gs[0].members, ["a-001"], "line order is the queue");
        assert_eq!(gs[2].members, ["c-003"], "and a line with no ordinal is still a group");
        assert_eq!(render_groups(&gs), "- 1  a-001\n- 2  b-002\n- 3  c-003\n");
    }

    /// `×` is what the design wrote and `x` is what the keyboard has, so both
    /// read and one is written. Being told your cap is a task id would be a
    /// poor way to learn which.
    #[test]
    fn a_cap_is_read_in_either_of_the_two_ways_it_gets_typed() {
        for line in ["- 1  ×2  a-001  a-002", "- 1  x2  a-001  a-002", "- 1  X2  a-001  a-002"] {
            let gs = parse_groups(line);
            assert_eq!(gs[0].cap, Some(2), "{line}");
            assert_eq!(gs[0].members, ["a-001", "a-002"], "{line}");
        }
        assert_eq!(render_groups(&parse_groups("- 1  ×2  a-001")), "- 1  x2  a-001\n");
    }

    /// Seven ids do not always fit on a line, and somebody will wrap them.
    /// Members before a `stop:` continue; everything after it is the prose.
    #[test]
    fn a_hand_wrapped_group_is_read_as_the_one_group_it_looks_like() {
        let gs = parse_groups(
            "- 2  render-071  render-022  data-006\n     wsp-043  render-057\n  stop: look at the diff\n        before going on\n",
        );
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].members.len(), 5, "the continuation is more members");
        assert_eq!(gs[0].stop, "look at the diff before going on");
    }

    /// A worklist is a file, and the file is the record: four scalars and the
    /// groups in the body, because `fm` takes `key: scalar` and a list of
    /// groups is richer than that.
    #[test]
    fn a_worklist_round_trips_through_the_markdown_it_is_stored_as() {
        let mut w = Worklist::new("batch", "Overnight batch");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[
            Group { members: vec!["robustness-069".into()], cap: None, stop: "stop rather than push through".into() },
            Group { members: vec!["render-041".into(), "render-068".into()], cap: Some(2), stop: String::new() },
        ]);
        w.log("group robustness-069 passed");

        let got = Worklist::from_doc(&fm::parse(&w.render()), "batch");
        assert_eq!(got.id, "batch");
        assert_eq!(got.title, "Overnight batch");
        assert_eq!(got.status(), WorklistStatus::Running);
        assert_eq!(got.groups(), w.groups(), "the queue survives the disk");
        assert_eq!(got.groups()[1].cap, Some(2));
        assert!(got.section("Log").unwrap().contains("robustness-069 passed"));
    }

    /// `## Groups` is the file's structure and belongs second, above the
    /// history that accumulates under it. A worklist logged through the task
    /// section order would have its groups re-filed after `## Decisions` by
    /// every note written on it — the same defect that put three log entries
    /// under a heading nothing would ever read.
    #[test]
    fn writing_a_worklists_log_does_not_move_its_groups() {
        let mut w = Worklist::new("batch", "Overnight batch");
        set_section_in_order(&mut w.body, "Overview", "what this night is for", &WORKLIST_SECTIONS);
        w.set_groups(&[Group { members: vec!["a-001".into()], ..Default::default() }]);
        set_section_in_order(&mut w.body, "Decisions", "- 2026-08-19 a thing", &WORKLIST_SECTIONS);
        w.log("started");
        w.log("group a-001 passed");

        assert_eq!(headings(&w.body), ["Overview", "Groups", "Decisions", "Log"]);
        assert_eq!(w.groups().len(), 1, "and the queue is still readable: {}", w.body);
        assert_eq!(w.section("Log").unwrap().lines().count(), 2);
    }

    /// The only state the file holds is the one somebody decides. `running` is
    /// what everything else keys on, and a word this build does not know is
    /// shown rather than laundered into `draft` on the next write.
    #[test]
    fn a_worklists_status_is_a_decision_and_nothing_about_where_it_is_up_to() {
        assert!(WorklistStatus::parse("running").unwrap().is_running());
        assert!(!WorklistStatus::parse("held").unwrap().is_running());
        assert_eq!(WorklistStatus::parse("draft"), Some(WorklistStatus::Draft));
        assert_eq!(WorklistStatus::parse("finished"), None);

        let mut w = Worklist::new("batch", "Overnight batch");
        assert_eq!(w.status(), WorklistStatus::Draft, "a new list has not been started");
        w.status_raw = "abandoned".into();
        assert_eq!(w.status(), WorklistStatus::Draft, "an unknown word reads as the default");
        assert!(w.render().contains("status: abandoned"), "and is still on disk to be seen");
    }
}
