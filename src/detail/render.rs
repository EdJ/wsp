//! What a task or a project looks like in full.
//!
//! Takes its data in as a [`Ctx`] rather than reaching for the store, so a
//! fixture can render a frame with nothing running — the same decision the
//! panel's `Snapshot` makes, for the same reason.

use crate::live::{self, AgentRef};
use crate::model::{Priority, Status, Task};
use crate::panel::{self, line, Line, Style};
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util;

use super::Focus;

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
    pub panes: Vec<AgentRef>,
    /// The sections in the editor columns beside this pane, left to right.
    ///
    /// Cannot be derived from `panes`: a pane row carries no geometry, so which
    /// label is the second column across takes a separate call. Filled once per
    /// repaint by `live`, and left empty by a storyboard, where there are no
    /// editors and the menu is rightly absent.
    pub columns: Vec<String>,
}

impl Ctx {
    pub(super) fn live(store: &Store) -> Ctx {
        Ctx {
            tasks: store.tasks(),
            index: Index::new(store.projects()),
            claims: store.claims(),
            worked: store.worked(),
            bindings: store.bindings(),
            panes: live::panes(),
            columns: super::editors::editor_columns(),
        }
    }

    /// The pane working this task, if the live join says so.
    fn pane_for(&self, task: &str) -> Option<&AgentRef> {
        let pane_id = self.bindings.iter().find_map(|(pane, b)| {
            (b.get("task_id").and_then(|t| t.as_str()) == Some(task)).then(|| pane.clone())
        })?;
        self.panes.iter().find(|p| p.pane == pane_id)
    }
}

