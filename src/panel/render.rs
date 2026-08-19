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
use super::rows::{hotkeys, render_row, Row, Ui};

/// How long the footer keeps what it was told.
///
/// One number for the panel and for the board it draws in place, because they
/// are the same footer on the same pane: a message that outlived the page it
/// was written on, or died sooner there, would be the seam showing.
pub(super) const NOTE: Duration = Duration::from_secs(4);

pub(super) const ACCENT: &str = "\x1b[38;2;95;191;164m";

pub(super) const WARN: &str = "\x1b[38;2;224;138;75m";

pub(super) const MUTED: &str = "\x1b[38;2;125;140;150m";

/// A question somebody has written down. Its own hue rather than a second use
/// of [`WARN`]: an agent stopped in front of you and an agent stopped behind a
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

    /// The columns from `n` on. The other half of [`fit`]: `fit` keeps the head
    /// of a line and this keeps the tail, and an overlay needs both because
    /// what it covers is a hole in the middle.
    fn from_column(&self, n: usize) -> Vec<Span> {
        let mut skip = n;
        let mut out: Vec<Span> = Vec::new();
        for s in &self.spans {
            let len = s.text.chars().count();
            if skip >= len {
                skip -= len;
                continue;
            }
            out.push(Span { text: s.text.chars().skip(skip).collect(), style: s.style });
            skip = 0;
        }
        out
    }

    /// Lay `over` on this line at column `left`, keeping what is behind it on
    /// both sides.
    ///
    /// The one way anything floats over the panel, because the obvious way is
    /// wrong and looks right. A popup used to be placed by building a fresh
    /// line, padding it out to `left` and appending the box — and that padding
    /// is not spacing, it is an eraser: every row the box occupied lost the
    /// tree beside it, so a menu at the right-hand edge blanked the whole width
    /// of the panel to its left. Ed, 2026-08-19: *"it wipes the horizontal
    /// space from the rest of the panel"*. A popover keeps what is behind it
    /// and covers only its own footprint, which is three steps rather than two
    /// — clip to `left`, the box, then whatever the row had after it.
    ///
    /// The highlight is what a composite cannot keep. `selected` is a property
    /// of a whole line ([`crate::draw`] re-asserts inverse per span across the
    /// row), so a row half covered cannot be half inverse, and inverting the
    /// box along with the row under it is the louder wrong. The thing in front
    /// wins.
    pub(crate) fn overlay(&mut self, left: usize, over: Line) {
        let tail = self.from_column(left + over.width());
        self.fit(left);
        self.spans.extend(over.spans);
        self.spans.extend(tail);
        self.selected = false;
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
    /// Deliberately not yet. The same square as `BLOCKED` because the work is
    /// stopped either way, and smaller and dim because nobody is owed anything
    /// — the shape is what the two states share, the weight is what tells them
    /// apart at a glance down the column.
    pub const PARKED: &str = "▪";
    /// An agent stopped behind a question. The task's own `■` says the work is
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
            "The first column in the tree, and it is the task's own status — an \
             agent on the task never displaces it. The agent hangs on its own \
             row beneath, wearing the agent marks further down, so the two \
             questions get a row each rather than fighting over one column. \
             That there is an agent at all is in the ink: a claimed task is \
             plain, an unclaimed one muted. On the board it is the other way \
             about — the columns have already said the status, so a card wears \
             its agent's mark and falls back to these.",
            vec![
                mark(&[(Style::Dim, g::QUIET)], "not started", "todo, or still in the inbox — one mark for both, because where it is filed is what the tree is already saying"),
                mark(&[(Style::Accent, g::DOING)], "doing", "started, and in hand"),
                mark(&[(Style::Warn, g::BLOCKED)], "blocked", "stopped, and somebody owes it an answer — the reason is on the task"),
                mark(&[(Style::Dim, g::PARKED)], "parked", "deliberately not yet, with the thing that would bring it back written on it — open work, and the quietest row here on purpose"),
                mark(&[(Style::Muted, g::REVIEW)], "review", "done enough to look at"),
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
                mark(&[(Style::Warn, g::BLOCKED), (Style::Warn, "1")], "blocked", "tasks stopped and waiting on an answer. Parked ones are inside the open number and get no mark of their own here: this column is what to do about a project, and the answer to a parked task is nothing"),
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
                mark(&[(Style::Query, g::QUESTION)], "a question", "an agent stopped on a task with a question written on it — drawn wherever that agent is, tree or strip"),
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
                mark(&[(Style::Query, g::QUESTION)], "blocked", "stopped, on a task blocked with a question written on it — its own colour, because it wants an answer rather than a nudge"),
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
                mark(&[(Style::Warn, "warn")], "waiting on you", "an agent stopped in front of you, and work blocked without a question"),
                mark(&[(Style::Query, "query")], "a question", "somebody has written one down and is waiting on the answer"),
            ],
        ),
    ]
}

