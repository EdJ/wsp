//! The surface a row is drawn onto.
//!
//! [`Line`] and [`Style`] are the whole vocabulary: styled spans, measured in
//! columns rather than bytes. A frame is a `Vec<Line>` and nothing above this
//! knows what it will be painted with — [`to_ansi`] puts it on a terminal and
//! [`to_html`] puts the same cells in the storyboard, which is why the two
//! cannot drift apart on colour.

use std::time::Duration;

use crate::util;

use super::keys::{keymap, Mode, View};
use super::rows::{hotkeys, render_row, Ui};

pub(super) const ACCENT: &str = "\x1b[38;2;95;191;164m";

pub(super) const WARN: &str = "\x1b[38;2;224;138;75m";

pub(super) const MUTED: &str = "\x1b[38;2;125;140;150m";

/// A question somebody has written down. Its own hue rather than a second use
/// of [`WARN`]: an agent stopped in front of you and an agent parked behind a
/// question are two different calls on your time, and one colour for both made
/// the pair unreadable in the tree, where they sit a row apart.
pub(super) const QUERY: &str = "\x1b[38;2;176;140;217m";

pub(super) const DIMC: &str = "\x1b[2m";

pub(super) const BOLD: &str = "\x1b[1m";

pub(crate) const INV: &str = "\x1b[7m";

pub(crate) const OFF: &str = "\x1b[0m";

/// How a run of text is drawn. Kept beside the text rather than baked into it:
/// lines used to be built with the escapes already embedded, which is why
/// measuring one meant scanning back over them to discount the escapes. With
/// style held separately, width is a plain char count and the same frame can
/// go to a terminal or to a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Dim,
    Bold,
    Muted,
    Accent,
    Warn,
    /// A question that is written down and waiting on an answer.
    Query,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Debug, Clone, Default)]
pub struct Line {
    pub spans: Vec<Span>,
    /// Drawn inverse — the selected row.
    pub selected: bool,
}

impl Line {
    pub(crate) fn push(&mut self, style: Style, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.spans.push(Span { text, style });
        }
    }

    pub(crate) fn width(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum()
    }

    pub(crate) fn pad(&mut self, n: usize) {
        self.push(Style::Plain, " ".repeat(n));
    }

    /// Pad or clip to exactly `w` columns.
    pub(crate) fn fit(&mut self, w: usize) {
        let have = self.width();
        if have < w {
            self.pad(w - have);
            return;
        }
        if have == w {
            return;
        }
        let mut left = w;
        let mut out: Vec<Span> = Vec::new();
        for s in self.spans.drain(..) {
            let n = s.text.chars().count();
            if n <= left {
                left -= n;
                out.push(s);
            } else {
                out.push(Span { text: s.text.chars().take(left).collect(), style: s.style });
                break;
            }
        }
        self.spans = out;
    }

    /// Style stripped — what the row says. For assertions, once the storyboard
    /// grows checks as well as frames.
    #[allow(dead_code)]
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

pub(crate) fn line(style: Style, text: impl Into<String>) -> Line {
    let mut l = Line::default();
    l.push(style, text);
    l
}

/// The panel's whole glyph vocabulary, named once so the legend cannot drift
/// from what the rows actually draw.
pub(crate) mod glyph {
    pub const OPEN: &str = "▾";
    pub const CLOSED: &str = "▸";
    pub const WORKING: &str = "●";
    pub const IDLE: &str = "○";
    pub const QUIET: &str = "·";
    pub const BLOCKED: &str = "■";
    /// An agent parked behind a question. The task's own `■` says the work is
    /// stopped; this says there is a sentence on it addressed to you, which is
    /// the half a square never managed to say.
    pub const QUESTION: &str = "?";
    pub const REVIEW: &str = "◆";
    pub const DONE: &str = "✓";
    pub const DOING: &str = "▸";
    pub const MORE: &str = "⋯";
    pub const NEEDS_YOU: &str = "←";
    /// A hand raised by an agent: look at this task. Drawn on the task's own
    /// row wherever it is in the tree, and again in the section pinned at the
    /// foot — the row is the deeplink, so it has to be findable in both.
    pub const FLAG: &str = "▲";
    /// A pane with no agent in it.
    pub const SHELL: &str = "▫";
    /// The custodian's slot on a project, filled or empty — `wsp govern`, `wsp
    /// wip` and the panel all draw the position with this one mark, so a seat
    /// looks like a seat wherever you meet it.
    pub const SEAT: &str = "▣";
    /// Something is written in Overview or Details.
    pub const NOTES: &str = "≡";
    /// In the tag picker: carried already, about to be added, about to be
    /// taken off. Three marks rather than two, because nothing is written
    /// until `↵` and the frame has to say what `↵` will do.
    pub const TAG_ON: &str = "✓";
    pub const TAG_NEW: &str = "+";
    pub const TAG_OFF: &str = "−";
    /// Ahead of its project's other work, and behind it. Taken from the model
    /// so the panel and the CLI cannot end up marking the same task two ways.
    /// `normal` is absent on purpose: the tree draws it as no column at all,
    /// where a list draws it as an empty one.
    pub const HIGH: &str = crate::model::Priority::High.mark();
    pub const LOW: &str = crate::model::Priority::Low.mark();
}

pub(crate) struct Mark {
    /// Drawn through the same Style the rows use, so the colour cannot drift.
    pub sample: Line,
    pub name: &'static str,
    pub note: &'static str,
}

pub(super) fn mark(spans: &[(Style, &str)], name: &'static str, note: &'static str) -> Mark {
    let mut sample = Line::default();
    for (st, t) in spans {
        sample.push(*st, *t);
    }
    Mark { sample, name, note }
}

