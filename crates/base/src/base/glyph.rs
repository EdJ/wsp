//! The panel's whole glyph vocabulary, named once so the legend cannot drift
//! from what the rows actually draw. Here rather than in the panel (wsp-435)
//! so the CLI's surfaces draw the same marks without reaching up to it; the
//! panel's `glyph` re-exports these beside the two priority marks it takes
//! from the model.

pub const OPEN: &str = "▾";
pub const CLOSED: &str = "▸";
pub const WORKING: &str = "●";
pub const IDLE: &str = "○";
pub const QUIET: &str = "·";
/// A far seat whose machine is silent: held, not gone.
pub const UNREACHABLE: &str = "⊘";
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
