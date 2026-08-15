//! What a task or a project looks like in full.
//!
//! Takes its data in as a [`Ctx`] rather than reaching for the store, so a
//! fixture can render a frame with nothing running — the same decision the
//! panel's `Snapshot` makes, for the same reason.

use crate::herdr;
use crate::model::{Status, Task};
use crate::panel::{self, line, Line, Style};
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util;

use super::Focus;

/// Break `text` to `w` columns on word boundaries, falling back to a hard cut
/// for a single word longer than the pane is wide.
fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(8);
    let mut out: Vec<String> = Vec::new();
    for para in text.split('\n') {
        if para.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for word in para.split_whitespace() {
            if word.chars().count() > w {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut rest: Vec<char> = word.chars().collect();
                while rest.len() > w {
                    out.push(rest[..w].iter().collect());
                    rest = rest[w..].to_vec();
                }
                cur = rest.into_iter().collect();
                continue;
            }
            let need = if cur.is_empty() { word.chars().count() } else { cur.chars().count() + 1 + word.chars().count() };
            if need > w {
                out.push(std::mem::take(&mut cur));
                cur = word.to_string();
            } else {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

const LABEL_W: usize = 9;

/// `label   value` — the shape every field in this view takes.
fn field(label: &str, value: &str, style: Style) -> Line {
    let mut l = Line::default();
    l.push(Style::Dim, util::pad(label, LABEL_W));
    if value.is_empty() {
        l.push(Style::Dim, "—");
    } else {
        l.push(style, value.to_string());
    }
    l
}

fn status_style(s: Status) -> Style {
    match s {
        Status::Blocked => Style::Warn,
        Status::Doing => Style::Accent,
        Status::Review => Style::Muted,
        Status::Done => Style::Dim,
        _ => Style::Plain,
    }
}

fn heading(text: &str) -> Line {
    line(Style::Dim, text.to_string())
}

/// Everything the view reads. Same shape of decision as the panel's Snapshot:
/// taking the data in means a fixture can render a frame with nothing running.
#[allow(dead_code)]
pub(crate) struct Ctx {
    pub tasks: Vec<Task>,
    pub index: Index,
    pub claims: std::collections::BTreeMap<String, serde_json::Value>,
    pub worked: std::collections::BTreeMap<String, serde_json::Value>,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    pub panes: Vec<herdr::Pane>,
}

impl Ctx {
    pub(super) fn live(store: &Store) -> Ctx {
        Ctx {
            tasks: store.tasks(),
            index: Index::new(store.projects()),
            claims: store.claims(),
            worked: store.worked(),
            bindings: store.bindings(),
            panes: herdr::panes().unwrap_or_default(),
        }
    }

    /// The pane working this task, if the live join says so.
    fn pane_for(&self, task: &str) -> Option<&herdr::Pane> {
        let pane_id = self.bindings.iter().find_map(|(pane, b)| {
            (b.get("task_id").and_then(|t| t.as_str()) == Some(task)).then(|| pane.clone())
        })?;
        self.panes.iter().find(|p| p.pane_id == pane_id)
    }
}

pub(crate) fn frame(ctx: &Ctx, focus: &Focus, w: usize, h: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    match focus {
        Focus::Nothing => {
            out.push(line(Style::Bold, "wsp"));
            out.push(line(Style::Dim, "─".repeat(w)));
            out.push(Line::default());
            for l in wrap("Nothing open. Press ↵ on a row in the panel.", w) {
                out.push(line(Style::Muted, l));
            }
        }
        Focus::Task(id) => task_frame(ctx, id, w, &mut out),
        Focus::Project(id) => project_frame(ctx, id, w, &mut out),
    }

    // A hint pinned to the bottom. `W` is worth naming here because it acts on
    // panes other than this one, which is not a thing you would guess.
    //
    // The section menu appears only in an edit tab, because that is the only
    // place its keys do anything. Offering three keys in a view with nothing
    // beside it would be advertising a menu whose every entry answers "no
    // editors open".
    let open = open_sections(ctx);
    let footer = if open.is_empty() { 2 } else { 3 };
    out.truncate(h.saturating_sub(footer));
    while out.len() < h.saturating_sub(footer) {
        out.push(Line::default());
    }
    out.push(line(Style::Dim, "─".repeat(w)));
    if !open.is_empty() {
        let mut menu = Line::default();
        for (i, name) in crate::model::PROSE.iter().enumerate() {
            if i > 0 {
                menu.push(Style::Dim, " · ");
            }
            let showing = open.iter().any(|s| s.eq_ignore_ascii_case(name));
            menu.push(Style::Dim, format!("{} ", section_key(name)));
            // Lit for what is on screen, muted for what a key would bring in.
            // The distinction is the whole point of drawing the menu: three
            // sections and two panes means one is always somewhere else.
            menu.push(
                if showing { Style::Accent } else { Style::Muted },
                name.to_lowercase(),
            );
        }
        menu.fit(w);
        out.push(menu);
    }
    out.push(line(
        Style::Dim,
        "h/l left or right · W save and close · q close, discarding",
    ));
    out
}

/// The key that brings a section in. Initials, not positions — `h`/`l` already
/// mean left and right in this pane, and one footer cannot teach both.
fn section_key(section: &str) -> char {
    match section {
        "Overview" => 'o',
        "Details" => 'd',
        _ => 'D',
    }
}

/// The sections open in editor panes beside this one.
///
/// Read from the panes the context already carries rather than asked for
/// again: the join is live, and a second round-trip to herdr on every repaint
/// would cost more than the line it draws. A storyboard `Ctx` holds synthetic
/// panes that share no tab with this process, so it finds none and the menu
/// stays off — which is right, since a still frame has no editors either.
fn open_sections(ctx: &Ctx) -> Vec<String> {
    let Some(me) = herdr::Env::read().pane_id else {
        return Vec::new();
    };
    let Some(tab) = ctx.panes.iter().find(|p| p.pane_id == me).map(|p| p.tab_id.clone()) else {
        return Vec::new();
    };
    ctx.panes
        .iter()
        .filter(|p| p.tab_id == tab && p.pane_id != me)
        .filter(|p| super::editors::is_section_label(&p.label))
        .map(|p| p.label.clone())
        .collect()
}

fn task_frame(ctx: &Ctx, id: &str, w: usize, out: &mut Vec<Line>) {
    let Some(t) = ctx.tasks.iter().find(|t| t.id == id) else {
        out.push(line(Style::Warn, format!("no task {id}")));
        return;
    };

    let mut head = Line::default();
    head.push(Style::Dim, "task ");
    head.push(Style::Bold, t.id.clone());
    out.push(head);
    out.push(line(Style::Dim, "─".repeat(w)));

    for l in wrap(&t.title, w) {
        out.push(line(Style::Plain, l));
    }
    out.push(Line::default());

    out.push(field("status", t.status().as_str(), status_style(t.status())));
    out.push(field("priority", t.priority().as_str(), Style::Plain));
    out.push(field("project", t.project.as_deref().unwrap_or("(inbox)"), Style::Plain));
    // What it is part of. A sub-task read on its own is missing the one thing
    // that says why it exists.
    if let Some(p) = t.parent.as_ref().and_then(|id| ctx.tasks.iter().find(|x| &x.id == id)) {
        out.push(field("under", &format!("{}  {}", p.id, p.title), Style::Muted));
    }

    // Own tags plus everything inherited from the project chain — the whole
    // point of inheritance is that it is invisible until something shows it.
    let inherited: Vec<String> = t
        .project
        .as_deref()
        .map(|p| ctx.index.effective_tags(p))
        .unwrap_or_default();
    let mut tags = t.tags.clone();
    for i in inherited {
        if !tags.contains(&i) {
            tags.push(i);
        }
    }
    out.push(field("tags", &tags.join(" "), Style::Muted));

    let age = util::age_days(&t.updated);
    out.push(field(
        "touched",
        &if age <= 0 { "today".to_string() } else { format!("{age}d ago") },
        if age > 30 { Style::Warn } else { Style::Plain },
    ));
    if !t.refs.is_empty() {
        out.push(field("refs", &t.refs.join("  "), Style::Muted));
    }

    // The join, spelled out: the durable claim and the live pane are different
    // facts and the panel only ever shows the second.
    out.push(Line::default());
    match ctx.claims.get(&t.id) {
        Some(c) => out.push(field("claimed", &crate::cmd_agent::claim_line(c), Style::Accent)),
        None => {
            out.push(field("claimed", "", Style::Dim));
            // The ghost: an agent works several tasks in sequence, and this is
            // the one it walked away from. Only once the claim is gone — while
            // one is live this is the shift before it, and the live fact wins
            // the line.
            if let Some(w) = ctx.worked.get(&t.id) {
                out.push(field("worked", &crate::cmd_agent::worked_line(w), Style::Muted));
            }
        }
    }
    match ctx.pane_for(&t.id) {
        Some(p) => {
            let what = if p.agent.is_empty() { "shell".to_string() } else { p.agent.clone() };
            out.push(field("pane", &format!("{} · {what} {}", p.pane_id, p.agent_status), Style::Plain));
        }
        None => out.push(field("pane", "", Style::Dim)),
    }

    // What is under it. The panel shows this as indentation and a count; here
    // there is room to say which children, and what state each is in — which
    // is the question you open a parent to ask.
    let kids = resolve::children_of(&ctx.tasks, &t.id);
    if !kids.is_empty() {
        let under = resolve::counts_under(&ctx.tasks, &t.id);
        out.push(Line::default());
        out.push(heading(&format!("sub-tasks · {} of {} open", under.open, kids.len())));
        for k in kids {
            let mut l = Line::default();
            l.push(Style::Dim, util::pad(k.status().as_str(), LABEL_W));
            l.push(status_style(k.status()), util::truncate(&k.title, w.saturating_sub(LABEL_W)));
            out.push(l);
        }
    }

    // The prose, in the order it is written: what this is, then what the work
    // needs. Both are optional and an empty one draws nothing rather than an
    // empty heading.
    for name in ["Overview", "Details", "Decisions"] {
        if let Some(text) = t.section(name) {
            out.push(Line::default());
            out.push(heading(&name.to_lowercase()));
            for l in wrap(&text, w) {
                if l.trim().is_empty() {
                    out.push(Line::default());
                } else {
                    out.push(line(Style::Plain, l));
                }
            }
        }
    }

    // The log, newest first — the reason the body exists is to be read after
    // the fact, and after the fact the last line is the one that matters.
    let log: Vec<&str> = t
        .body
        .lines()
        .filter(|l| l.trim_start().starts_with("- "))
        .collect();
    if !log.is_empty() {
        out.push(Line::default());
        out.push(heading("log"));
        for entry in log.iter().rev() {
            let text = entry.trim_start().trim_start_matches("- ");
            let mut first = true;
            for l in wrap(text, w.saturating_sub(2)) {
                let mut ln = Line::default();
                ln.push(Style::Dim, if first { "· " } else { "  " });
                ln.push(Style::Muted, l);
                out.push(ln);
                first = false;
            }
        }
    }
}

fn project_frame(ctx: &Ctx, id: &str, w: usize, out: &mut Vec<Line>) {
    let Some(p) = ctx.index.get(id).cloned() else {
        out.push(line(Style::Warn, format!("no project {id}")));
        return;
    };

    let mut head = Line::default();
    head.push(Style::Dim, "project ");
    head.push(Style::Bold, p.id.clone());
    out.push(head);
    out.push(line(Style::Dim, "─".repeat(w)));

    if p.name != p.id {
        for l in wrap(&p.name, w) {
            out.push(line(Style::Plain, l));
        }
        out.push(Line::default());
    }

    out.push(field("parent", p.parent.as_deref().unwrap_or("—"), Style::Plain));
    out.push(field("status", &p.status, Style::Plain));
    out.push(field("tags", &ctx.index.effective_tags(&p.id).join(" "), Style::Muted));
    if !p.roots.is_empty() {
        out.push(field("roots", &p.roots.join("  "), Style::Muted));
    }
    if !p.brief.is_empty() {
        out.push(Line::default());
        for l in wrap(&p.brief, w) {
            out.push(line(Style::Muted, l));
        }
    }

    // A project's prose, same as a task's — it is the same machinery now, and
    // a project is the natural home for "what is this and why".
    for name in ["Overview", "Details", "Decisions"] {
        if let Some(text) = p.section(name) {
            out.push(Line::default());
            out.push(heading(&name.to_lowercase()));
            for l in wrap(&text, w) {
                if l.trim().is_empty() {
                    out.push(Line::default());
                } else {
                    out.push(line(Style::Plain, l));
                }
            }
        }
    }

    let counts = resolve::counts_by_project(&ctx.index, &ctx.tasks);
    let c = counts.get(&p.id).copied().unwrap_or_default();
    out.push(Line::default());
    let mut totals = Line::default();
    totals.push(Style::Dim, util::pad("work", LABEL_W));
    totals.push(Style::Plain, format!("{} open", c.open));
    if c.doing > 0 {
        totals.push(Style::Dim, " · ");
        totals.push(Style::Accent, format!("{} doing", c.doing));
    }
    if c.blocked > 0 {
        totals.push(Style::Dim, " · ");
        totals.push(Style::Warn, format!("{} blocked", c.blocked));
    }
    if c.done > 0 {
        totals.push(Style::Dim, format!(" · {} done", c.done));
    }
    out.push(totals);

    let kids = ctx.index.children(Some(&p.id));
    if !kids.is_empty() {
        out.push(Line::default());
        out.push(heading("under it"));
        for k in kids {
            let kc = counts.get(&k.id).copied().unwrap_or_default();
            let mut l = Line::default();
            l.push(Style::Dim, "· ");
            l.push(Style::Plain, k.id.clone());
            let right = line(Style::Dim, if kc.open > 0 { kc.open.to_string() } else { "—".into() });
            l.pad(w.saturating_sub(l.width() + right.width()).max(1));
            l.spans.extend(right.spans);
            out.push(l);
        }
    }

    // Its own tasks, attention first — the same order the panel uses.
    let mut own: Vec<&Task> = ctx
        .tasks
        .iter()
        .filter(|t| t.project.as_deref() == Some(p.id.as_str()) && t.status().is_open())
        .collect();
    own.sort_by_key(|t| (t.status().rank(), t.priority().rank(), t.id.clone()));
    if !own.is_empty() {
        out.push(Line::default());
        out.push(heading("tasks"));
        for t in own {
            let mut l = Line::default();
            l.push(status_style(t.status()), format!("{} ", glyph_for(t.status())));
            l.push(Style::Plain, util::truncate(&t.title, w.saturating_sub(2)));
            out.push(l);
        }
    }
}

fn glyph_for(s: Status) -> &'static str {
    use panel::glyph as g;
    match s {
        Status::Blocked => g::BLOCKED,
        Status::Review => g::REVIEW,
        Status::Doing => g::DOING,
        Status::Done => g::DONE,
        _ => g::QUIET,
    }
}