/// The vocabulary, grouped by where it appears. Built from the same glyph
/// constants and the same `Style` values the renderer uses.
pub(crate) fn legend() -> Vec<(&'static str, &'static str, Vec<Mark>)> {
    use glyph as g;
    vec![
        (
            "On a task",
            "The first column. When an agent is on the task it shows the agent's \
             state — so the task's own status is not drawn, and a claimed task \
             looks the same whether it is todo or doing.",
            vec![
                mark(&[(Style::Accent, g::WORKING)], "working", "an agent is on this task and busy"),
                mark(&[(Style::Muted, g::IDLE)], "idle", "an agent is on this task and waiting"),
                mark(&[(Style::Dim, g::QUIET)], "no agent", "nobody has picked this up"),
                mark(&[(Style::Warn, g::BLOCKED)], "blocked", "parked, with a reason on the task"),
                mark(&[(Style::Muted, g::REVIEW)], "review", "done enough to look at"),
                mark(&[(Style::Accent, g::DOING)], "doing", "started — the task's own state, which it keeps even when claimed"),
                mark(&[(Style::Dim, g::DONE)], "done", "finished — only shown under A"),
            ],
        ),
        (
            "Priority, on a task",
            "Where a task sits among the others in its own project — `!` cycles \
             it. It orders the list and nothing else: two tasks in different \
             projects are never in the same list, so a `!` here never outranks \
             anything over there. Under status, so `!` on work not started \
             does not jump it above work already in hand.",
            vec![
                mark(&[(Style::Warn, g::HIGH)], "high", "do this one first, in this project"),
                mark(&[(Style::Dim, g::LOW)], "low", "sunk to the foot of the project — kept, not next"),
                mark(&[(Style::Muted, "no mark")], "normal", "what almost everything is; drawn as nothing at all"),
            ],
        ),
        (
            "On a project",
            "A fold marker on the left, and on the right the workload rolled up \
             from every project beneath it.",
            vec![
                mark(&[(Style::Dim, g::OPEN)], "unfolded", "tasks and child projects are showing"),
                mark(&[(Style::Dim, g::CLOSED)], "folded", "← hides them, → brings them back"),
                mark(&[(Style::Dim, "7")], "open", "tasks not yet done, including everything below"),
                mark(&[(Style::Accent, g::DOING), (Style::Accent, "3")], "in flight", "tasks someone has started"),
                mark(&[(Style::Warn, g::BLOCKED), (Style::Warn, "1")], "blocked", "tasks parked and waiting"),
                mark(&[(Style::Dim, g::DONE)], "all clear", "there is work here and all of it is finished"),
                mark(&[(Style::Accent, g::WORKING), (Style::Accent, "2")], "panes", "panes standing in this project, agent or not"),
            ],
        ),
        (
            "In the tag picker",
            "`t` on a task docks the tags the store already uses. Nothing is \
             written until `↵`, so the marks have to say what `↵` is going to \
             do. Inherited tags are in the list and drawn as carried, because \
             `wsp show` prints them and the detail pane prints them — leaving \
             them out drew `rust` as absent on a task everything else says is \
             tagged `rust`.",
            vec![
                mark(&[(Style::Plain, g::TAG_ON)], "carried", "on the task now, and staying"),
                mark(&[(Style::Accent, g::TAG_NEW)], "coming", "`␣` put it on; `↵` is what writes it"),
                mark(&[(Style::Warn, g::TAG_OFF)], "going", "on the task now and about to come off"),
                mark(&[(Style::Dim, g::TAG_ON), (Style::Plain, " rust"), (Style::Dim, "  wsp")], "lent by a project", "on the task, and not this task's to remove — `wsp tag` cannot reach it. The name on the right is the project it comes from, which is where it does come off"),
                mark(&[(Style::Muted, g::QUIET)], "not carried", "in the vocabulary, not on this task"),
                mark(&[(Style::Accent, g::TAG_NEW), (Style::Plain, " mix"), (Style::Dim, "  new")], "a tag nobody has used", "what you typed is not in the list, so the last row offers to make it"),
            ],
        ),
        (
            "Everywhere else",
            "Rows and markers that are not about a single task.",
            vec![
                mark(&[(Style::Warn, g::NEEDS_YOU)], "wants you", "an idle agent on a task that is still doing — it has stopped and you are the blocker"),
                mark(&[(Style::Warn, g::NEEDS_YOU), (Style::Accent, g::WORKING), (Style::Muted, g::IDLE)], "the strip", "the same question in the header, once per agent — see below"),
                mark(&[(Style::Query, g::QUESTION)], "a question", "an agent parked on a task with a question written on it — drawn wherever that agent is, tree or strip"),
                mark(&[(Style::Dim, g::MORE), (Style::Muted, " 2 more")], "overflow", "past the six-task cap; ↵ opens the tail in place"),
                mark(&[(Style::Accent, g::WORKING), (Style::Plain, " "), (Style::Accent, "Trance Video")], "a pane", "nested under the task it claimed, or under the project it stands in — the same mark and the same colour the agents view gives it, so one pane does not read two ways in one glance"),
                mark(&[(Style::Dim, g::SHELL), (Style::Plain, " "), (Style::Muted, "Trance Lite")], "a shell", "a pane with no agent — never started, as against an idle one that stopped"),
                mark(&[(Style::Accent, g::SEAT), (Style::Plain, " "), (Style::Accent, "governor · wsp")], "a governor", "the custodial slot on the project above it: one agent, answerable for everything beneath, drawn where it is answerable rather than under whatever task it borrowed. It names the project rather than the work, because it holds no work. T says something to it, ↵ goes there"),
                mark(&[(Style::Dim, g::SEAT), (Style::Plain, " "), (Style::Muted, "governor · verb"), (Style::Plain, "  "), (Style::Muted, "empty")], "a vacancy", "the position outlives whoever was in it — wsp spawn -p <project> --govern fills it again. Drawn only where no slot above it is filled: a vacancy is an invitation, and one per level would bury the governors that exist"),
                mark(&[(Style::Dim, g::NOTES)], "written on", "something is in this row's Overview or Details — E opens it"),
                mark(&[(Style::Warn, g::FLAG)], "raised", "an agent has flagged this task and said why — x lowers it"),
                mark(&[(Style::Dim, g::OPEN), (Style::Plain, " "), (Style::Muted, "inbox")], "a group", "not a project, but still a scope — folds and takes the cursor like one"),
                mark(&[(Style::Dim, g::OPEN), (Style::Plain, " "), (Style::Muted, "agents"), (Style::Plain, " "), (Style::Dim, "7")], "the section", "pinned at the foot: the five agents the strip puts first, under the project each is in, with the count of all of them"),
                mark(&[(Style::Dim, "1")], "hotkey", "1-9 jump straight to that agent's terminal"),
                mark(&[(Style::Accent, "+done")], "showing done", "A is on, so finished work is included"),
            ],
        ),
        (
            "In the header, and among the agents",
            "herdr says working or idle and nothing more. What an idle agent is \
             waiting for comes from the task in its hands, which is the half \
             the store knows — so the same two states from herdr become four \
             different answers here. One mark per agent, what wants you first, \
             what is free next: there is nothing to do about an agent that is \
             working. Each mark is clickable and goes to that terminal.",
            vec![
                mark(&[(Style::Warn, g::NEEDS_YOU)], "wants you", "stopped, holding work that is still live — you are the blocker"),
                mark(&[(Style::Query, g::QUESTION)], "blocked", "stopped, on a task parked with a question written on it — its own colour, because it wants an answer rather than a nudge"),
                mark(&[(Style::Accent, g::WORKING)], "working", "running"),
                mark(&[(Style::Muted, g::IDLE)], "spare", "stopped, holding nothing — a person's worth of attention going spare"),
                mark(&[(Style::Accent, g::SEAT)], "coordinating", "stopped, and in a project's governor slot — a governor is idle between the agents it starts, which is most of the night, and none of that idleness is you being the blocker"),
                mark(&[(Style::Dim, g::QUIET)], "quiet", "herdr says neither, usually an agent that has not spoken since it started"),
                mark(&[(Style::Dim, g::MORE), (Style::Plain, " "), (Style::Dim, "11")], "too many to draw", "the strip is clipped, never the count beside it — click the ⋯ for the rest"),
            ],
        ),
        (
            "Colour on its own",
            "Seven roles, used consistently regardless of glyph.",
            vec![
                mark(&[(Style::Plain, "plain")], "claimed", "a task with an agent on it"),
                mark(&[(Style::Muted, "muted")], "unclaimed", "a task nobody is on; agent names"),
                mark(&[(Style::Dim, "dim")], "structure", "carets, counts, punctuation, finished work"),
                mark(&[(Style::Bold, "bold")], "project", "project names only"),
                mark(&[(Style::Accent, "accent")], "live", "running agents and work in flight"),
                mark(&[(Style::Warn, "warn")], "waiting on you", "an agent stopped in front of you, and work parked without a question"),
                mark(&[(Style::Query, "query")], "a question", "somebody has written one down and is waiting on the answer"),
            ],
        ),
    ]
}

