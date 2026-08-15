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

pub(super) const DIMC: &str = "\x1b[2m";

pub(super) const BOLD: &str = "\x1b[1m";

pub(super) const INV: &str = "\x1b[7m";

pub(super) const OFF: &str = "\x1b[0m";

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
    pub const REVIEW: &str = "◆";
    pub const DONE: &str = "✓";
    pub const DOING: &str = "▸";
    pub const MORE: &str = "⋯";
    pub const NEEDS_YOU: &str = "←";
    /// A pane with no agent in it.
    pub const SHELL: &str = "▫";
    /// Something is written in Overview or Details.
    pub const NOTES: &str = "≡";
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
            "Everywhere else",
            "Rows and markers that are not about a single task.",
            vec![
                mark(&[(Style::Warn, g::NEEDS_YOU)], "wants you", "an idle agent on a task that is still doing — it has stopped and you are the blocker"),
                mark(&[(Style::Warn, "1 "), (Style::Warn, g::NEEDS_YOU)], "how many", "the same count, in the header"),
                mark(&[(Style::Dim, g::MORE), (Style::Muted, " 2 more")], "overflow", "past the six-task cap; ↵ opens the tail in place"),
                mark(&[(Style::Accent, g::WORKING), (Style::Plain, " "), (Style::Muted, "Trance Video")], "a pane", "nested under the task it claimed, or under the project it stands in"),
                mark(&[(Style::Dim, g::SHELL), (Style::Plain, " "), (Style::Muted, "Trance Lite")], "a shell", "a pane with no agent — never started, as against an idle one that stopped"),
                mark(&[(Style::Dim, g::NOTES)], "written on", "something is in this row's Overview or Details — E opens it"),
                mark(&[(Style::Dim, g::OPEN), (Style::Plain, " "), (Style::Muted, "inbox")], "a group", "not a project, but still a scope — folds and takes the cursor like one"),
                mark(&[(Style::Dim, "1")], "hotkey", "1-9 jump straight to that agent's terminal"),
                mark(&[(Style::Accent, "+done")], "showing done", "A is on, so finished work is included"),
            ],
        ),
        (
            "Colour on its own",
            "Six roles, used consistently regardless of glyph.",
            vec![
                mark(&[(Style::Plain, "plain")], "claimed", "a task with an agent on it"),
                mark(&[(Style::Muted, "muted")], "unclaimed", "a task nobody is on; agent names"),
                mark(&[(Style::Dim, "dim")], "structure", "carets, counts, punctuation, finished work"),
                mark(&[(Style::Bold, "bold")], "project", "project names only"),
                mark(&[(Style::Accent, "accent")], "live", "running agents and work in flight"),
                mark(&[(Style::Warn, "warn")], "wants a decision", "blocked, or waiting on you"),
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

/// Rows the tree keeps whatever the map wants. A cursor with no neighbours
/// above or below it is not a tree you can aim with, and aiming is the whole
/// reason the map is open.
pub(super) const MIN_TREE_ROWS: usize = 6;

/// Where the body starts, given where the cursor is.
///
/// The selection is held near the middle rather than pushed against an edge.
/// Scrolling only when the cursor reaches the last row means it then *stays*
/// there, and a cursor parked on the bottom line shows you everything you have
/// already walked past and nothing you are about to reach. Both ends clamp, so
/// the first and last screens do not scroll into empty space — the cursor rides
/// up to the top and down to the foot there, which is what those screens mean.
///
/// Derived from `sel` every frame rather than remembered: a stored offset and a
/// cursor are two truths about one thing, and they drift.
pub(super) fn scroll_for(sel: usize, n: usize, body: usize) -> usize {
    if body == 0 || n <= body {
        return 0;
    }
    sel.saturating_sub(body / 2).min(n - body)
}

/// The whole panel as styled lines. No escapes, no terminal — a backend turns
/// this into something you can look at.
pub(crate) fn frame(ui: &Ui, view: &View, w: usize, h: usize) -> Vec<Line> {
    let mode = &view.mode;
    let mut lines: Vec<Line> = Vec::new();

    let mut head = Line::default();
    head.push(Style::Bold, "wsp");
    head.push(Style::Plain, " ");
    head.push(Style::Dim, "·");
    head.push(Style::Plain, format!(" {} ", ui.agents_total));
    head.push(Style::Dim, "agents ·");
    head.push(Style::Plain, " ");
    if ui.needs > 0 {
        head.push(Style::Warn, format!("{} {}", ui.needs, glyph::NEEDS_YOU));
    } else {
        head.push(Style::Dim, "·");
    }
    lines.push(head);
    lines.push(line(Style::Dim, "─".repeat(w)));

    let footer_rows = 3;
    let room = h.saturating_sub(lines.len() + footer_rows);

    // The map takes the rows it needs out of the tree's, never the other way
    // about, and its first line is a ruled heading — so it needs no separator
    // of its own and costs the tree nothing but its own height.
    let map = if view.help { help_lines(w) } else { Vec::new() };
    let map_rows = map.len().min(room.saturating_sub(MIN_TREE_ROWS));
    let body_rows = room - map_rows;
    let keys = hotkeys(&ui.rows);

    // The dock keeps its rows whatever the tree is doing. An agent with no
    // work is the row you most need to see and the one the tree would push off
    // the bottom first, since it sorts by work and that pane has none.
    // One line of rule, so the dock reads as its own pane rather than as the
    // tail of the tree. It is only worth its row if the rows it separates fit.
    let dock_rows = if ui.dock == 0 {
        0
    } else {
        (ui.dock + 1).min(body_rows.saturating_sub(MIN_TREE_ROWS))
    };
    let tree_len = ui.rows.len() - ui.dock;
    let tree_rows = body_rows - dock_rows;

    // Scroll on the tree's own length. A cursor down in the dock leaves the
    // tree where it was rather than dragging it to the end.
    // A filter that matches nothing draws an empty tree, which looks exactly
    // like a broken panel. Say which it is.
    if tree_len == 0 && ui.review_only {
        lines.push(Line::default());
        lines.push(line(Style::Dim, "  nothing waiting on you"));
        lines.push(line(Style::Dim, "  R for the whole tree"));
    }

    let anchor = ui.sel.min(tree_len.saturating_sub(1));
    let scroll = scroll_for(anchor, tree_len, tree_rows);
    for (i, row) in ui.rows.iter().enumerate().take(tree_len).skip(scroll).take(tree_rows) {
        let mut l = render_row(row, w, keys[i]);
        l.selected = i == ui.sel;
        lines.push(l);
    }
    while lines.len() < h.saturating_sub(footer_rows + map_rows + dock_rows) {
        lines.push(Line::default());
    }
    if dock_rows > 0 {
        lines.push(line(Style::Dim, "─".repeat(w)));
        for (i, row) in ui.rows.iter().enumerate().skip(tree_len).take(dock_rows - 1) {
            let mut l = render_row(row, w, keys[i]);
            l.selected = i == ui.sel;
            lines.push(l);
        }
    }
    let hidden = map.len() - map_rows;
    lines.extend(map.into_iter().take(map_rows));

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
    if ui.show_done {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Accent, "+done");
    }
    // A filter left on silently reads as an empty backlog, and the panel is
    // furniture you stop looking at. So it says so, every frame it is up.
    if ui.review_only {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Accent, "review only");
    }
    lines.push(foot);

    // The last line belongs to whatever the panel is waiting for: a value, an
    // answer, a destination — and only otherwise a message or the key hint.
    lines.push(match mode {
        Mode::Prompt { verb, buffer } => {
            let mut l = Line::default();
            l.push(Style::Accent, format!("{}> ", verb.label()));
            // Show the tail once the value outruns the pane, so the caret is
            // always the thing you can see.
            let room = w.saturating_sub(l.width() + 1);
            let shown: String = if buffer.chars().count() > room {
                buffer.chars().skip(buffer.chars().count() - room).collect()
            } else {
                buffer.clone()
            };
            l.push(Style::Plain, shown);
            l.push(Style::Accent, "▌");
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
            _ => line(Style::Dim, "↵ open · E edit · a add · ? keys"),
        },
    });

    lines.truncate(h);
    lines
}