/// The key map as rows, cut to what the cursor's own row can use — see
/// [`super::keys::keymap`]. A heading rules off to the edge rather than
/// sitting above a blank line: a sidebar is short, and separation has to cost
/// nothing.
pub(super) fn help_lines(ui: &Ui, w: usize) -> Vec<Line> {
    let target = ui.selected_target();
    let flagged = ui.selected_flag().is_some();
    let map = keymap(&target, flagged);
    let keyw = map
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);

    let mut out = Vec::new();
    for (section, keys) in map {
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

/// The most of the paragraph a card will put over the tree behind it.
///
/// A card is a knock on the door, not a document: the paragraph an agent wrote
/// may be long and what is worth reading over somebody else's tree is the top
/// of it. What is cut says how much of it was, and `o` opens the task in the
/// detail pane, which is the surface with room.
///
/// In characters rather than in rows, and that is the fix. A row is not the
/// same amount of reading on every surface: this budget is fourteen rows at
/// [`CARD_W`], which is where it was chosen, and the same fourteen rows in a
/// thirty-four-column sidebar are two hundred characters — a fifth of it. One
/// number served both and only one of them had room, so a card in the sidebar
/// stopped a fifth of the way into a paragraph it had been given the rows to
/// finish. Held as characters, a sidebar spends the budget on eighteen short
/// rows and a zoomed pane on seven long ones, and they are the same paragraph.
pub(super) const CARD_CHARS: usize = 500;

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
///
/// The gap is a column this returns nothing for, not a blank one it draws. It
/// used to be a leading space per line, which is a column of the tree painted
/// out rather than left showing — the same mistake as the padding
/// [`Line::overlay`] exists to undo, in the one place it happened to be nearly
/// invisible because a card is nearly full width. `w` is therefore what the
/// card is *allotted*; the box inside it is two columns narrower, and the
/// caller places it one column in.
///
/// `room` is what the tree has, not what a card is allowed: how tall this one
/// gets is decided here, out of [`CARD_CHARS`] and the width, and `room` is the
/// wall it cannot go through.
///
/// Nothing here says how many other asks are behind this one, and that is on
/// purpose: the footer says it, in the one arm it draws while a card is up, and
/// a card that also said it would be a second copy of one fact in one frame —
/// worded differently, because it would have had to be, and two phrasings of
/// the same number is how a reader learns to trust neither.
pub(super) fn card_lines(card: &super::rows::Card, w: usize, room: usize) -> Vec<Line> {
    let box_w = w.saturating_sub(2).max(12);
    // Two for the border, two for the padding inside it.
    let text_w = box_w.saturating_sub(4).max(6);
    let rule = |left: &str, right: &str| {
        let mut l = Line::default();
        l.push(Style::Warn, format!("{left}{}{right}", "─".repeat(box_w - 2)));
        l
    };
    let row = |body: Line| {
        let mut l = Line::default();
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
    // however many lines the tail needs — and then, of what is left, no more
    // paragraph than the budget buys at this width.
    let budget = CARD_CHARS.div_ceil(text_w).max(3);
    let avail = room.saturating_sub(5 + tail.len()).max(1).min(budget);
    let wrapped = util::wrap(&card.body, text_w);
    let cut = wrapped.len() > avail;
    // One row of the paragraph goes to saying how much of it is not here. That
    // is the row it is worth: ` …` told you it had been cut and left you no way
    // to know whether the rest was a sentence or a page, which is the decision
    // the card is here to support — a thousand words behind an `o` is worth a
    // pane and forty are not.
    let shown = if cut { avail - 1 } else { avail };
    for t in wrapped.iter().take(shown) {
        inner.push(line(Style::Plain, t.clone()));
    }
    if cut {
        let n = wrapped.len() - shown;
        let mut l = Line::default();
        l.push(Style::Muted, format!("… {n} more line{} · ", if n == 1 { "" } else { "s" }));
        l.push(Style::Accent, "o");
        inner.push(l);
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

/// The word in the footer that opens the menu, and the whole of what a click
/// has to land on.
///
/// herdr's own sidebar keeps its menu behind exactly this: the word `menu`,
/// right-aligned in the sidebar's footer strip. Copied rather than invented,
/// and copied as a word rather than as a hamburger — `≡` is already
/// [`glyph::NOTES`] here and means "something is written", and a panel with one
/// glyph meaning two things is one you read twice.
///
/// It is drawn in the counts line, which is the one line of the panel that is
/// never a message and never a prompt. A door that comes and goes with a
/// four-second footer message is a door nobody learns is there.
pub(super) const MENU_BUTTON: &str = "menu";

/// Where the menu box goes: the row it starts on, the column it starts at, and
/// how wide it is.
///
/// One function because the frame draws it and a click has to find it again,
/// and a menu you can see two rows above where you can press it is worse than
/// one with no mouse at all.
///
/// Anchored to the bottom right — sitting on the footer, at the edge the `≡` is
/// at — rather than centred the way a card is. The two are different arrivals:
/// a card is somebody else's question landing in the middle of what you were
/// reading, and a menu is a drawer you opened, so it comes out of the thing you
/// opened it with.
pub(super) fn menu_box(menu: &super::keys::Menu, w: usize, h: usize) -> (usize, usize, usize) {
    // The header strip and its rule, and the three the footer keeps. The same
    // two numbers [`geometry`] holds, and for the same reason: what is left
    // between them is what a popup is allowed.
    const HEAD: usize = 2;
    const FOOTER: usize = 3;
    // Two columns of border, one of padding either side, and two for the mark
    // in the margin.
    let box_w = (menu.width() + 6).min(w.saturating_sub(2)).max(6);
    let left = w.saturating_sub(box_w + 1);
    let rows = menu.items.len() + 2;
    // The footer keeps its three lines: the menu is over the tree, never over
    // the y/n it is about to raise. Clamped under the header rule for a pane
    // too short to hold both, where what gives way is the top of the box —
    // `menu_lines` draws from the top, so a clamp here loses the last rows and
    // the caller's `get_mut` drops them.
    let top = h.saturating_sub(FOOTER + rows).max(HEAD);
    (top, left, box_w)
}

/// The menu, drawn as a box over the tree.
///
/// Bordered for the reason a card is: everything else on the panel is always
/// there, and this arrived and is going away again.
pub(super) fn menu_lines(menu: &super::keys::Menu, w: usize) -> Vec<Line> {
    let text_w = w.saturating_sub(4).max(4);
    let rule = |left: &str, right: &str| {
        let mut l = Line::default();
        l.push(Style::Accent, format!("{left}{}{right}", "─".repeat(w.saturating_sub(2))));
        l
    };
    let mut out = vec![rule("┌", "┐")];
    for (i, item) in menu.items.iter().enumerate() {
        let mut l = Line::default();
        l.push(Style::Accent, "│");
        l.push(Style::Plain, " ");
        let mut body = Line::default();
        // Marked in the margin rather than by inverting the line. `Line`'s
        // inverse is the whole line, and the whole line here includes the box's
        // own border — and, once the frame has composited it over the tree, the
        // tree beside it too, which is now really there. So the mark is a
        // column, and the label is loud instead.
        let picked = i == menu.sel;
        body.push(Style::Accent, if picked { format!("{} ", glyph::CLOSED) } else { "  ".into() });
        body.push(
            if picked { Style::Bold } else { Style::Plain },
            util::truncate(item.label(), text_w.saturating_sub(2)),
        );
        body.fit(text_w);
        l.spans.extend(body.spans);
        l.push(Style::Plain, " ");
        l.push(Style::Accent, "│");
        out.push(l);
    }
    out.push(rule("└", "┘"));
    out
}

/// Which row of the menu a click at `x`,`y` landed on.
///
/// The border is not a row and answers `None`: a click on the frame of a box is
/// a click that missed, and the row nearest it is `quit herdr`.
pub(super) fn menu_at(
    menu: &super::keys::Menu,
    w: usize,
    h: usize,
    x: usize,
    y: usize,
) -> Option<usize> {
    let (top, left, box_w) = menu_box(menu, w, h);
    if x < left || x >= left + box_w {
        return None;
    }
    let i = y.checked_sub(top + 1)?;
    (i < menu.items.len()).then_some(i)
}

/// Is the pointer on the menu at all — its frame included?
///
/// [`menu_at`] answers "which row", and says `None` both for the border and for
/// the tree a mile away. Those are the same answer to that question and
/// different answers to this one: a click on the frame is a click that missed a
/// row, and a click outside is a click that means *put it away*. Dismissing on
/// the border would make the box's own edge a trapdoor.
pub(super) fn menu_holds(
    menu: &super::keys::Menu,
    w: usize,
    h: usize,
    x: usize,
    y: usize,
) -> bool {
    let (top, left, box_w) = menu_box(menu, w, h);
    let rows = menu.items.len() + 2;
    x >= left && x < left + box_w && y >= top && y < top + rows
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
/// ninety-six is roughly the width at which a row stops being truncated and
/// starts saying what the work is. Below it the panel is a sidebar however it
/// got there; at or above it, it is being read rather than glanced at.
///
/// It is a width somebody asks the host *for* — see [`super::verbs::cycle`] and
/// `super::run::Page::width` — and never a width anything is tested against.
/// Nothing about the frame branches on it, because a panel that drew different
/// content either side of a threshold rewrites itself under the reader when a
/// split moves, and that was a real bug: crossing this number un-collapsed
/// folded projects and jumped the scroll. Wider is the same panel with more
/// room.
pub(super) const PAGE_MIN: usize = 96;

/// What a page asks for when there is no width that would be enough.
///
/// [`PAGE_MIN`] is a measure — the tree stops abbreviating around there, and
/// more room after that only spends columns on whitespace. A board has no such
/// number: it is four columns side by side, every one of them a title that was
/// written to be read, and every column it is given goes straight into them.
/// So it asks for more than a terminal has and lets the host decide.
///
/// **That is the mechanism working rather than being cheated.** The host owns
/// the rect: it gives what it has less the column it keeps for itself, and
/// reports what it gave as the size of the next frame — so a request past the
/// end is answered with the end, and the board is built for the width that
/// came back. Asking for a specific large number instead would be this side
/// guessing at a screen it cannot see, and being wrong on every other one.
///
/// The value is the largest the wire can carry: `cols` crosses as a `u16`, and
/// a number that would not fit is a request the host cannot read at all.
pub(super) const WHOLE_SCREEN: usize = u16::MAX as usize;

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
    view.placed = Anchor::of(ui, &g);
    g
}

/// The row this frame was drawn against, and where on the pane it was drawn.
///
/// The reason it is the *cursor* rather than the top visible row: the cursor is
/// the one row the panel already carries across a rebuild by identity — see
/// [`super::rows::Cursor`] — so it is the only anchor that still means
/// something after the rows themselves have been rebuilt. The top row can
/// vanish (an overflow row, an agent that has gone), and an anchor that can
/// vanish is an anchor with a fallback, which is two rules where one will do.
#[derive(Clone)]
pub(crate) struct Anchor {
    /// The row the cursor was on.
    on: super::rows::Cursor,
    /// Lines from the top of the tree to the line it was drawn on.
    line: usize,
}

impl Anchor {
    /// What this frame anchors to, if it anchors to anything.
    ///
    /// Nothing when the cursor is not among the rows the tree drew: down in
    /// the pinned dock, or off the pane entirely because the wheel has carried
    /// the view away from it. Both are states the panel is allowed to be in,
    /// and in neither is the cursor what the reader is looking at.
    fn of(ui: &Ui, g: &Geometry) -> Option<Anchor> {
        let on_pane = ui.sel < g.tree_len && ui.sel >= g.scroll && ui.sel < g.scroll + g.tree_rows;
        on_pane.then(|| Anchor { on: ui.cursor(), line: ui.sel - g.scroll })
    }
}

pub(super) fn geometry(ui: &Ui, view: &View, w: usize, h: usize) -> Geometry {
    const HEAD: usize = 2;
    const FOOTER: usize = 3;
    let room = h.saturating_sub(HEAD + FOOTER);
    let map_rows = if view.help {
        help_lines(ui, w).len().min(room.saturating_sub(MIN_TREE_ROWS))
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
    let tree_len = ui.rows.len() - ui.dock;
    let mut dock_rows = if ui.dock == 0 {
        0
    } else {
        (ui.dock + 1).min(body_rows.saturating_sub(MIN_TREE_ROWS))
    };
    // What is granted is drawn from the top of the dock, so a pane too short
    // for the whole of it cuts the tail — and the tail of a run is its agents.
    // A group heading left standing with nothing beneath it is the one row the
    // section must never end on: it says a project has agents and then hides
    // every one of them, which is worse than the pane being short. Hand that
    // row back to the tree instead. The heading over the section still counts
    // the census in full, so nothing goes missing without saying so.
    while dock_rows > 1
        && matches!(ui.rows.get(tree_len + dock_rows - 2), Some(Row::Group { .. }))
    {
        dock_rows -= 1;
    }
    let tree_rows = body_rows - dock_rows;
    // The cursor is on the row it was on when the last frame placed this
    // offset, so nothing that has changed since is the reader travelling:
    // whatever moved, moved around them. See [`super::keys::View::placed`].
    let held = view.placed.as_ref().filter(|a| a.on == ui.cursor()).map(|a| a.line);
    let scroll = match (view.scroll, held) {
        // The cursor pulls the view only when the cursor is what moved. A
        // pointer has just said where it wants to be looking — and where the
        // selection is has nothing to do with it: the wheel is entitled to
        // carry the view clean off the selected row and leave it selected.
        //
        // A cursor down in the dock never pulls it either. The dock is pinned
        // and always drawn, so following the cursor into it would scroll the
        // tree to its end to answer a question about a row that was on screen
        // the whole time.
        //
        // The anchor below is not consulted here for the same reason: it is the
        // *cursor's* line, and a view the wheel has just placed is not anchored
        // to the cursor. So a tree the wheel has moved does still shift when the
        // rows are rebuilt under it, by however much the rebuild changed above
        // its top row. Answering that needs the top row's identity rather than
        // the cursor's, which is `rows`' to give and not this file's to invent.
        (Some(s), _) if !view.keyed || ui.sel >= tree_len => {
            s.min(tree_len.saturating_sub(tree_rows))
        }
        // Re-anchored rather than clamped: the cursor goes back on the line it
        // was drawn on, and everything the reader can see stays where it was.
        //
        // A row *number* is the wrong thing to carry across any of this. What
        // it counts changes under it — the rows above the cursor multiply when
        // a pane wide enough to be a page stops abbreviating them, an agent's
        // row leaves the tree when the agent does — and how many rows fit
        // changes too, through every block that takes its height from the pane:
        // the focus dock rewrapping at a new width, the map or the tag picker
        // opening, the dock at the foot growing an agent. Clamping the old
        // number back into the new range is not a small correction. Its floor
        // is `sel + LOOKAHEAD + 1 - tree_rows`, so a stale offset that is now
        // too small parks the row you were reading at the *foot* of the pane
        // and slides the whole tree up past you; its ceiling does the mirror
        // image. `Z` twice is the trigger a person hits on purpose, and it
        // moves seven lines on a forty-four row sidebar.
        //
        // [`LOOKAHEAD`] is deliberately not asked for here. Rows beyond the
        // cursor are what *travel* is owed — you are reading in a direction and
        // want to see where you are going — and this is the case where the
        // cursor has not moved at all. Demanding it of a pane that has just
        // changed height is how a change of shape becomes a scroll: the company
        // a taller pane can now afford is company the view has to move to give.
        // What is asked instead is only that the cursor is still drawn, so a
        // pane that got shorter gives up the least it can.
        //
        // At the end of the tree it still moves, and has to: a pane that grew
        // there can only fill from above, and `scroll_to`'s own clamp is what
        // says so. That is the one case where the rows under the reader are
        // allowed to slide, because there is no other answer.
        (Some(_), Some(line)) => {
            scroll_to(ui.sel.saturating_sub(line), ui.sel, tree_len, tree_rows, 0)
        }
        (Some(s), _) => scroll_to(s, ui.sel, tree_len, tree_rows, LOOKAHEAD),
        (None, _) => scroll_for(ui.sel.min(tree_len.saturating_sub(1)), tree_len, tree_rows),
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
    let map = if view.help { help_lines(ui, w) } else { Vec::new() };
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
        // Everything the tree has. What a card is *allowed* is decided inside
        // [`card_lines`], where the width is known — this is only the wall, and
        // handing it a cap worked out here would be handing it the constant
        // that could not tell a sidebar from a zoomed pane.
        let room = tree_rows;
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
                // `left + 1`: the inset the card's doc promises is a column of
                // the tree showing at either edge, so it is a column the box
                // does not cover — not one it paints blank, which is what
                // drawing the gap inside [`card_lines`] amounted to.
                slot.overlay(left + 1, l);
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
    // And the door, at the right-hand edge, where herdr keeps its own. Fitted
    // rather than appended: the counts are what give way if a pane is narrow
    // enough for the two to meet, because a count that is one column short is
    // still a count and a menu that is one column short is not a door.
    foot.fit(w.saturating_sub(MENU_BUTTON.chars().count() + 1));
    foot.push(Style::Plain, " ");
    foot.push(Style::Dim, MENU_BUTTON);
    lines.push(foot);

    // The last line belongs to whatever the panel is waiting for: a value, an
    // answer, a destination — and only otherwise a message or the key hint.
    lines.push(match mode {
        Mode::Prompt { verb, buffer, armed } => {
            let mut l = Line::default();
            l.push(Style::Accent, format!("{}> ", verb.label()));
            // A prompt that opens holding a value needs to say how to be rid
            // of it — backspacing a title away is not an edit anyone makes
            // twice. Only while there is something to clear, and only when
            // the pane is wide enough that the hint is not eating the value.
            //
            // A half-pressed cancel takes the same slot and takes it first: it
            // is the answer to a key pressed a moment ago, not a standing
            // offer, and an `esc` that changed nothing on the line would read
            // as a panel that had stopped listening — which is the reflex
            // being fixed pressing itself harder. Loud, for the same reason.
            const CLEAR: &str = "  ^U";
            const AGAIN: &str = "  esc again";
            let hint = match (armed, buffer.is_empty()) {
                (true, _) => Some((Style::Warn, AGAIN)),
                (false, false) => Some((Style::Dim, CLEAR)),
                (false, true) => None,
            }
            .filter(|(_, text)| w > l.width() + text.chars().count() + 12);
            // Show the tail once the value outruns the pane, so the caret is
            // always the thing you can see.
            let room = w
                .saturating_sub(l.width() + 1)
                .saturating_sub(hint.map_or(0, |(_, text)| text.chars().count()));
            let shown: String = if buffer.chars().count() > room {
                buffer.chars().skip(buffer.chars().count() - room).collect()
            } else {
                buffer.clone()
            };
            l.push(Style::Plain, shown);
            l.push(Style::Accent, "▌");
            if let Some((style, text)) = hint {
                l.push(style, text);
            }
            l
        }
        // The keys, and nothing else. A menu is a list you are reading, so the
        // one thing the line under it is worth is how to take a row and how to
        // put the list away — and `q` is said because `q` is very likely what
        // opened it.
        Mode::Menu(_) => line(Style::Dim, "↵ takes it · q or esc closes"),
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
        //
        // Counted off what is *unread*, not off what is raised. It used to be
        // `ui.flagged`, which is every hand still up — so a panel with one ask
        // waiting and two already read said `1 of 3 raised` and promised two
        // cards that were never coming, and a second ask arriving behind the
        // one on screen moved a number that was already wrong. The word changed
        // with the count: `raised` is the census and it is up in the header and
        // in the section; this line is the only place that says how many of
        // them are still a question.
        Mode::Card(_) => {
            let mut l = Line::default();
            l.push(Style::Warn, format!("{} ", glyph::FLAG));
            l.push(
                Style::Muted,
                match ui.waiting {
                    0 | 1 => "raised".to_string(),
                    n => format!("1 of {n} unread"),
                },
            );
            l
        }
        Mode::Browse => match &ui.message {
            Some((m, at)) if at.elapsed() < NOTE => {
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

    // The menu, over everything the panel has drawn except its own footer.
    //
    // Last of all, and after the docks rather than with the card, because it is
    // the only popup that can be up while the key map is: `the key map` is a
    // row of it, and a menu that slid behind the thing it just opened would be
    // one you thought had failed. The footer keeps its three lines whatever is
    // up — see [`menu_box`] — because the next thing this menu draws is a y/n
    // down there.
    //
    // It is allowed over the agents dock, where a card is deliberately not.
    // The card is clipped to the tree because it is one of a queue of asks and
    // the dock is where the rest of them are; a menu is a drawer somebody just
    // opened, it goes away on `↵`, `q` or `esc`, and it comes out of the word
    // in the footer — which is under the dock, not above it.
    if let Mode::Menu(menu) = mode {
        let (top, left, box_w) = menu_box(menu, w, h);
        for (i, l) in menu_lines(menu, box_w).into_iter().enumerate() {
            if let Some(slot) = lines.get_mut(top + i) {
                slot.overlay(left, l);
            }
        }
    }

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

/// The same colour, as three numbers.
///
/// A third surface — herdr's sidebar drawn natively — is handed cells rather
/// than escapes, because a colour that only exists as an escape sequence is a
/// colour only a terminal can be told. This is the same table as [`ansi_of`]
/// and not a second one: the test below parses that function's output and
/// fails if the two ever disagree, which is the only way a table copied by
/// hand stays copied correctly.
pub(crate) fn rgb_of(style: Style) -> Option<(u8, u8, u8)> {
    match style {
        Style::Plain | Style::Dim | Style::Bold => None,
        Style::Muted => Some((125, 140, 150)),
        Style::Accent => Some((95, 191, 164)),
        Style::Warn => Some((224, 138, 75)),
        Style::Query => Some((176, 140, 217)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;
    use crate::panel::rows::{status_mark, Target};

    /// Whether some section of the map lists this key, however it is
    /// grouped with another one on the same line — `has(&map, "s")` also
    /// matches a row written `"s v"`.
    fn has(map: &[(&str, Vec<(&str, &str)>)], key: &str) -> bool {
        map.iter()
            .flat_map(|(_, keys)| keys.iter())
            .any(|(k, _)| k.split_whitespace().any(|w| w == key))
    }

    /// The map is read to press a key on the row under the cursor, and a
    /// refusal is not a key: `s`/`v` mean nothing until the cursor is on a
    /// task, so away from one they have no business in the list, while `P`
    /// — which never refuses — stays no matter what is selected.
    #[test]
    fn the_key_map_hides_a_task_only_key_when_nothing_is_selected() {
        let map = keymap(&Target::Nothing, false);
        assert!(!has(&map, "s"), "start/review only means anything on a task");
        assert!(has(&map, "P"), "P never refuses, whatever is selected");
    }

    /// `T` talks to a project's governor and nothing else answers to it, so
    /// it has no business on a task's row — and the reverse holds too.
    #[test]
    fn the_key_map_offers_the_governors_key_on_a_seat_and_not_on_a_task() {
        let on_task = keymap(&Target::Task("t1".into()), false);
        assert!(has(&on_task, "s"));
        assert!(!has(&on_task, "T"), "a task has no governor to talk to");

        let on_seat = keymap(&Target::Seat("proj".into()), false);
        assert!(has(&on_seat, "T"));
        assert!(!has(&on_seat, "s"), "a seat is not a task to start or review");
    }

    /// `x` lowers a flag that is actually up — offering it over a task with
    /// nothing raised is the one entry the row itself cannot tell you is
    /// wrong, since `x` on nothing does the same as `q` on nothing: prints a
    /// message and changes nothing.
    #[test]
    fn the_key_map_only_offers_to_lower_a_flag_that_is_raised() {
        let target = Target::Task("t1".into());
        assert!(!has(&keymap(&target, false), "x"));
        assert!(has(&keymap(&target, true), "x"));
    }

    /// A colour has one definition, whichever surface asks for it.
    ///
    /// `ansi_of` and `rgb_of` are two spellings of one table, and a table
    /// spelled twice drifts the first time somebody adjusts a hue in the one
    /// they happen to be reading. This parses the escape the terminal is sent
    /// and asserts the numbers a cell-based surface is handed are the same
    /// ones, so the drift fails here instead of showing up as a sidebar that
    /// is a slightly different green from the panel beside it.
    #[test]
    fn a_style_is_the_same_colour_whether_it_is_drawn_as_an_escape_or_as_cells() {
        for style in [
            Style::Plain,
            Style::Dim,
            Style::Bold,
            Style::Muted,
            Style::Accent,
            Style::Warn,
            Style::Query,
        ] {
            let escape = ansi_of(style);
            let from_escape = escape.strip_prefix("\x1b[38;2;").and_then(|rest| {
                let mut n = rest.trim_end_matches('m').split(';');
                Some((
                    n.next()?.parse().ok()?,
                    n.next()?.parse().ok()?,
                    n.next()?.parse().ok()?,
                ))
            });
            assert_eq!(
                from_escape,
                rgb_of(style),
                "{style:?} is a different colour depending on which surface asks"
            );
        }
    }

    /// The group drifted once already: it described the first column as the
    /// agent's state long after `render_row` had gone back to drawing the
    /// task's own status, and three of its seven marks were of a column
    /// nobody could see any more. Prose cannot be checked, but the marks can
    /// — so they are pinned to the function the column is actually drawn
    /// from, and a status that gains, loses or changes its mark takes the
    /// legend with it or fails here.
    #[test]
    fn the_on_a_task_marks_are_exactly_what_the_first_column_can_draw() {
        let every = [
            Status::Inbox,
            Status::Todo,
            Status::Doing,
            Status::Blocked,
            Status::Parked,
            Status::Review,
            Status::Done,
        ];
        for s in every {
            // No wildcard: an eighth status has to be listed above before
            // this compiles, which is how it gets looked at here at all.
            match s {
                Status::Inbox
                | Status::Todo
                | Status::Doing
                | Status::Blocked
                | Status::Parked
                | Status::Review
                | Status::Done => {}
            }
        }
        let mut drawn: Vec<(Style, &str)> = every.iter().map(|s| status_mark(*s)).collect();
        drawn.sort_by_key(|(_, g)| *g);
        drawn.dedup();

        let group = legend()
            .into_iter()
            .find(|(name, ..)| *name == "On a task")
            .expect("the legend still has an On a task group");
        let mut listed: Vec<(Style, &str)> = group
            .2
            .iter()
            .map(|m| {
                let [span] = &m.sample.spans[..] else {
                    panic!("{}: a status mark is one glyph in one style", m.name)
                };
                (span.style, span.text.as_str())
            })
            .collect();
        listed.sort_by_key(|(_, g)| *g);

        assert_eq!(
            drawn, listed,
            "the legend lists marks the first column does not draw, or misses ones it does"
        );
    }
}