pub(crate) fn frame(ctx: &Ctx, focus: &Focus, w: usize, h: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    match focus {
        Focus::Nothing => {
            out.push(line(Style::Bold, "wsp"));
            out.push(line(Style::Dim, "─".repeat(w)));
            out.push(Line::default());
            for l in util::wrap("Nothing open. Press ↵ on a row in the panel.", w) {
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
    let cols = &ctx.columns;
    let footer = if cols.is_empty() { 2 } else { 3 };
    out.truncate(h.saturating_sub(footer));
    while out.len() < h.saturating_sub(footer) {
        out.push(Line::default());
    }
    out.push(line(Style::Dim, "─".repeat(w)));
    if !cols.is_empty() {
        out.push(menu_line(cols, w));
    }
    out.push(line(
        Style::Dim,
        if cols.is_empty() {
            "h/l left or right · W save and close · q close, discarding"
        } else {
            "o/d/D then 1-3 places a section · 1/2/3 alone sets the columns · W save · q discard"
        }
        .to_string(),
    ));
    out
}

/// The menu: what is in each column, and what a key would bring in.
///
/// Two halves, because there are two questions. On the left, the columns as
/// they are — numbered, because the number is what you type. On the right, the
/// sections that are not on screen, so `D` has something to point at when
/// decisions is nowhere. A section that is showing is never repeated on the
/// right; the whole value of the line is telling those two states apart.
fn menu_line(cols: &[String], w: usize) -> Line {
    let mut menu = Line::default();
    for (i, section) in cols.iter().enumerate() {
        if i > 0 {
            menu.push(Style::Dim, "  ");
        }
        menu.push(Style::Dim, format!("{} ", i + 1));
        menu.push(Style::Accent, section.to_lowercase());
    }
    let elsewhere: Vec<&&str> = crate::model::PROSE
        .iter()
        .filter(|s| !cols.iter().any(|c| c.eq_ignore_ascii_case(s)))
        .collect();
    if !elsewhere.is_empty() {
        menu.push(Style::Dim, "   ·   ");
        for (i, section) in elsewhere.iter().enumerate() {
            if i > 0 {
                menu.push(Style::Dim, "  ");
            }
            menu.push(Style::Dim, format!("{} ", section_key(section)));
            menu.push(Style::Muted, section.to_lowercase());
        }
    }
    menu.fit(w);
    menu
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

    for l in util::wrap(&t.title, w) {
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
            let what = if p.agent { p.kind.clone() } else { "shell".to_string() };
            out.push(field("pane", &format!("{} · {what} {}", p.pane, p.state), Style::Plain));
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
            for l in util::wrap(&text, w) {
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
            for l in util::wrap(text, w.saturating_sub(2)) {
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
        for l in util::wrap(&p.name, w) {
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
        for l in util::wrap(&p.brief, w) {
            out.push(line(Style::Muted, l));
        }
    }

    // A project's prose, same as a task's — it is the same machinery now, and
    // a project is the natural home for "what is this and why".
    //
    // The vocabulary rather than a literal copy of it: this was the fourth copy
    // of the task list, and it is the one that would have gone stale silently
    // when `Handbook` landed — a section written through the CLI and simply
    // absent from the pane, with nothing failing.
    for name in crate::model::PROJECT_PROSE {
        if let Some(text) = p.section(name) {
            out.push(Line::default());
            out.push(heading(&name.to_lowercase()));
            for l in util::wrap(&text, w) {
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
            // The same marks the tree draws, in the same order they sorted the
            // list — a row that leads for a reason has to carry it here too,
            // or this pane and the panel beside it disagree about why. As a
            // column, not a flag: this pane is wide and the list is read down.
            let prio = t.priority();
            let ink = match prio {
                Priority::High => Style::Warn,
                Priority::Low => Style::Dim,
                Priority::Normal => Style::Plain,
            };
            l.push(ink, format!("{} ", prio.mark()));
            l.push(Style::Plain, util::truncate(&t.title, w.saturating_sub(4)));
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


#[cfg(test)]
mod tests {
    use super::*;

    /// The spans that carry a section name, paired with whether they are lit.
    /// Style is the whole message here — an accented name is a column you have,
    /// a muted one is a key away — so a test that only read the text would pass
    /// on a line that told the user the opposite of the truth.
    fn named(l: &Line) -> Vec<(String, bool)> {
        l.spans
            .iter()
            .filter(|s| crate::model::PROSE.iter().any(|p| p.eq_ignore_ascii_case(s.text.trim())))
            .map(|s| (s.text.trim().to_string(), matches!(s.style, Style::Accent)))
            .collect()
    }

    #[test]
    fn the_menu_numbers_the_columns_it_has_and_dims_the_rest() {
        let cols = vec!["overview".to_string(), "details".to_string()];
        let l = menu_line(&cols, 120);
        assert_eq!(
            named(&l),
            [
                ("overview".to_string(), true),
                ("details".to_string(), true),
                ("decisions".to_string(), false),
            ]
        );
        let text: String = l.spans.iter().map(|s| s.text.as_str()).collect();
        assert!(text.starts_with("1 overview"), "columns are numbered: {text}");
        assert!(text.contains("2 details"), "…in order: {text}");
        assert!(text.contains("D decisions"), "and the absent one shows its key: {text}");
    }

    /// With every section on screen there is nothing left to offer, and the
    /// line must not invent a fourth entry or leave a trailing separator
    /// pointing at nothing.
    #[test]
    fn three_columns_leaves_nothing_on_the_right() {
        let cols: Vec<String> =
            crate::model::PROSE.iter().map(|s| s.to_lowercase()).collect();
        let l = menu_line(&cols, 120);
        assert!(named(&l).iter().all(|(_, lit)| *lit), "all three are columns");
        let text: String = l.spans.iter().map(|s| s.text.as_str()).collect();
        assert!(!text.contains("·"), "no separator with nothing after it: {text}");
    }

    /// A column order the user made with `d 1` has to survive into the menu, or
    /// the numbers name the wrong panes and `d 2` moves something else.
    #[test]
    fn the_menu_follows_the_column_order_rather_than_the_canonical_one() {
        let cols = vec!["decisions".to_string(), "overview".to_string()];
        let text: String =
            menu_line(&cols, 120).spans.iter().map(|s| s.text.as_str()).collect();
        assert!(text.starts_with("1 decisions"), "{text}");
        assert!(text.contains("2 overview"), "{text}");
        assert!(text.contains("d details"), "details is the one off screen: {text}");
    }

    /// A project's prose is drawn from the project vocabulary, so a section
    /// added there appears here without anybody remembering to come and add it.
    ///
    /// This frame carried a fourth literal copy of the *task* section list, and
    /// it is the copy that would have gone stale silently: a `## Handbook`
    /// written through the CLI, present in the file, absent from the pane, with
    /// nothing failing and nobody told.
    #[test]
    fn a_project_draws_every_section_the_project_vocabulary_names() {
        let mut p = crate::model::Project::new("wsp");
        p.body = "## Overview\nthe control plane\n\n\
                  ## Handbook\nthe map of the code is architecture.md\n\n\
                  ## Decisions\n- 2026-08-16 the store is the only writer\n"
            .into();
        let ctx = Ctx {
            tasks: Vec::new(),
            index: Index::new(vec![p]),
            claims: Default::default(),
            worked: Default::default(),
            bindings: Default::default(),
            panes: Vec::new(),
            columns: Vec::new(),
        };
        let mut out = Vec::new();
        project_frame(&ctx, "wsp", 100, &mut out);
        let text: String = out
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>() + "\n")
            .collect();

        for needle in ["the control plane", "architecture.md", "the store is the only writer"] {
            assert!(text.contains(needle), "missing {needle} from:\n{text}");
        }
        assert!(text.contains("handbook"), "and it is labelled:\n{text}");
    }

    /// A narrow pane must not wrap the menu into the frame above it: `fit`
    /// pads and truncates, and the footer is one line by construction.
    #[test]
    fn the_menu_fits_a_narrow_pane() {
        let cols: Vec<String> =
            crate::model::PROSE.iter().map(|s| s.to_lowercase()).collect();
        for w in [20usize, 40, 200] {
            assert_eq!(menu_line(&cols, w).width(), w, "menu is exactly {w} wide");
        }
    }
}