pub(super) fn ansi_of(style: Style) -> &'static str {
    match style {
        Style::Plain => "",
        Style::Dim => DIMC,
        Style::Bold => BOLD,
        Style::Muted => MUTED,
        Style::Accent => ACCENT,
        Style::Warn => WARN,
    }
}

/// What the live panel prints.
///
/// Inverse is re-asserted per span rather than wrapped around the row: every
/// span ends with a reset, and a reset clears inverse too, so a single opening
/// `INV` only ever highlighted a selected row up to its first styled run.
pub(crate) fn to_ansi(frame: &[Line], w: usize, h: usize) -> String {
    let mut out = String::from("\x1b[H\x1b[2J");
    for (i, l) in frame.iter().take(h).enumerate() {
        out.push_str(&format!("\x1b[{};1H", i + 1));
        let mut l = l.clone();
        l.fit(w);
        for s in &l.spans {
            if l.selected {
                out.push_str(INV);
            }
            out.push_str(ansi_of(s.style));
            out.push_str(&s.text);
            out.push_str(OFF);
        }
    }
    out
}

pub(super) fn class_of(style: Style) -> &'static str {
    match style {
        Style::Plain => "p",
        Style::Dim => "d",
        Style::Bold => "b",
        Style::Muted => "m",
        Style::Accent => "a",
        Style::Warn => "w",
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
