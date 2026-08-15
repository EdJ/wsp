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

mod install;
mod keys;
mod render;
mod rows;
mod run;
mod shared;
mod verbs;

pub(crate) use keys::{apply_key, Effect, View};
// The live path reaches these through `super::keys`; only the storyboard's
// tests need them from outside.
#[cfg(test)]
pub(crate) use keys::{click, wheel, Hit};
// Same bargain: `rows` builds the tree for this module, and the storyboard
// tests the naming rule directly rather than through six fabricated panes.
#[cfg(test)]
pub(crate) use rows::pane_name;
pub(crate) use render::{frame, glyph, legend, line, to_ansi, to_html, to_html_spans, Line, Style};
// The live click mapping reaches `row_at` through `super::render` inside this
// module; only the storyboard's tests need it from outside, so it is exported
// for them alone rather than widening the surface for everyone.
#[cfg(test)]
pub(crate) use render::row_at;
#[cfg(test)]
pub(crate) use render::to_html_spans as spans_of;

/// Draw one row exactly as the frame would, so a test can ask whether the row
/// a click maps to is the row that was painted there.
#[cfg(test)]
pub(crate) fn render_row_for_test(ui: &Ui, i: usize, w: usize) -> Line {
    let keys = rows::hotkeys(&ui.rows);
    rows::render_row(&ui.rows[i], w, keys[i])
}
pub(crate) use rows::{collect, refetch_into, RowKind, Snapshot, Target, Ui};
pub(crate) use run::{exe_stamp, stty, term_size};
pub use install::{install, install_if_adopted, uninstall};
pub use run::run;