/// The key map as rows. A heading rules off to the edge rather than sitting
/// above a blank line: a sidebar is short, and separation has to cost nothing.
pub(super) fn help_lines(w: usize) -> Vec<Line> {
    let keyw = keymap()
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);

    let mut out = Vec::new();
    for (section, keys) in keymap() {
        let mut head = Line::default();
        head.push(Style::Bold, section);
        head.push(Style::Plain, " ");
        head.push(Style::Dim, "─".repeat(w.saturating_sub(head.width())));
        out.push(head);

        for (k, what) in keys {
            let mut l = Line::default();
            l.push(Style::Plain, " ");
            l.push(Style::Plain, k);
            l.pad(keyw - k.chars().count() + 2);
            l.push(Style::Muted, util::truncate(what, w.saturating_sub(keyw + 3).max(4)));
            out.push(l);
        }
    }
    out
}

/// Lines the focus dock keeps whatever is selected, and the most it will grow
/// to when the title needs them.
///
/// Fixed height is what a dock under a moving cursor wants: a panel that shrank
/// to fit every short title would take a row off the tree and give it back on
/// every keypress, and the rows you are reading would step up and down while
/// you scrolled past them. But three lines is ninety-odd columns, and a tenth
/// of the titles here run past that — a focus panel that cuts the title is a
/// panel that fails on exactly the rows it exists for. So: three lines always,
/// six when the title needs them, and only the longest tenth ever moves it.
/// How tall the tag picker is allowed to get, before its rule.
///
/// The vocabulary is nineteen words in the store this was built against, which
/// is more than a sidebar can spare beside the task it is about — and the task
/// has to stay on screen, or you are tagging something you can no longer see.
/// So the list scrolls inside this and the filter is how you reach past it,
/// which is the other half of what the filter is for.
pub(super) const TAGS_MAX: usize = 8;

/// The picker's rows: what is on, what is about to change, and — when the
/// filter names something the vocabulary does not hold — the offer to make it.
///
/// `sel` is an index into the *filtered* rows, so the window is computed here
/// against the same list the reducer moved through.
pub(super) fn tags_lines(t: &super::keys::Tags, w: usize, room: usize) -> Vec<Line> {
    use super::keys::{TagRow, TagState};
    let shown = t.shown();
    let rows = room.min(TAGS_MAX).max(1);
    // Keep the cursor in the window, scrolling by as little as will do — the
    // same bargain the tree makes, and for the same reason.
    let first = t.sel.saturating_sub(rows.saturating_sub(1)).min(shown.len().saturating_sub(rows.min(shown.len())));

    let mut out = Vec::new();
    for (i, r) in shown.iter().enumerate().skip(first).take(rows) {
        let mut l = Line::default();
        // The right-hand note, when the row has one to make.
        let mut note = Line::default();
        let (mark, style, name) = match r {
            TagRow::New(name) => {
                note.push(Style::Dim, "new");
                (glyph::TAG_NEW, Style::Accent, name.clone())
            }
            TagRow::Tag(name) => {
                // Any row a project also lends says which one, whatever else
                // is happening to it. On an inherited row it is the reason `␣`
                // will not move it; on one being *removed* it is the warning
                // that removing it will not take the tag off the task, because
                // the project puts it back — which is common, since a task
                // under `wsp` carrying its own `rust` is exactly this.
                if let Some(from) = t.lender(name) {
                    note.push(Style::Dim, util::truncate(from, 12));
                }
                match t.state(name) {
                    // On before and on after: nothing is happening to it.
                    TagState::Kept => (glyph::TAG_ON, Style::Plain, name.clone()),
                    TagState::Adding => (glyph::TAG_NEW, Style::Accent, name.clone()),
                    // Struck through would be better and is not available, so
                    // the mark carries it and the ink goes warn: this row is
                    // the one about to lose something.
                    TagState::Removing => (glyph::TAG_OFF, Style::Warn, name.clone()),
                    // On the task, and drawn as on, because it is — `wsp show`
                    // prints it and so does the detail pane. Dim, because it is
                    // the one row here `␣` cannot move.
                    TagState::Inherited(_) => (glyph::TAG_ON, Style::Dim, name.clone()),
                    TagState::Off => (glyph::QUIET, Style::Muted, name.clone()),
                }
            }
        };
        let room = w.saturating_sub(4 + if note.spans.is_empty() { 0 } else { note.width() + 1 });
        l.push(Style::Plain, " ");
        l.push(style, mark);
        l.push(Style::Plain, " ");
        l.push(style, util::truncate(&name, room));
        if !note.spans.is_empty() {
            l.pad(w.saturating_sub(l.width() + note.width()).max(1));
            l.spans.extend(note.spans);
        }
        l.selected = i == t.sel;
        out.push(l);
    }
    // A filter that matches nothing and offers nothing cannot happen — the
    // `new` row is always there when the filter is non-empty — but an empty
    // vocabulary on an untagged task can, and an empty box reads as broken.
    if out.is_empty() {
        out.push(line(Style::Dim, "  no tags yet — type one"));
    }
    out
}

