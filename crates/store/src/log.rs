//! `## Log`: the one module that edits it.
//!
//! A log entry is a dated bullet appended under the `## Log` heading, in the
//! record's own section order (a worklist's `## Groups` must not be re-filed
//! under every note). Every other module records an entry through these verbs
//! — [`append`] / [`append_to_list`] when it will save the record itself with
//! other changes, [`note`] / [`note_to_list`] when the entry is the whole edit.
//! `Task::log` and `Worklist::log` are the same verbs under their old names.

use crate::model::{append_dated, append_dated_in_order, Task, Worklist, WORKLIST_SECTIONS};
use crate::store::Store;

/// What `wsp land` writes in a task's `## Log` when its branch reaches the trunk
/// (the full story is on [`crate::repair`]'s use of it).
pub const LANDED: &str = "wsp: landed";

/// Append a dated entry to a task's log; the caller saves.
pub fn append(t: &mut Task, line: &str) {
    append_dated(&mut t.body, "Log", line);
}

/// Append a dated entry to a worklist's log; the caller saves.
pub fn append_to_list(w: &mut Worklist, line: &str) {
    append_dated_in_order(&mut w.body, "Log", line, &WORKLIST_SECTIONS);
}

/// Append a dated entry to a bare body (a record edited as text).
pub fn append_to_body(body: &mut String, line: &str) {
    append_dated(body, "Log", line);
}

/// Record an entry on a task and save it.
pub fn note(store: &Store, t: &mut Task, line: &str) -> std::io::Result<()> {
    append(t, line);
    store.save_task(t)
}

/// Record an entry on a worklist and save it.
pub fn note_to_list(store: &Store, w: &mut Worklist, line: &str) -> std::io::Result<()> {
    append_to_list(w, line);
    store.save_worklist(w)
}
