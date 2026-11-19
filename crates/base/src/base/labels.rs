//! The labels backend carries on wsp's own panes.

/// backend's label on a panel, so we can find ours again. Ours are furniture
/// and never appear in the tree as work.
pub const PANEL_LABEL: &str = "wsp";

/// The label backend carries on a detail pane, so we can find ours again.
pub const VIEW_LABEL: &str = "wsp:view";

/// And on the panel `Z` opens in a tab of its own — the whole tree, at the
/// width of the workspace.
///
/// A tab rather than backend's `pane.zoom`, which is what this started as and had
/// to stop being. Ed lost a panel to it: `Z`, then a switch to another agent,
/// and the pane never came back — "without being recoverable".
///
/// A zoom is not a bigger pane. It is a display mode over the whole tab, set by
/// one pane and outliving it: measured against the live server, it survives a
/// switch to another workspace and back, and `pane.focus` will move the keyboard
/// onto a pane the zoom is hiding, so what is on the screen and what the keys
/// reach stop being the same question. The panel is furniture — thought about
/// by nobody — and furniture has no business holding a workspace in a mode it
/// cannot see the state of, whose only undo is a key inside itself.
///
/// A tab hides nothing, backend's own switcher lists it, and closing it puts you
/// back where you were.
pub const FULL_LABEL: &str = "wsp:full";

/// And on a board's. Furniture like the other two, and marked so for the same
/// two reasons: the tree must not draw it as a pane somebody is working in, and
/// the next `K` has to be able to find the board already open and replace it.
///
/// Its own label rather than [`VIEW_LABEL`] shared: `inspect` and `close_view`
/// find the detail pane by that string, and a board wearing it would be closed
/// by `esc` and mistaken for a detail pane that already exists.
pub const BOARD_LABEL: &str = "wsp:board";