/// The most rows a card will take out of the tree behind it.
///
/// A card is a knock on the door, not a document: the paragraph an agent wrote
/// may be long and what is worth reading over somebody else's tree is the top
/// of it. What is cut says so, and `o` opens the task in the detail pane, which
/// is the surface with room.
pub(super) const CARD_MAX: usize = 14;

/// And the widest. A sidebar is never this wide and a zoomed pane always is:
/// the card is one paragraph and a question, and a line of it running the whole
/// of a hundred and fifty columns is one the eye loses its place in.
pub(super) const CARD_W: usize = 76;

/// The ask, drawn as a card over the tree.
///
/// A box rather than another dock, and it is the one thing on the panel drawn
/// with a border. Everything else here is furniture — it is *always* there, so
/// a rule is enough to separate it — and this is the exception: it arrived, it
/// is in front of what you were reading, and it is going away again. The border
/// is what says all three.
///
/// Inset by a column so the rows behind it show at both edges. That gap is the
/// whole difference between a card lying on top of the tree and a pane that has
/// replaced it, and it costs two columns of a pane that has thirty-four.
pub(super) fn card_lines(card: &super::rows::Card, w: usize, room: usize) -> Vec<Line> {
    let box_w = w.saturating_sub(2).max(12);
    // Two for the border, two for the padding inside it.
    let text_w = box_w.saturating_sub(4).max(6);
    let rule = |left: &str, right: &str| {
        let mut l = Line::default();
        l.push(Style::Plain, " ");
        l.push(Style::Warn, format!("{left}{}{right}", "─".repeat(box_w - 2)));
        l
    };
    let row = |body: Line| {
        let mut l = Line::default();
        l.push(Style::Plain, " ");
        l.push(Style::Warn, "│");
        l.push(Style::Plain, " ");
        let mut body = body;
        body.fit(text_w);
        l.spans.extend(body.spans);
        l.push(Style::Plain, " ");
        l.push(Style::Warn, "│");
        l
    };

    // What is under the body, worked out before the body is: who is asking, and
    // the keys. Both are fixed and both have to be on screen — a card cut off
    // above its own keys is a popup with no way out written on it — so the
    // paragraph is what gives way, and it is the part that has somewhere else
    // to be read in full.
    let mut tail: Vec<Line> = Vec::new();
    let mut who = Line::default();
    if !card.who.is_empty() {
        who.push(Style::Muted, card.who.clone());
        who.push(Style::Muted, " ");
    }
    who.push(
        Style::Muted,
        match card.ask {
            Some(a) => a.asking().to_string(),
            // Nothing is being asked for, so the line says what raised it
            // rather than inventing a question. `raised this` is thin and it is
            // the truth: somebody wanted it in front of you.
            None => "raised this".to_string(),
        },
    );
    tail.push(who);
    for t in util::wrap(card.ask.map(|a| a.keys()).unwrap_or("↵ ok · o open · x lower"), text_w) {
        tail.push(line(Style::Accent, t));
    }

    // The heading, then the paragraph, then the tail. Built as a list first so
    // the box can be closed at whatever height the pane actually allows.
    let mut inner: Vec<Line> = Vec::new();
    let mut head = Line::default();
    head.push(Style::Warn, format!("{} ", glyph::FLAG));
    head.push(Style::Bold, util::truncate(&card.title, text_w.saturating_sub(2)));
    inner.push(head);
    inner.push(Line::default());

    // Two rules, the heading, and the blank line either side of the body, plus
    // however many lines the tail needs.
    let body_room = room.saturating_sub(5 + tail.len()).max(1);
    let wrapped = util::wrap(&card.body, text_w);
    let cut = wrapped.len() > body_room;
    for (i, t) in wrapped.into_iter().take(body_room).enumerate() {
        let last = cut && i + 1 == body_room;
        inner.push(line(
            Style::Plain,
            if last { util::truncate(&format!("{t} …"), text_w) } else { t },
        ));
    }

    inner.push(Line::default());
    inner.extend(tail);

    let mut out = vec![rule("┌", "┐")];
    out.extend(inner.into_iter().map(row));
    // The bottom rule is never what gets cut. A box that is open at the foot
    // does not read as a box lying over the tree, it reads as a panel that has
    // gone wrong — so if anything has to go it is a line of the paragraph,
    // which is what `body_room` already took care of.
    out.truncate(room.saturating_sub(1).max(2));
    out.push(rule("└", "┘"));
    out
}

pub(super) const FOCUS_MIN: usize = 3;

pub(super) const FOCUS_MAX: usize = 6;

/// The selected row's own words, wrapped to the pane.
///
/// Padded out to [`FOCUS_MIN`] rather than returned short, so the caller draws
/// the same number of rows it reserved.
pub(super) fn focus_lines(ui: &Ui, w: usize) -> Vec<Line> {
    let text = ui.rows.get(ui.sel).map(super::rows::full_text).unwrap_or_default();
    let room = w.saturating_sub(2);
    let wrapped = util::wrap(&text, room);
    let over = wrapped.len() > FOCUS_MAX;
    let mut kept: Vec<String> = wrapped.into_iter().take(FOCUS_MAX).collect();
    // Six lines is a title of about two hundred characters and there is one of
    // those. Say so rather than stop mid-word: a panel whose whole job is the
    // full title has to admit the once it is not showing you one.
    if over {
        if let Some(last) = kept.last_mut() {
            *last = util::truncate(&format!("{last} …"), room);
        }
    }
    let mut out: Vec<Line> = kept
        .into_iter()
        .map(|s| {
            let mut l = Line::default();
            l.push(Style::Plain, " ");
            l.push(Style::Plain, s);
            l
        })
        .collect();
    while out.len() < FOCUS_MIN {
        out.push(Line::default());
    }
    out
}

/// Rows the tree keeps whatever the map wants. A cursor with no neighbours
/// above or below it is not a tree you can aim with, and aiming is the whole
/// reason the map is open.
pub(super) const MIN_TREE_ROWS: usize = 6;

/// Rows kept beyond the cursor when the cursor is what moved. Enough to read
/// where you are going without the view moving under every keystroke: at two,
/// eleven of the fourteen rows on a normal pane are still when you press `j`.
pub(super) const LOOKAHEAD: usize = 2;

