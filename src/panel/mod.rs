//! `wsp panel` — the sidebar replacement.
//!
//! herdr's sidebar lists workspaces (really: open folders) and, beneath them,
//! agents — a view that falls out of how herdr is built rather than how work is
//! organised. This panel inverts that: the spine is the project tree, tasks
//! hang off projects, and agents attach to tasks. A workspace is only ever a
//! destination, never a heading.
//!
//! It keeps the two things herdr's sidebar is genuinely good at — live agent
//! status and jumping to a terminal — by subscribing to the same event stream
//! and calling `workspace.focus`/`pane.focus` on Enter.
//!
//! No TUI crate: we own one pane, so a string buffer plus ANSI is enough, and
//! the dependency list stays at `serde_json`.

/// herdr's label on the panes we install. Ours are furniture and never appear
/// in the tree as work.
pub(crate) const PANEL_LABEL: &str = "wsp";

/// The label herdr carries on a detail pane, so we can find ours again.
pub(crate) const VIEW_LABEL: &str = "wsp:view";

/// And on the panel `Z` opens in a tab of its own — the whole tree, at the
/// width of the workspace.
///
/// A tab rather than herdr's `pane.zoom`, which is what this started as and had
/// to stop being. Ed lost a panel to it: `Z`, then a switch to another agent,
/// and the pane never came back — "without being recoverable".
///
/// A zoom is not a bigger pane. It is a display mode over the whole tab, set by
/// one pane and outliving it: measured against the live server, it survives a
/// switch to another workspace and back, and `pane.focus` will move the keyboard
/// onto a pane the zoom is hiding, so what is on the screen and what the keys
/// reach stop being the same question. The panel is furniture — installed in
/// every workspace, thought about by nobody — and furniture has no business
/// holding a workspace in a mode it cannot see the state of, whose only undo is
/// a key inside itself.
///
/// A tab hides nothing, herdr's own switcher lists it, and closing it puts you
/// back where you were.
pub(crate) const FULL_LABEL: &str = "wsp:full";

/// And on a board's. Furniture like the other two, and marked so for the same
/// two reasons: the tree must not draw it as a pane somebody is working in, and
/// the next `K` has to be able to find the board already open and replace it.
///
/// Its own label rather than [`VIEW_LABEL`] shared: `inspect` and `close_view`
/// find the detail pane by that string, and a board wearing it would be closed
/// by `esc` and mistaken for a detail pane that already exists.
pub(crate) const BOARD_LABEL: &str = "wsp:board";

mod install;
mod keys;
mod render;
mod rows;
mod run;
mod shared;
mod surface;
mod verbs;

pub(crate) use keys::{apply_input, Effect, Input, View};
// The reducer's inner door: one key, no pane size, no focus. Every input the
// live loop takes now goes through `apply_input`, so the only callers left are
// the tests that predate it and are about a key alone.
#[cfg(test)]
pub(crate) use keys::apply_key;
// The card a raised hand becomes, and the mode it holds the keyboard in. Out
// here for the storyboard, which has to be able to ask whether a card is up —
// a popup that only the live panel could reach would be one no fixture could
// ever draw.
#[cfg(test)]
pub(crate) use keys::Mode;
#[cfg(test)]
pub(crate) use rows::Card;
// The live path reaches these through `super::keys`; only the storyboard's
// tests need them from outside.
#[cfg(test)]
pub(crate) use keys::{click, wheel, Hit};
pub(crate) use render::{
    frame, glyph, legend, line, place, to_ansi, to_html, to_html_spans, Line, Style,
};
// The style tables, out to the renderer. `crate::draw` paints a cell to a
// terminal through exactly these, so there is one answer to what `Accent` looks
// like rather than one per surface.
pub(crate) use render::ansi_of;
pub(crate) use render::{INV as ANSI_INV, OFF as ANSI_OFF};
// Which row is drawn at which line. The live click mapping reaches this
// through `super::render` inside the module; outside it, the storyboard needs
// it to point a *scripted* click at a row rather than at a coordinate — for
// the reason `Driver::to_task` exists rather than a count of `j` presses. A
// script that said `click 2,14` would describe the fixture it was written
// against and would go on passing, silently pointing at a different row, the
// day a project gained a task above it.
pub(crate) use render::row_at;
#[cfg(test)]
pub(crate) use render::to_html_spans as spans_of;

/// The column of the header strip that stands for a pane, if the strip is
/// drawing it.
///
/// The strip is the one clickable thing on the panel that is not a row, so a
/// script pointing at it cannot go through [`row_at`] — and a column counted by
/// hand describes the fixture's census rather than the strip. Asked of
/// `strip_at`, which is the arithmetic the live click already reads, so a
/// scripted click on a mark lands where a real one would.
pub(crate) fn strip_column(ui: &Ui, w: usize, pane: &str) -> Option<usize> {
    (0..w).find(|&x| match render::strip_at(ui, w, x) {
        Some(render::StripHit::Agent(i)) => ui.census.get(i).is_some_and(|(_, a)| a.pane == pane),
        _ => false,
    })
}

/// And the column of the ellipsis at the end of a strip too long for the pane.
/// Only a test drives that one: the filmstrip is drawn at a width where the
/// strip fits, because a scene narrow enough to clip it is a scene about the
/// clipping and not about the panel.
#[cfg(test)]
pub(crate) fn strip_rest_column(ui: &Ui, w: usize) -> Option<usize> {
    (0..w).find(|&x| render::strip_at(ui, w, x) == Some(render::StripHit::Rest))
}

/// Draw one row exactly as the frame would, so a test can ask whether the row
/// a click maps to is the row that was painted there.
#[cfg(test)]
pub(crate) fn render_row_for_test(ui: &Ui, i: usize, w: usize) -> Line {
    render::cell(ui, i, w, &rows::hotkeys(ui))
}
/// The tag picker's rows, exactly as the frame draws them.
///
/// Read from the mode rather than scraped back out of the frame: the picker's
/// rows and a task row both begin with a mark and a space, so a test hunting
/// the frame for one finds the other — which is a test that passes for the
/// wrong reason as readily as it fails.
#[cfg(test)]
pub(crate) fn tag_rows_for_test(view: &View, w: usize) -> Vec<String> {
    match &view.mode {
        keys::Mode::Tags(t) => {
            render::tags_lines(t, w, render::TAGS_MAX).iter().map(|l| l.text()).collect()
        }
        _ => Vec::new(),
    }
}

/// A row's own words, unabridged — what the focus dock is drawing when the
/// cursor is on that row, so a test can ask whether the two agree.
#[cfg(test)]
pub(crate) fn full_text_for_test(ui: &Ui, i: usize) -> String {
    rows::full_text(&ui.rows[i])
}
pub(crate) use rows::{collect, refetch_into, RowKind, Snapshot, Target, Ui};
// The board is a second surface over the same facts, so it takes its glyphs and
// its join of "what herdr says" with "what the store holds" from here rather
// than keeping a copy. A second table of marks would drift the first time
// either gained a state.
pub(crate) use rows::{agent_state, status_mark, word as agent_word, AgentState};
pub(crate) use run::{stty, term_size};
// And it runs the CLI the same way every key in the panel does — same capture,
// same closed stdin, same one implementation of what a verb means.
pub(crate) use verbs::{inspect, pop_out, run_wsp};
pub use install::{install, install_if_adopted, uninstall};
pub use run::run;
// The same panel, drawn by a host that owns the cells — herdr's forked sidebar
// today. See `surface.rs` for why the frame is built here and not there.
pub use surface::run as surface;
