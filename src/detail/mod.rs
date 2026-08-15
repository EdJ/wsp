//! `wsp view` — the detail pane.
//!
//! The panel answers "what is there"; this answers "what is this". It runs in a
//! pane of its own beside the panel, and follows whatever the panel last opened
//! — so pressing `↵` costs one pane for the whole session rather than one per
//! thing you look at, and the pane you are reading never moves.
//!
//! It shares the panel's `Line`/`Style` model, which means the same frame can be
//! painted to a terminal or dropped into the storyboard, and the two views
//! cannot drift apart on colour.
//!
//! Three parts, split where the work splits: [`render`] draws a task or a
//! project, [`editors`] gets the pair of editors a pop-out opened to go, and
//! [`run`] is the pane itself. What stays here is the seam between the panel
//! and the view — where one leaves a target and the other picks it up.

mod editors;
mod render;
mod run;

pub(crate) use render::{frame, Ctx};
pub use run::run;

use serde_json::json;

use crate::store::Store;

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