/// Where the view sits, given where it sat and where the cursor is now.
///
/// The view has a position of its own and keeps it. What the cursor may do is
/// push it: `sel` has to be on the pane, with `off` rows of company beyond it
/// where there are rows to spare, and the nearest position satisfying that is
/// the one taken — so reading down the tree moves the cursor through a still
/// pane until it reaches the far end, and only then does the tree move.
///
/// This used to be derived from `sel` alone, holding it in the middle of the
/// pane. That is a view with no position: every row of travel scrolled the
/// whole tree, so nothing on screen but the cursor stayed where you last read
/// it, and the two half-screens at the ends — where the clamp stops the view
/// but not the cursor — were the only still ones. A remembered offset and a
/// cursor are two truths about one thing and they do drift, which is why this
/// is a clamp rather than a read: the stored value is never trusted, only
/// brought back into the range that keeps the cursor on the pane.
///
/// Both ends still clamp, so the first and last screens do not scroll into
/// empty space — the cursor rides up to the top and down to the foot there,
/// which is what those screens mean.
pub(super) fn scroll_to(at: usize, sel: usize, n: usize, body: usize, off: usize) -> usize {
    if body == 0 || n <= body {
        return 0;
    }
    let last = n - body;
    // Never more than half a pane of company, or the two demands cross and
    // there is no position that answers both.
    let off = off.min((body - 1) / 2);
    // The window is `at ..= at + body - 1`. Wanting `sel` inside it with `off`
    // to spare at each end is a floor and a ceiling on `at`.
    let ceil = sel.saturating_sub(off);
    let floor = (sel + off + 1).saturating_sub(body);
    at.clamp(floor.min(ceil), ceil).min(last)
}

/// Where a view with no position of its own starts: the cursor near the
/// middle, which is the best a first frame can do — it has no travel behind it
/// to say which way you are reading.
pub(super) fn scroll_for(sel: usize, n: usize, body: usize) -> usize {
    if body == 0 || n <= body {
        return 0;
    }
    sel.saturating_sub(body / 2).min(n - body)
}

/// Where a sidebar ends and a page begins.
///
/// A title in this store averages sixty-four characters, and a row spends about
/// thirty more on depth, marks, an id and the counts down the right — so
/// ninety-six is roughly the width at which the tree stops abbreviating and
/// starts saying what the work is. Below it the panel is a sidebar however it
/// got there; at or above it, it is being read rather than glanced at.
pub(super) const PAGE_MIN: usize = 96;

/// Is this pane a page rather than a sidebar?
///
/// Asked of the width alone, and it decides one thing: whether a project shows
/// all its tasks or six and a count of the rest. How many rows there are cannot
/// come into it, because the cap is what decides how many rows there are, and a
/// rule that read them back would chase itself.
pub(crate) fn is_page(w: usize) -> bool {
    w >= PAGE_MIN
}

/// The whole panel as styled lines. No escapes, no terminal — a backend turns
/// Where the frame's parts land in a pane of this size.
///
/// Extracted from [`frame`] rather than restated, because a click has to be
/// turned back into a row by the *same* arithmetic that drew it. Two copies of
/// this would agree until the day one of them gained a header, and then a
/// click would quietly act on the row above the one under the pointer — which
/// is the kind of wrong that gets blamed on the mouse.
pub(crate) struct Geometry {
    /// Rows above the tree: the title and its rule.
    pub head: usize,
    pub map_rows: usize,
    /// The tag picker and the rule above it, or zero when it is not up.
    pub tags_rows: usize,
    /// The focus dock and the rule above it, or zero when it is off.
    pub focus_rows: usize,
    pub dock_rows: usize,
    pub tree_rows: usize,
    pub tree_len: usize,
    pub scroll: usize,
}

/// Work out the geometry and leave the view holding the offset it was drawn
/// at. The one place `scroll` is written by the panel itself.
///
/// A caller that draws gets this for nothing. Anything that drives the panel
/// without a terminal — the storyboard, a test — has to take the step too, or
/// it is exercising a view that never keeps its place, which is the whole of
/// what the scrolling now is.
pub(crate) fn place(ui: &Ui, view: &mut View, w: usize, h: usize) -> Geometry {
    // An ask nobody has read yet comes up here, on the way to being drawn —
    // which is the one step every path takes, live or offline, where the view
    // can be written to. [`geometry`] deliberately does not: a click asks it
    // where the rows are, and answering that question must not raise a popup.
    super::keys::pop_pending(ui, view);
    let g = geometry(ui, view, w, h);
    view.scroll = Some(g.scroll);
    g
}

pub(super) fn geometry(ui: &Ui, view: &View, w: usize, h: usize) -> Geometry {
    const HEAD: usize = 2;
    const FOOTER: usize = 3;
    let room = h.saturating_sub(HEAD + FOOTER);
    let map_rows = if view.help {
        help_lines(w).len().min(room.saturating_sub(MIN_TREE_ROWS))
    } else {
        0
    };
    // The picker takes its rows before the focus dock does, because it is a
    // mode: it is what the panel is in the middle of, and the dock is a
    // convenience that can wait until it closes.
    let tags_rows = match &view.mode {
        Mode::Tags(t) => {
            let want = tags_lines(t, w, TAGS_MAX).len() + 1;
            match want.min(room.saturating_sub(map_rows + MIN_TREE_ROWS)) {
                n if n < 2 => 0,
                n => n,
            }
        }
        _ => 0,
    };
    // Before the map's rows are taken as well, or a short pane with both up
    // would hand the same rows out twice.
    let focus_rows = if view.focus {
        // Its own rule included, and never that rule on its own: one line of
        // furniture with nothing under it says the title is missing.
        match (focus_lines(ui, w).len() + 1)
            .min(room.saturating_sub(map_rows + tags_rows + MIN_TREE_ROWS))
        {
            n if n < 2 => 0,
            n => n,
        }
    } else {
        0
    };
    let body_rows = room - map_rows - tags_rows - focus_rows;
    let dock_rows = if ui.dock == 0 {
        0
    } else {
        (ui.dock + 1).min(body_rows.saturating_sub(MIN_TREE_ROWS))
    };
    let tree_len = ui.rows.len() - ui.dock;
    let tree_rows = body_rows - dock_rows;
    let scroll = match view.scroll {
        // The cursor pulls the view only when the cursor is what moved. A
        // pointer has just said where it wants to be looking — and where the
        // selection is has nothing to do with it: the wheel is entitled to
        // carry the view clean off the selected row and leave it selected.
        //
        // A cursor down in the dock never pulls it either. The dock is pinned
        // and always drawn, so following the cursor into it would scroll the
        // tree to its end to answer a question about a row that was on screen
        // the whole time.
        Some(s) if !view.keyed || ui.sel >= tree_len => {
            s.min(tree_len.saturating_sub(tree_rows))
        }
        Some(s) => scroll_to(s, ui.sel, tree_len, tree_rows, LOOKAHEAD),
        None => scroll_for(ui.sel.min(tree_len.saturating_sub(1)), tree_len, tree_rows),
    };
    Geometry { head: HEAD, map_rows, tags_rows, focus_rows, dock_rows, tree_rows, tree_len, scroll }
}

