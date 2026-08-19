//! `wsp view` — a task or a project, in full.
//!
//! The panel answers "what is there"; this answers "what is this".
//!
//! # Two places it is drawn, and which one is the design
//!
//! **A page**, which is the panel itself: `↵` asks its host for a page's width
//! and draws this where the tree was — see `panel::run`'s `TaskPage` and the
//! seam under it, `panel::verbs::expand`. Nothing is rented and nothing is
//! spawned; the frame below is simply given more columns and a different thing
//! to put in them.
//!
//! **A pane of its own**, which is the fallback and what this file's [`run`]
//! is: a `wsp view` process beside the panel, following whatever the panel last
//! opened through a file. It is what a tty panel gets, and any host too old to
//! say it takes widths. It is also the context pane in the tab `E` opens, where
//! it is not a fallback at all but the right shape — the editors are beside it.
//!
//! Which it is changes one thing in the frame, and [`Placed`] is how it is told:
//! the keys at the foot. Everything above them is the same drawing, because it
//! is the same question being answered.
//!
//! It shares the panel's `Line`/`Style` model, which means the same frame can be
//! painted to a terminal, handed to the panel as a page, or dropped into the
//! storyboard, and none of the three can drift apart on colour.
//!
//! Three parts, split where the work splits: [`render`] draws a task or a
//! project, [`editors`] gets the pair of editors a pop-out opened to go, and
//! [`run`] is the pane itself. What stays here is the seam between the panel
//! and the view — where one leaves a target and the other picks it up.

mod editors;
mod render;
mod run;

pub(crate) use editors::{edit_command, slot_path, start_editor, Columns};
pub(crate) use render::{frame, Ctx, WHEEL_STEP};
pub use run::run;

use serde_json::json;

use crate::store::Store;

/// Where a frame is being drawn, which is the one thing about it the data
/// cannot say.
///
/// The same task in the same columns offers different keys depending on who is
/// holding the pane, and the foot of the frame has to say which — a page that
/// advertised `W save and close` would be offering to save editors that are not
/// there, in a view that cannot open one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placed {
    /// A pane of its own: `wsp view`, split off the panel or off the work, and
    /// in an edit tab the context above the editors. It closes by ending.
    Pane,
    /// A page: the panel itself, widened, drawing this instead of the tree. It
    /// closes by giving the room back — see `panel::verbs::expand`.
    Page,
}

/// What a detail pane is pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Focus {
    Task(String),
    Project(String),
    Nothing,
}

impl Focus {
    fn from_json(v: &serde_json::Value) -> Focus {
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        match v.get("kind").and_then(|x| x.as_str()) {
            Some("task") if !id.is_empty() => Focus::Task(id),
            Some("project") if !id.is_empty() => Focus::Project(id),
            _ => Focus::Nothing,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            Focus::Task(id) => json!({ "kind": "task", "id": id }),
            Focus::Project(id) => json!({ "kind": "project", "id": id }),
            Focus::Nothing => json!({}),
        }
    }
}

/// Where the panel leaves the target for the view to pick up. Keyed by
/// workspace, so two workspaces can be reading different things at once.
fn focus_path(store: &Store) -> std::path::PathBuf {
    store.state.join("detail.json")
}

fn read_all(store: &Store) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(focus_path(store))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub(crate) fn set_focus(store: &Store, workspace: &str, focus: &Focus) {
    let mut all = read_all(store);
    all.insert(workspace.to_string(), focus.to_json());
    let _ = std::fs::create_dir_all(&store.state);
    let _ = crate::store::write_atomic(
        &focus_path(store),
        &serde_json::to_string_pretty(&serde_json::Value::Object(all)).unwrap_or_default(),
    );
}

pub(crate) fn get_focus(store: &Store, workspace: &str) -> Focus {
    read_all(store).get(workspace).map(Focus::from_json).unwrap_or(Focus::Nothing)
}
