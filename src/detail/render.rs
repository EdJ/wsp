//! What a task or a project looks like in full.
//!
//! Takes its data in as a [`Ctx`] rather than reaching for the store, so a
//! fixture can render a frame with nothing running — the same decision the
//! panel's `Snapshot` makes, for the same reason.

use crate::live::{self, AgentRef};
use crate::message::{self, Message};
use crate::model::{Priority, Status, Task};
use crate::panel::{self, line, Line, Style};
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util;

use super::{Focus, Placed};

const LABEL_W: usize = 9;

/// The widest prose is laid out, however much room the frame is given.
///
/// A field is a label and a short value and reads at any width; a paragraph
/// does not. Wrapped to the whole of a ninety-six column page the eye loses the
/// start of the next line, which is why books, newspapers and this repository's
/// own source all stop well short of the page they are printed on.
///
/// It exists because of [`Placed::Page`]. In a pane this rarely binds — a
/// detail pane is a split of something else and is usually narrower than this —
/// but the page is as wide as the panel could get, and "fill the rect" is the
/// one thing prose must not do with the room.
const MEASURE: usize = 80;

/// How wide to lay prose out in a frame of `w` columns. See [`MEASURE`].
fn measure(w: usize) -> usize {
    w.min(MEASURE)
}

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
        Status::Parked => Style::Dim,
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
    /// Every message on this machine, so a task can be asked what is raised
    /// about it.
    ///
    /// `worklist-018`: the card cuts a long paragraph at five hundred
    /// characters and says *"… 6 more lines · o"*, and `o` opened a page that
    /// drew the task and nothing about the hand — so the rest of the message
    /// was not anywhere. A surface that says where the rest is has to be right
    /// about it, or it is the flag receipt's fault one level down: a near side
    /// reporting on a far side it has not looked at.
    pub messages: Vec<Message>,
    /// The sections in the editor columns beside this pane, left to right.
    ///
    /// Cannot be derived from `panes`: a pane row carries no geometry, so which
    /// label is the second column across takes a separate call. Filled once per
    /// repaint by `live`, and left empty by a storyboard, where there are no
    /// editors and the menu is rightly absent.
    pub columns: Vec<String>,
}

impl Ctx {
    /// What a pane reads, including which editors are beside it.
    pub(super) fn live(store: &Store) -> Ctx {
        Ctx { columns: super::editors::editor_columns(), ..Ctx::read(store, live::panes()) }
    }

    /// What a page reads: the same thing, with the panes handed in and no
    /// editors beside it.
    ///
    /// Empty columns is a fact rather than an economy — a page is the panel
    /// drawing this in its own room, and there is nothing next to it to place a
    /// section in.
    ///
    /// The panes are taken as an argument for a sharper reason. A pane asks
    /// herdr for them over a socket; a *surface* is told what herdr is running
    /// and has the answer already, which is the clock `fork-001` removed and
    /// measured — 6.9% of a core down to 2.3%. A page repainting four times a
    /// second must not put it back, so it is given the census the loop already
    /// holds rather than asking for its own.
    pub(crate) fn page(store: &Store, panes: Vec<AgentRef>) -> Ctx {
        Ctx::read(store, panes)
    }

    fn read(store: &Store, panes: Vec<AgentRef>) -> Ctx {
        Ctx {
            tasks: store.tasks(),
            index: Index::new(store.projects()),
            claims: store.claims(),
            worked: store.worked(),
            bindings: store.bindings(),
            panes,
            messages: message::all(store),
            columns: Vec::new(),
        }
    }

    /// The pane working this task, if the live join says so.
    fn pane_for(&self, task: &str) -> Option<&AgentRef> {
        let pane_id = self.bindings.iter().find_map(|(pane, b)| {
            (b.get("task_id").and_then(|t| t.as_str()) == Some(task)).then(|| pane.clone())
        })?;
        self.panes.iter().find(|p| p.pane == pane_id)
    }