/// One row as the frame paints it: drawn to the pane's width and marked when it
/// is the row under the cursor.
///
/// The single place a row becomes a line, so the storyboard's mapping sweep can
/// ask for the line the frame drew rather than for a restatement of it.
pub(crate) fn cell(ui: &Ui, i: usize, w: usize, keys: &[Option<u8>]) -> Line {
    let mut l = render_row(&ui.rows[i], w, keys.get(i).copied().flatten());
    l.selected = i == ui.sel;
    l
}

/// The row a click at pane row `y` landed on, if it landed on one.
///
/// `None` for the title, the rule, the blank tail, the key map and the footer:
/// a click on furniture is not a click on a row, and moving the cursor because
/// somebody clicked a horizontal line is worse than doing nothing.
pub(crate) fn row_at(ui: &Ui, view: &View, w: usize, h: usize, y: usize) -> Option<usize> {
    let g = geometry(ui, view, w, h);

    let drawn = g.tree_rows.min(g.tree_len.saturating_sub(g.scroll));
    if y >= g.head && y < g.head + drawn {
        return Some(g.scroll + (y - g.head));
    }

    // The dock sits at the bottom whatever the tree is doing, under a rule of
    // its own. `dock_rows` counts that rule, so its rows are one fewer.
    if g.dock_rows > 1 {
        let first =
            h.saturating_sub(3 + g.focus_rows + g.tags_rows + g.map_rows + g.dock_rows) + 1;
        if y >= first && y < first + g.dock_rows - 1 {
            let i = g.tree_len + (y - first);
            if i < ui.rows.len() {
                return Some(i);
            }
        }
    }
    None
}

/// The top line: the name, then one mark per running agent.
///
/// A count said there were eleven agents, which is a number you can do nothing
/// with. The strip says the same eleven and which of them have stopped, in the
/// same glyphs and the same colours the rows use — the ones that want you are
/// first and in warn, so the line is read left to right and the answer is at
/// the near end. It is drawn from the whole census whatever the tree is
/// filtered to: a header that went quiet under `R` would be a header you learn
/// to distrust.
///
/// The total stays on the right, because a narrow pane truncates the strip and
/// a truncated strip must not be the only thing saying how many there are.
pub(super) fn header(ui: &Ui, w: usize) -> Line {
    let mut l = Line::default();
    l.push(Style::Bold, "wsp");
    l.push(Style::Plain, " ");
    if ui.census.is_empty() {
        l.push(Style::Dim, "· no agents");
        return l;
    }

    let s = strip(ui, w);
    for (state, _) in ui.census.iter().take(s.shown) {
        let (st, mark) = state.mark();
        l.push(st, mark);
    }
    if s.clipped {
        l.push(Style::Dim, glyph::MORE);
    }
    let right = line(Style::Dim, ui.census.len().to_string());
    l.pad(w.saturating_sub(l.width() + right.width()).max(1));
    l.spans.extend(right.spans);
    l
}

/// Where the strip's marks are, in columns.
///
/// Extracted from [`header`] for the same reason [`geometry`] is extracted from
/// [`frame`]: a click has to be turned back into an agent by the arithmetic
/// that drew it. Two copies would agree until the header gained a word, and
/// then clicking `←` would focus the pane beside the one you pointed at.
pub(super) struct Strip {
    /// Column the first mark is drawn at.
    pub at: usize,
    pub shown: usize,
    /// The rest did not fit and a `⋯` stands for them.
    pub clipped: bool,
}

pub(super) fn strip(ui: &Ui, w: usize) -> Strip {
    // "wsp" and its space, and on the right the total with a column of gap.
    let at = 4;
    let total = ui.census.len();
    let room = w.saturating_sub(at + total.to_string().chars().count() + 1);
    if total > room {
        Strip { at, shown: room.saturating_sub(1), clipped: true }
    } else {
        Strip { at, shown: total, clipped: false }
    }
}

/// What a click on the top line landed on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StripHit {
    /// The nth agent of the census — the one that mark stands for.
    Agent(usize),
    /// The `⋯`: everything the strip could not draw.
    Rest,
}

/// The mark under column `x` of the top line, if it is a mark.
pub(crate) fn strip_at(ui: &Ui, w: usize, x: usize) -> Option<StripHit> {
    if ui.census.is_empty() {
        return None;
    }
    let s = strip(ui, w);
    if x < s.at {
        return None;
    }
    let i = x - s.at;
    if i < s.shown {
        return Some(StripHit::Agent(i));
    }
    if s.clipped && i == s.shown {
        return Some(StripHit::Rest);
    }
    None
}

