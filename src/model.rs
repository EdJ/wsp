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

/// True when either prose section carries anything — what the tree uses to
/// mark a row as having something written on it.
pub fn has_prose(body: &str) -> bool {
    section_of(body, "Overview").is_some() || section_of(body, "Details").is_some()
}

/// The headings a task body carries, in the order they are written.
///
/// `Overview` is what the task is — written once, read to re-enter it later.
/// `Details` is working material: criteria, links, whatever the work needs.
/// `Log` is the dated, append-only trail and is never edited by hand.
pub const SECTIONS: [&str; 3] = ["Overview", "Details", "Log"];

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