    /// The hands up about one task, oldest first.
    ///
    /// [`Message::needs_attention`] and not `is_open`, because a record whose
    /// state this build cannot parse is a record to put in front of somebody
    /// rather than one to go quiet about — the installed binary is routinely
    /// not the tree, and *"I cannot read this"* is a reason to fetch a human.
    ///
    /// Answers are left out because they are already here: `message::answer`
    /// writes the reply to the asker's task log before it delivers it, so a
    /// section drawing them too would put the same sentence on the page twice
    /// and in two voices.
    fn raised_for(&self, task: &str) -> Vec<&Message> {
        self.messages
            .iter()
            .filter(|m| m.about.task() == Some(task))
            .filter(|m| m.reply_to.is_none() && m.needs_attention())
            .collect()
    }
}

/// How far down a frame one press of the wheel or `space` moves it.
///
/// Three for the wheel is the tree's own [`crate::panel::keys::wheel`] step,
/// so a notch means the same amount of travel whichever of the two the panel
/// is drawing. A screenful keeps a line of overlap, because the line you were
/// reading when you pressed the key is the one that tells you where you are.
pub(crate) const WHEEL_STEP: usize = 3;

/// A task or a project, drawn into `h` rows starting `off` lines down.
///
/// # The offset is a parameter, and it is clamped here
///
/// This frame builds the whole write-up and then cuts it to the room it was
/// given, so by the time it returns the overflow is gone — which is why the
/// offset cannot live anywhere but in this call. It is `&mut` for the reason
/// [`crate::panel::render::frame`] takes a `&mut View`: the length of the
/// write-up is not known until it has been built, so the *frame* is the only
/// place that can say how far down it is possible to be, and the caller keeps
/// what it decides.
///
/// That write-back is not bookkeeping. A key handler here adds three and lets
/// this clamp, so a reader who holds `j` at the foot has no overshoot to walk
/// off before `k` moves — the bug the panel's own wheel documents having had.
/// `usize::MAX` is how `G` says "the end" without knowing where the end is.
pub(crate) fn frame(
    ctx: &Ctx,
    focus: &Focus,
    w: usize,
    h: usize,
    placed: Placed,
    off: &mut usize,
) -> Vec<Line> {
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
    let cols = match placed {
        // A page has no editors beside it whatever the context says, and the
        // menu is the half of this footer that is only about them.
        Placed::Page => &[][..],
        Placed::Pane => &ctx.columns[..],
    };
    let footer = if cols.is_empty() { 2 } else { 3 };
    let body = h.saturating_sub(footer);
    let total = out.len();
    // Clamp against what was actually built and hand it back. Everything below
    // reads the clamped value, so the rule and the hint describe the frame in
    // front of the reader rather than what a key asked for.
    *off = (*off).min(total.saturating_sub(body));
    out.drain(..*off);
    out.truncate(body);
    while out.len() < body {
        out.push(Line::default());
    }
    let scrolls = total > body;
    let shown = (*off + body).min(total);
    out.push(rule(w, scrolls.then_some((shown, total))));
    if !cols.is_empty() {
        out.push(menu_line(cols, w));
    }
    out.push(line(
        Style::Dim,
        util::truncate(
            match (placed, cols.is_empty()) {
                // The panel's own keys, because on a page the panel is what is
                // holding them. Nothing here acts on another pane, so nothing
                // here needs naming for being surprising — what needs naming is
                // the way out, and that `E` still puts you in your own editor.
                //
                // The scroll keys are named only when there is something to
                // scroll, and only on a page. A page has the room and is the
                // view whose whole purpose is reading; the pane's two hints are
                // already at the width of a split, and pushing `q discard` off
                // the end to advertise `space` would cost more than it bought.
                // The rule above says there is more in both.
                (Placed::Page, _) if scrolls => {
                    "space or j/k scrolls · ↵ or q closes it · E edits it"
                }
                (Placed::Page, _) => "↵ or q closes it · E edits it",
                (Placed::Pane, true) => {
                    "h/l left or right · W save and close · q close, discarding"
                }
                (Placed::Pane, false) => {
                    "o/d/D then 1-3 places a section · 1/2/3 alone sets the columns · W save · q discard"
                }
            },
            w,
        ),
    ));
    out
}