/// this into something you can look at.
///
/// `&mut` for one field: the view keeps the position it was last drawn at, and
/// this is where it is drawn. [`geometry`] is the rule and stays a function of
/// what it is given — every other caller of it, a click especially, needs the
/// answer this frame used, which is why the frame writes it back rather than
/// leaving each of them to derive its own.
pub(crate) fn frame(ui: &Ui, view: &mut View, w: usize, h: usize) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(header(ui, w));
    lines.push(line(Style::Dim, "─".repeat(w)));

    let footer_rows = 3;
    let g = place(ui, view, w, h);
    let view = &*view;
    let mode = &view.mode;

    // The map takes the rows it needs out of the tree's, never the other way
    // about, and its first line is a ruled heading — so it needs no separator
    // of its own and costs the tree nothing but its own height.
    let map = if view.help { help_lines(w) } else { Vec::new() };
    let map_rows = g.map_rows;
    let keys = hotkeys(ui);

    // The dock keeps its rows whatever the tree is doing. An agent with no
    // work is the row you most need to see and the one the tree would push off
    // the bottom first, since it sorts by work and that pane has none.
    // One line of rule, so the dock reads as its own pane rather than as the
    // tail of the tree. It is only worth its row if the rows it separates fit.
    let dock_rows = g.dock_rows;
    let tree_len = g.tree_len;
    let tree_rows = g.tree_rows;

    // Scroll on the tree's own length. A cursor down in the dock leaves the
    // tree where it was rather than dragging it to the end.
    // A filter that matches nothing draws an empty tree, which looks exactly
    // like a broken panel. Say which it is.
    if tree_len == 0 && ui.review_only {
        lines.push(Line::default());
        lines.push(line(Style::Dim, "  nothing waiting on you"));
        lines.push(line(Style::Dim, "  R for the whole tree"));
    }
    if tree_len == 0 && ui.agents {
        lines.push(Line::default());
        lines.push(line(Style::Dim, "  nobody is running"));
        lines.push(line(Style::Dim, "  w for the work"));
    }
    // A search that matches nothing is the commonest empty tree of the three,
    // because it happens while somebody is still typing the word.
    if tree_len == 0 && !ui.filter.is_empty() {
        lines.push(Line::default());
        lines.push(line(Style::Dim, &format!("  nothing matches \"{}\"", ui.filter)));
        lines.push(line(Style::Dim, "  ⌫ widens · esc clears"));
    }

    // The tree: one row to a line, every line the width of the pane. A wide
    // pane is a wide row — which is the whole of what a fullscreen panel is,
    // and why there is no second layout here for [`row_at`] to disagree with.
    let scroll = g.scroll;
    for i in scroll..tree_len.min(scroll + g.tree_rows) {
        lines.push(cell(ui, i, w, &keys));
    }
    while lines.len()
        < h.saturating_sub(footer_rows + g.focus_rows + g.tags_rows + map_rows + dock_rows)
    {
        lines.push(Line::default());
    }
    if dock_rows > 0 {
        lines.push(line(Style::Dim, "─".repeat(w)));
        // The dock is one row to a line whatever the tree is doing above it —
        // it is a pinned list of five, not a tree, and columns of it would be
        // five names in a row of white space.
        for i in tree_len..(tree_len + dock_rows - 1).min(ui.rows.len()) {
            lines.push(cell(ui, i, w, &keys));
        }
    }
    // The card, over the rows the tree just drew.
    //
    // Last, so it lies on top of whatever was there, and clipped to the tree's
    // own rows so it never covers the dock — the section at the foot is where
    // the *other* asks are, and a popup that hid them would answer one question
    // by hiding the queue behind it. Centred in what is left, because a card
    // pinned to the top edge reads as a header and this is not one.
    if let Mode::Card(card) = mode {
        let room = tree_rows.min(CARD_MAX);
        // A card is a paragraph somebody wrote, and a paragraph does not want a
        // hundred and fifty columns: past about seventy the eye loses the start
        // of the next line. So it stops growing and is centred in what is left,
        // which in a sidebar — where it has never been as wide as this — is the
        // same card in the same place.
        let box_w = w.min(CARD_W);
        let left = (w - box_w) / 2;
        let card = card_lines(card, box_w, room);
        let top = g.head + tree_rows.saturating_sub(card.len()) / 2;
        for (i, l) in card.into_iter().enumerate() {
            if let Some(slot) = lines.get_mut(top + i) {
                let mut placed = Line::default();
                placed.pad(left);
                placed.spans.extend(l.spans);
                *slot = placed;
            }
        }
    }

    let hidden = map.len() - map_rows;
    lines.extend(map.into_iter().take(map_rows));

    // The picker, above the focus dock and below the map: it is the thing the
    // panel is in the middle of, so it sits as near the footer — and the
    // filter line — as anything that is not the footer itself.
    if let Mode::Tags(t) = mode {
        if g.tags_rows > 1 {
            lines.push(line(Style::Dim, "─".repeat(w)));
            lines.extend(tags_lines(t, w, g.tags_rows - 1).into_iter().take(g.tags_rows - 1));
        }
    }

    // Last before the footer, so it sits where the eye already goes for what
    // the panel is saying about right now — and so the tree above it is one
    // unbroken run whatever else is up.
    if g.focus_rows > 1 {
        lines.push(line(Style::Dim, "─".repeat(w)));
        lines.extend(focus_lines(ui, w).into_iter().take(g.focus_rows - 1));
    }

    lines.push(line(Style::Dim, "─".repeat(w)));

    // No inbox count here: it is a row of its own now, at the top, where it can
    // be folded and aimed at. Restating it would be two places to keep true.
    let mut foot = Line::default();
    if ui.blocked > 0 {
        foot.push(Style::Warn, format!("blocked {}", ui.blocked));
    } else {
        foot.push(Style::Dim, "blocked 0");
    }
    // Beside it, and only when there is one: an agent stops at `review`, so a
    // count here is work finished and waiting on you. Zero is the resting
    // state and saying so every time would train the eye to skip the line.
    // Not while the filter is up: there the count is the tree itself, and
    // `review 1  review only` is the footer saying one thing twice.
    if ui.review > 0 && !ui.review_only {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Warn, format!("review {}", ui.review));
    }
    // And what an agent has raised a hand about. The section at the foot draws
    // these in full, and this is here for the one case the section cannot
    // cover: a pane too short to give it any rows. A hand raised and nowhere on
    // screen is the one failure this whole thing exists to prevent.
    if ui.flagged > 0 {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Warn, format!("{} {}", glyph::FLAG, ui.flagged));
    }
    if ui.show_done {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Accent, "+done");
    }
    // The search, wearing the key that set it. Loudest of the three markers
    // because it is the one that hides the most: a phrase left on can take the
    // tree down to two rows, and a panel showing two rows with nothing to say
    // why is one you stop trusting. Truncated to what is left of the line —
    // the phrase is up in the footer to be recognised, not read.
    if !ui.filter.is_empty() {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Accent, format!("/{}", util::truncate(&ui.filter, 16)));
    }
    // A filter left on silently reads as an empty backlog, and the panel is
    // furniture you stop looking at. So it says so, every frame it is up.
    if ui.review_only {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Accent, "review only");
    }
    // Same reason as the filter: the agents view answers a different question
    // from the one the panel is normally asking, and a panel you have stopped
    // reading is one that has to say when it is not showing you the work.
    if ui.agents {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Accent, "agents");
    }
    lines.push(foot);

    // The last line belongs to whatever the panel is waiting for: a value, an
    // answer, a destination — and only otherwise a message or the key hint.
    lines.push(match mode {
        Mode::Prompt { verb, buffer } => {
            let mut l = Line::default();
            l.push(Style::Accent, format!("{}> ", verb.label()));
            // A prompt that opens holding a value needs to say how to be rid
            // of it — backspacing a title away is not an edit anyone makes
            // twice. Only while there is something to clear, and only when
            // the pane is wide enough that the hint is not eating the value.
            const CLEAR: &str = "  ^U";
            let hint = !buffer.is_empty() && w > l.width() + CLEAR.chars().count() + 12;
            // Show the tail once the value outruns the pane, so the caret is
            // always the thing you can see.
            let room = w
                .saturating_sub(l.width() + 1)
                .saturating_sub(if hint { CLEAR.chars().count() } else { 0 });
            let shown: String = if buffer.chars().count() > room {
                buffer.chars().skip(buffer.chars().count() - room).collect()
            } else {
                buffer.clone()
            };
            l.push(Style::Plain, shown);
            l.push(Style::Accent, "▌");
            if hint {
                l.push(Style::Dim, CLEAR);
            }
            l
        }
        Mode::Confirm { question, .. } => {
            let mut l = Line::default();
            l.push(Style::Warn, util::truncate(question, w.saturating_sub(6)));
            l.push(Style::Dim, "  y/n");
            l
        }
        Mode::Pick { verb } => {
            let mut l = Line::default();
            l.push(Style::Accent, util::truncate(verb.hint(), w.saturating_sub(4)));
            l.push(Style::Dim, "  ↵");
            l
        }
        // The search shares that line for the same reason, and says how much of
        // the tree is left: the count is the whole feedback loop — you type a
        // letter, three hundred becomes forty, another letter, forty becomes
        // four, and that is when you stop typing and start looking.
        Mode::Find { buffer } => {
            let mut l = Line::default();
            l.push(Style::Accent, "find> ");
            let hint = match ui.rows.iter().filter(|r| matches!(r, super::rows::Row::Task { .. })).count() {
                0 if buffer.is_empty() => String::new(),
                0 => "  none".to_string(),
                1 => "  1 task".to_string(),
                n => format!("  {n} tasks"),
            };
            let room = w.saturating_sub(l.width() + 1 + hint.chars().count());
            let shown: String = if buffer.chars().count() > room {
                buffer.chars().skip(buffer.chars().count() - room).collect()
            } else {
                buffer.clone()
            };
            l.push(Style::Plain, shown);
            l.push(Style::Accent, "▌");
            if w > l.width() + hint.chars().count() {
                l.pad(w.saturating_sub(l.width() + hint.chars().count()));
                l.push(Style::Dim, hint);
            }
            l
        }
        // The filter and the hint share the line the prompt uses, because they
        // are the same thing: a place to type. What is different is that the
        // typing narrows a list rather than becoming the value — so the line
        // has to say what the keys do, which a prompt never needs to.
        Mode::Tags(t) => {
            let mut l = Line::default();
            l.push(Style::Accent, "tag> ");
            l.push(Style::Plain, t.filter.clone());
            l.push(Style::Accent, "▌");
            // The key you next want, and only that. With nothing picked the
            // thing to do is pick; with something picked it is save, and the
            // count is what makes `↵` worth pressing rather than `esc`.
            // Never "apply" over an empty diff: `↵` there says `unchanged` and
            // runs no command, and a footer promising otherwise is a footer
            // lying about what a key does.
            let n = t.changes().len();
            let hint = if n == 0 { "  ␣ toggle".to_string() } else { format!("  ↵ saves {n}") };
            if w > l.width() + hint.chars().count() {
                l.pad(w.saturating_sub(l.width() + hint.chars().count()));
                l.push(Style::Dim, hint);
            }
            l
        }
        // The card carries its own keys, inside the box where the question is.
        // What the footer is worth here is the queue: answering one of three
        // and finding another underneath is fine, and being surprised by it is
        // not.
        Mode::Card(_) => {
            let mut l = Line::default();
            l.push(Style::Warn, format!("{} ", glyph::FLAG));
            l.push(
                Style::Muted,
                match ui.flagged {
                    0 | 1 => "raised".to_string(),
                    n => format!("1 of {n} raised"),
                },
            );
            l
        }
        Mode::Browse => match &ui.message {
            Some((m, at)) if at.elapsed() < Duration::from_secs(4) => {
                line(Style::Accent, util::truncate(m, w))
            }
            // With the map up, the hint is a fifth of what is already on
            // screen. What the line is worth then is the way out — and an
            // honest count of what the pane was too short to hold.
            _ if view.help => {
                let mut l = line(Style::Dim, "? or esc closes");
                if hidden > 0 {
                    l.push(Style::Plain, "  ");
                    l.push(Style::Muted, format!("{hidden} more"));
                }
                l
            }
            // The way out, on the surface that needs one said. The sidebar is
            // always there and `?` is the answer to everything else; this is a
            // tab somebody opened a minute ago, and a fullscreen you have to
            // hunt the exit of is one you close by closing the tab.
            _ if view.full => line(Style::Dim, "↵ open · E edit · a add · q closes · ? keys"),
            _ => line(Style::Dim, "↵ open · E edit · a add · ? keys"),
        },
    });

    lines.truncate(h);
    lines
}

