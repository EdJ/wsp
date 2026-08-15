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
    let text = text.trim_end().to_string();
    match secs.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        Some(slot) => slot.1 = text,
        None if !text.trim().is_empty() => secs.push((name.to_string(), text)),
        None => {}
    }
    secs.retain(|(n, t)| !(n.is_empty() && t.trim().is_empty()));

    let rank = |n: &str| -> usize {
        if n.is_empty() {
            return 0;
        }
        SECTIONS
            .iter()
            .position(|s| s.eq_ignore_ascii_case(n))
            .map(|i| i + 1)
            .unwrap_or(SECTIONS.len() + 1)
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

/// The sections a person writes: [`SECTIONS`] without `Log`.
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
pub const PROSE: [&str; 3] = ["Overview", "Details", "Decisions"];

/// The headings a task or project body carries, in the order they are written.
///
/// `Overview` is what the thing is — written once, read to re-enter it later.
/// `Details` is working material: criteria, links, whatever the work needs.
/// `Decisions` is what was settled and now binds: dated, append-only, and not
/// to be edited, because a decision that can be quietly rewritten is not a
/// record of anything.
/// `Log` is the dated, append-only trail and is never edited by hand.
///
/// `Decisions` sits before `Log` deliberately. The log is where a thing has
/// been; a decision is a constraint on where it can go, which is worth reading
/// before the history rather than after it — and it keeps `Log` last, which is
/// what lets [`Task::log`] go on appending to the end of the body.
pub const SECTIONS: [&str; 4] = ["Overview", "Details", "Decisions", "Log"];

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
    pub fn log(&mut self, line: &str) {
        let stamp = util::today_ymd();
        if !self.body.contains("## Log") {
            if !self.body.is_empty() && !self.body.ends_with('\n') {
                self.body.push('\n');
            }
            if !self.body.is_empty() {
                self.body.push('\n');
            }
            self.body.push_str("## Log\n");
        } else if !self.body.ends_with('\n') {
            self.body.push('\n');
        }
        self.body.push_str(&format!("- {stamp} {line}\n"));
    }

    /// The text under `## <name>`, heading excluded.
    pub fn section(&self, name: &str) -> Option<String> {
        section_of(&self.body, name)
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `Log` is append-only and belongs to `wsp note`, so it is not offered to
    /// an editor. Everything else in the vocabulary is, and the two lists must
    /// not drift — `edit_prose` naming its own pair is what let `--decisions`
    /// fall through and overwrite `Overview`.
    #[test]
    fn the_editable_sections_are_the_body_minus_the_log() {
        let editable: Vec<&str> = SECTIONS.iter().copied().filter(|s| *s != "Log").collect();
        assert_eq!(PROSE.to_vec(), editable);
        assert!(!PROSE.contains(&"Log"));
    }
}