/// The rule above the footer, carrying how far down the write-up you are.
///
/// A frame that silently drops its tail is the whole of what this task was
/// about, so scrolling on its own is half a fix: a reader who cannot see that
/// there is more has no reason to press the key. The count goes on the rule
/// rather than in the hint because the rule is the one line in this frame with
/// nothing on it, and in a pane the hint has no room to spare.
///
/// `at` is the line drawn on the bottom row and the length of the write-up, and
/// it is `None` when the whole thing fits. A marker that is always there says
/// nothing — the question it answers only exists when the answer can be yes —
/// and every frame in the storyboard that fits stays byte-identical.
///
/// It keeps saying `62/62` once you reach the foot, which is not redundant: it
/// is how a reader at the end can still tell a long write-up they have read to
/// the bottom of from a short one that never scrolled at all. What it reports
/// is that this scrolls, and where in it you are.
fn rule(w: usize, at: Option<(usize, usize)>) -> Line {
    let mut l = Line::default();
    // Two dashes of tail so it reads as set into the rule rather than as the
    // rule having run out. Below that width there is no room to set anything
    // into, and the plain rule is the honest drawing.
    let tail = 2;
    let mark = match at {
        Some((shown, total)) => format!(" {shown}/{total} "),
        None => String::new(),
    };
    if mark.is_empty() || w < mark.chars().count() + tail + tail {
        l.push(Style::Dim, "─".repeat(w));
        return l;
    }
    l.push(Style::Dim, "─".repeat(w - mark.chars().count() - tail));
    l.push(Style::Muted, mark);
    l.push(Style::Dim, "─".repeat(tail));
    l
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

/// One raised hand's first line: the mark, and who is asking for what.
///
/// The headline and not a heading taken from somewhere else. `worklist-018` is
/// the whole reason: a flag carried a heading, a sentence and a paragraph in
/// three fields, and every surface picked a different one, so a hand raised
/// with only the third drew as an empty row. A message is one text whose first
/// line is the headline ([`Message::title`]), and there is no second field here
/// to prefer over it.
fn hand(m: &Message, w: usize) -> Line {
    let mut l = Line::default();
    l.push(Style::Warn, format!("{} ", panel::glyph::FLAG));
    // Empty is not a fault and is not an error either — `wsp flag <id>` on its
    // own means *look at this task, it exists*, and the words for that are the
    // sender's absence rather than anything to invent.
    let head = match m.title() {
        "" => "raised this, with nothing written on it".to_string(),
        t => t.to_string(),
    };
    l.push(Style::Bold, util::truncate(&head, w.saturating_sub(2)));
    l
}

/// The line under a hand: who is asking, how long they have been, and the verb
/// that ends it.
///
/// The verb is named because this page is where a person reads the whole of a
/// question, and a question read and not answered is `worklist-013` — a hand
/// that stayed up for 2h14m after the answer had been given down another
/// channel. Naming `wsp answer <id>` beside the words is the cheapest place to
/// put the two together.
fn answering(m: &Message) -> String {
    let mut parts = vec![m.from.byline()];
    if let Some(crate::message::Ask::Claim) = m.ask() {
        parts.push("asks to take it".into());
    }
    let held = util::since(&m.at);
    if held > 0 {
        parts.push(util::duration_human(held));
    }
    parts.push(match m.shape() {
        Some(crate::message::Shape::Question) => format!("wsp answer {} \"…\"", m.id),
        _ => format!("wsp ack {}", m.id),
    });
    parts.join(" · ")
}

fn task_frame(ctx: &Ctx, id: &str, w: usize, out: &mut Vec<Line>) {
    let Some(t) = ctx.tasks.iter().find(|t| t.id == id) else {
        out.push(line(Style::Warn, format!("no task {id}")));
        return;
    };
    // Decisions and the log are drawn out of the body as written, so the stamps
    // in it are turned into this reader's dates once, here, rather than at each
    // of the two places below that would otherwise each have to remember.
    let t = &Task { body: crate::model::localise_dates(&t.body), ..t.clone() };

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

    // The hands up about this task, before anything that is merely true about
    // it. This is the page a card sends you to when it has cut a paragraph —
    // `o` on a card is `Focus::Task` — so the words it cut have to be here, in
    // full, and above the fold rather than under the prose.
    let raised = ctx.raised_for(&t.id);
    if !raised.is_empty() {
        out.push(Line::default());
        out.push(heading(&format!("raised · {}", raised.len())));
        for m in raised {
            out.push(hand(m, measure(w)));
            for l in util::wrap(m.text.trim(), measure(w).saturating_sub(2)) {
                let mut ln = Line::default();
                ln.push(Style::Dim, "  ");
                ln.push(Style::Plain, l);
                out.push(ln);
            }
            out.push(line(Style::Muted, format!("  {}", answering(m))));
        }
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
            for l in util::wrap(&text, measure(w)) {
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
            for l in util::wrap(text, measure(w).saturating_sub(2)) {
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
    let Some(mut p) = ctx.index.get(id).cloned() else {
        out.push(line(Style::Warn, format!("no project {id}")));
        return;
    };
    // Same reason as `task_frame`: `## Decisions` is drawn from the body as
    // written, and a project's is the one people read most.
    p.body = crate::model::localise_dates(&p.body);

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
        for l in util::wrap(&p.brief, measure(w)) {
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
            for l in util::wrap(&text, measure(w)) {
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
    // Dim, and after the blocked count: it is part of the open number above
    // and the one part of it nobody has to do anything about.
    if c.parked > 0 {
        totals.push(Style::Dim, format!(" · {} parked", c.parked));
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
        Status::Parked => g::PARKED,
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

    fn task_ctx(t: crate::model::Task) -> Ctx {
        Ctx {
            tasks: vec![t],
            index: Index::new(vec![crate::model::Project::new("wsp")]),
            claims: Default::default(),
            worked: Default::default(),
            bindings: Default::default(),
            panes: Vec::new(),
            messages: Vec::new(),
            columns: Vec::new(),
        }
    }

    /// A hand up about `task`, carrying `text`.
    fn raised(task: &str, text: &str) -> Message {
        crate::message::Message::question(
            crate::message::Party::pane("w4:p2", "w4"),
            crate::message::Kind::Note,
            text,
            crate::message::Waiting::new("w4:p2", task),
        )
        .about(crate::message::About::Task(task.to_string()))
    }

    /// `worklist-018`. The card cuts a paragraph at five hundred characters and
    /// says *"… 6 more lines · o"*; `o` is `Focus::Task` and lands here. Before
    /// this, the page it landed on drew the task and said nothing about the
    /// hand at all — so a card that told you where the rest was was wrong, and
    /// the only way to read a long raised hand was `flags.json` by hand, which
    /// is exactly how the fault this task exists for was found.
    #[test]
    fn the_page_a_card_sends_you_to_has_the_words_the_card_cut() {
        let mut t = crate::model::Task::new("the panel work, in order", "wsp-013");
        t.project = Some("wsp".into());
        let long = format!("this is next — can I take it?\n\n{}\n\nand the last line of it.", "x".repeat(900));
        let ctx = Ctx { messages: vec![raised("wsp-013", &long)], ..task_ctx(t) };

        let page = text_of(&frame(&ctx, &Focus::Task("wsp-013".into()), 96, 60, Placed::Page, &mut 0));
        assert!(page.contains("raised · 1"), "the section is there:\n{page}");
        assert!(page.contains("this is next — can I take it?"), "the headline:\n{page}");
        assert!(
            page.contains("and the last line of it."),
            "and the end of a paragraph no card could hold — this is the whole point:\n{page}",
        );
        assert!(page.contains("wsp answer "), "with the verb that ends it:\n{page}");
    }

    /// `wsp flag <id>` on its own is a complete thing to say — *look at this
    /// task, it exists* — and it is the one input that leaves the record with
    /// no words in it. It draws as a sentence rather than as an empty row,
    /// because an empty row and a hand about a task that has been retired are
    /// the two things a reader must never have to tell apart by guessing.
    #[test]
    fn a_hand_with_nothing_written_on_it_says_that_rather_than_drawing_blank() {
        let mut t = crate::model::Task::new("the panel work, in order", "wsp-013");
        t.project = Some("wsp".into());
        let ctx = Ctx { messages: vec![raised("wsp-013", "")], ..task_ctx(t) };

        let page = text_of(&frame(&ctx, &Focus::Task("wsp-013".into()), 96, 60, Placed::Page, &mut 0));
        assert!(page.contains("nothing written on it"), "said out loud:\n{page}");
    }

    /// An answer is on the page already — `message::answer` writes it to the
    /// asker's task log before it delivers it — so drawing replies here too
    /// would put one sentence on one page twice, in two voices, and leave a
    /// reader deciding which of them is the record.
    #[test]
    fn an_answer_is_not_a_second_raised_hand() {
        let mut t = crate::model::Task::new("the panel work, in order", "wsp-013");
        t.project = Some("wsp".into());
        let mut reply = raised("wsp-013", "yes, take it");
        reply.reply_to = Some("m-1".to_string());
        let ctx = Ctx { messages: vec![reply], ..task_ctx(t) };

        let page = text_of(&frame(&ctx, &Focus::Task("wsp-013".into()), 96, 60, Placed::Page, &mut 0));
        assert!(!page.contains("raised · "), "an answer raises nothing:\n{page}");
    }

    fn text_of(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>() + "\n")
            .collect()
    }

    /// The one thing prose must not do with the room a page was given.
    ///
    /// `↵` widens the panel to ninety-six columns, and everything else in this
    /// frame is happy there — a field is a label and a short value, a sub-task
    /// row is a status and a title. A paragraph is not: at ninety-six the eye
    /// loses the start of the next line, which is why [`MEASURE`] exists and
    /// why this checks the drawn frame rather than the constant.
    ///
    /// It would have been very easy not to notice. Nothing looks broken — the
    /// text is all there, correctly wrapped, to the wrong width.
    #[test]
    fn prose_is_laid_out_to_a_measure_rather_than_to_the_width_it_was_given() {
        let mut t = crate::model::Task::new("a title", "wsp-013");
        t.project = Some("wsp".into());
        t.body = format!("## Overview\n{}\n", "one two three four five six ".repeat(40));
        let ctx = task_ctx(t);

        // The prose lines and nothing else: a rule is drawn to the rect on
        // purpose, and it is the paragraph this is about.
        let prose = |w: usize| -> usize {
            frame(&ctx, &Focus::Task("wsp-013".into()), w, 90, Placed::Page, &mut 0)
                .iter()
                .filter(|l| l.text().contains("one two three"))
                .map(|l| l.width())
                .max()
                .unwrap_or(0)
        };

        let widest = prose(200);
        assert!(widest > 0, "the overview was not drawn at all");
        assert!(
            widest <= MEASURE,
            "prose ran to {widest} columns; a page is wide and the prose in it is not",
        );

        // …and a frame narrower than the measure still fills what it has, or
        // this would be a paragraph in a column down the left of every pane.
        assert!(prose(40) > 30, "a narrow frame should still use its width");
    }

    /// A page offers the panel's keys, because on a page the panel is what is
    /// holding them.
    ///
    /// The pane's footer names `W save and close` and a section menu, and both
    /// act on **editor panes beside this one**. A page has none and cannot open
    /// one, so advertising them would be offering keys that answer nothing —
    /// and the menu is drawn from a context the page shares with the pane, so
    /// this is checked with columns present rather than absent.
    #[test]
    fn a_page_offers_the_way_out_and_never_the_editors_it_does_not_have() {
        let mut t = crate::model::Task::new("a title", "wsp-013");
        t.project = Some("wsp".into());
        let ctx = Ctx {
            columns: vec!["overview".to_string(), "details".to_string()],
            ..task_ctx(t)
        };
        let focus = Focus::Task("wsp-013".into());

        let page = text_of(&frame(&ctx, &focus, 96, 40, Placed::Page, &mut 0));
        assert!(page.contains("↵ or q closes it"), "the way out is named:\n{page}");
        assert!(page.contains("E edits it"), "and so is the editor:\n{page}");
        assert!(!page.contains("W save"), "there is nothing to save:\n{page}");
        assert!(!page.contains("1 overview"), "and no columns to place:\n{page}");

        // The same context in a pane still gets the menu, so the line above is
        // about where it is drawn and not about the columns having gone.
        let pane = text_of(&frame(&ctx, &focus, 96, 40, Placed::Pane, &mut 0));
        assert!(pane.contains("1 overview"), "a pane keeps its menu:\n{pane}");
        assert!(pane.contains("W save"), "and its keys:\n{pane}");
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
            messages: Vec::new(),
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

    /// The pane draws decisions and the log straight out of the body, so it is
    /// the frame most likely to put a stored stamp in front of somebody. Both
    /// blocks go through the same conversion, and a line written before the
    /// hour was stored still draws.
    #[test]
    fn the_pane_draws_dates_and_never_the_stamp_underneath_them() {
        let mut t = crate::model::Task::new("dates read as local", "wsp-013");
        t.body = "## Decisions\n- 2026-08-16T23:15:00Z libc gives us the offset\n\n\
                  ## Log\n- 2026-08-14 claimed by pane w2:p1\n\
                  - 2026-08-16T23:15:00Z blocked: which offset source\n"
            .into();
        let ctx = Ctx {
            tasks: vec![t],
            index: Index::new(Vec::new()),
            claims: Default::default(),
            worked: Default::default(),
            bindings: Default::default(),
            panes: Vec::new(),
            messages: Vec::new(),
            columns: Vec::new(),
        };
        let mut out = Vec::new();
        task_frame(&ctx, "wsp-013", 100, &mut out);
        let text: String = out
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>() + "\n")
            .collect();

        assert!(!text.contains("23:15:00Z"), "a stored stamp reached the pane:\n{text}");
        assert!(text.contains("2026-08-14 claimed"), "an old line stands as it is:\n{text}");
        let day = util::local_ymd("2026-08-16T23:15:00Z");
        assert!(text.contains(&format!("{day} libc gives us the offset")), "decision:\n{text}");
        assert!(text.contains(&format!("{day} blocked:")), "log:\n{text}");
    }

    /// A task with `n` log lines, each one findable on its own.
    ///
    /// The log because that is what actually overflowed: it is drawn last and
    /// newest-first, so the half a truncated frame loses is the most recent
    /// thing anybody wrote about the task. A fixture that overflowed on the
    /// overview would be testing the arithmetic and not the complaint.
    fn long_log(n: usize) -> Ctx {
        let mut t = crate::model::Task::new("a long-running task", "wsp-013");
        t.project = Some("wsp".into());
        let log: String =
            (0..n).map(|i| format!("- 2026-08-19 line {i}\n")).collect();
        t.body = format!("## Log\n{log}");
        task_ctx(t)
    }

    /// The whole of what this task was about: the last line of a long write-up
    /// is reachable, where before it was simply not there.
    ///
    /// Asserted on the *first* log line, which is the one drawn last — the log
    /// reads newest-first, so the end of the frame is the oldest entry, and a
    /// frame that can show it has shown everything between.
    #[test]
    fn a_write_up_taller_than_the_room_can_be_read_to_its_last_line() {
        let ctx = long_log(60);
        let focus = Focus::Task("wsp-013".into());
        let (w, h) = (96, 20);

        let mut off = 0;
        let top = text_of(&frame(&ctx, &focus, w, h, Placed::Page, &mut off));
        assert!(top.contains("line 59"), "the newest entry is at the top:\n{top}");
        assert!(!top.contains("line 0\n"), "and the oldest is off the bottom");

        // `G`, as the page sends it: past the end, and the frame is what knows
        // where the end is.
        off = usize::MAX;
        let foot = text_of(&frame(&ctx, &focus, w, h, Placed::Page, &mut off));
        assert!(foot.contains("line 0\n"), "the last line is reachable:\n{foot}");
        assert!(off < usize::MAX, "and the offset came back clamped");
    }

    /// The clamp is handed back, and that write-back is the point of the `&mut`.
    ///
    /// Without it a reader who held `j` at the foot would build an offset of
    /// four hundred against a write-up of sixty, and `k` would move nothing at
    /// all until the overshoot had been walked off one line at a time — which
    /// is the bug `panel::keys::wheel` documents having had, arriving here by
    /// the same route.
    #[test]
    fn an_offset_past_the_end_comes_back_at_the_end_rather_than_where_it_was_asked() {
        let ctx = long_log(60);
        let focus = Focus::Task("wsp-013".into());

        let mut off = usize::MAX;
        let at_end = text_of(&frame(&ctx, &focus, 96, 20, Placed::Page, &mut off));
        let settled = off;

        // One line back from the far end moves, immediately.
        off = settled - 1;
        let one_up = text_of(&frame(&ctx, &focus, 96, 20, Placed::Page, &mut off));
        assert_ne!(one_up, at_end, "a line back from the foot is a different frame");

        // And asking for the end again lands in the same place, not further on.
        off = usize::MAX;
        let _ = frame(&ctx, &focus, 96, 20, Placed::Page, &mut off);
        assert_eq!(off, settled, "the end is one place");
    }

    /// A frame is exactly the size it was given, however far down it is.
    ///
    /// The offset moves where the cut is taken; it must not change that there
    /// is one. A frame short of its rows leaves whatever the pane drew last
    /// showing underneath it, and one over its rows pushes its own footer off.
    #[test]
    fn a_frame_is_its_full_height_at_every_offset() {
        let ctx = long_log(60);
        let focus = Focus::Task("wsp-013".into());
        for h in [6usize, 20, 200] {
            for want in [0usize, 7, usize::MAX] {
                let mut off = want;
                let n = frame(&ctx, &focus, 96, h, Placed::Page, &mut off).len();
                assert_eq!(n, h, "h={h} off={want}");
            }
        }
    }

    /// Scrolling silently is half a fix: a reader with no sign that there is
    /// more has no reason to press the key.
    ///
    /// Which is also why the sign is absent when everything fits — the rule
    /// answers "is there more", and that question does not exist when the
    /// answer cannot be yes.
    #[test]
    fn the_rule_says_how_far_down_it_is_only_when_there_is_further_to_go() {
        let focus = Focus::Task("wsp-013".into());
        // The rule is the frame's second-from-last line on a page, above the
        // one-line hint.
        let rule_of = |ctx: &Ctx, h: usize, mut off: usize| -> String {
            let f = frame(ctx, &focus, 96, h, Placed::Page, &mut off);
            f[f.len() - 2].text()
        };

        let short = long_log(2);
        assert_eq!(
            rule_of(&short, 40, 0).trim_matches('─'),
            "",
            "a write-up that fits gets the plain rule it always had",
        );

        let long = long_log(60);
        let top = rule_of(&long, 20, 0);
        assert!(top.contains('/'), "somewhere to go, and the rule says so: {top}");
        // And it goes on saying so at the foot, where the two halves agree —
        // which is how "read to the bottom of a long one" is told apart from
        // "a short one that never scrolled".
        let foot = rule_of(&long, 20, usize::MAX);
        let mark = foot.trim_matches('─').trim().to_string();
        let (shown, total) = mark.split_once('/').unwrap_or_default();
        assert_eq!(shown, total, "at the foot the two halves agree: {foot}");
        assert!(!total.is_empty(), "and there is a marker at all: {foot}");

        // Whatever it says, it is still a rule of exactly the width it was
        // given — the marker is set into it, not appended to it.
        for (h, off) in [(20usize, 0usize), (20, usize::MAX), (40, 0)] {
            let mut o = off;
            let f = frame(&long, &focus, 96, h, Placed::Page, &mut o);
            assert_eq!(f[f.len() - 2].width(), 96, "h={h} off={off}");
        }
    }

    /// The keys are named where there is room to name them and where they are
    /// worth naming: on a page, and only when there is something to scroll.
    ///
    /// The pane's hints are already at the width of a split. Pushing
    /// `q discard` off the end to advertise `space` would cost a key that
    /// throws work away to advertise one that moves the view — see [`frame`],
    /// where the rule carries the signal that both placements get.
    #[test]
    fn a_page_that_scrolls_names_the_keys_and_a_pane_keeps_its_own() {
        let focus = Focus::Task("wsp-013".into());
        let hint = |ctx: &Ctx, placed: Placed| -> String {
            let mut off = 0;
            frame(ctx, &focus, 96, 20, placed, &mut off).last().expect("a footer").text()
        };

        let long = long_log(60);
        let page = hint(&long, Placed::Page);
        assert!(page.contains("space or j/k"), "{page}");
        assert!(page.contains("q closes it"), "and the way out is still there: {page}");

        assert!(!hint(&long_log(2), Placed::Page).contains("j/k"), "nothing to scroll");
        assert!(hint(&long, Placed::Pane).contains("q close, discarding"), "the pane's own");
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