/// The one style-to-escape table. `pub(crate)` because the renderer's terminal
/// target ([`crate::draw::Ansi`]) paints through it too, and a second copy is
/// how a colour comes to mean one thing in a pane and another in a storyboard.
pub(crate) fn ansi_of(style: Style) -> &'static str {
    match style {
        Style::Plain => "",
        Style::Dim => DIMC,
        Style::Bold => BOLD,
        Style::Muted => MUTED,
        Style::Accent => ACCENT,
        Style::Warn => WARN,
        Style::Query => QUERY,
    }
}

/// What the live panel prints.
///
/// The one-cell case of [`crate::draw::Ansi`], and delegating rather than
/// repeating it is the point of t-260816-092: a pane wsp is standing in is a
/// spec of one pane at the origin, so there is one positioning loop and one
/// inverse rule instead of two that agree until somebody edits one of them.
pub(crate) fn to_ansi(frame: &[Line], w: usize, h: usize) -> String {
    use crate::arrange::{Body, Content, Rect, Slot};
    use crate::draw::{Ansi, Cell, Target};

    let cell = Cell {
        slot: Slot::new("self"),
        label: String::new(),
        rect: Rect { x: 0, y: 0, w: w as u32, h: h as u32 },
        body: Body::Rendered(Content::new("self")),
    };
    let mut out = Ansi::fresh();
    out.rendered(&cell, &frame.to_vec());
    out.finish()
}

pub(super) fn class_of(style: Style) -> &'static str {
    match style {
        Style::Plain => "p",
        Style::Dim => "d",
        Style::Bold => "b",
        Style::Muted => "m",
        Style::Accent => "a",
        Style::Warn => "w",
        Style::Query => "q",
    }
}

pub(super) fn esc_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// The same frame as a block of styled spans. The panel is text, so this is
/// not an approximation of the terminal — it is the same cells with the same
/// colours, and it costs no font work and no new dependency.
pub(crate) fn to_html_spans(l: &Line) -> String {
    let mut out = String::new();
    for s in &l.spans {
        out.push_str(&format!(
            "<span class=\"{}\">{}</span>",
            class_of(s.style),
            esc_html(&s.text)
        ));
    }
    out
}

pub(crate) fn to_html(frame: &[Line], w: usize) -> String {
    let mut out = String::from("<pre class=\"wsp\">");
    for l in frame {
        let mut l = l.clone();
        l.fit(w);
        out.push_str(if l.selected { "<span class=\"sel\">" } else { "<span>" });
        out.push_str(&to_html_spans(&l));
        out.push_str("</span>\n");
    }
    out.push_str("</pre>");
    out
}
