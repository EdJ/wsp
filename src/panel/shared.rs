//! The half of a panel's view that belongs to the person rather than the pane.
//!
//! herdr owns panes through workspaces, so a panel cannot be one pane that
//! follows you — t-260815-040 has the evidence. What it can be is twenty-two
//! panes nobody can tell apart, and this is the difference between the two: the
//! folds, the filters and the cursor live in the store, so the panel you switch
//! into is already showing what the one you left was showing.
//!
//! Deliberately not everything. `mode` is a half-typed title, `showing` is
//! which detail pane *this* workspace has open, and `land_on` is a one-shot.
//! Carrying any of those across would be worse than the reset it is fixing.
//!
//! `scroll` *is* carried, which it was not at first and should have been. The
//! tree normally has no scroll offset of its own — it holds the cursor near the
//! middle and the window follows, so sharing the cursor shares where the tree
//! sits. A click is the exception: it pins the row under the pointer, and a
//! pinned view that did not travel left the panel you switched to showing the
//! same rows from a different place, which reads as exactly the reset this was
//! supposed to end.
//!
//! It lives in the state directory rather than the store proper, which is what
//! keeps it out of `Store::fingerprint` — that walks `projects/` and `tasks/`
//! only. A file inside the fingerprint would give every panel on the machine a
//! full rebuild on every fold and every press of `j`.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::store::Store;

use super::keys::View;
use super::rows::Target;

const FILE: &str = "panel-view.json";

/// What every panel agrees about.
#[derive(Default, PartialEq, Eq)]
pub(super) struct Shared {
    collapsed: Vec<String>,
    expanded: Vec<String>,
    reveal: Vec<String>,
    show_done: bool,
    review_only: bool,
    ids: bool,
    cursor: Target,
    /// Only ever set by a click, and cleared by the next keystroke. Shared
    /// because the panel you arrive at should be looking at what the one you
    /// left was looking at, however it came to be looking there.
    scroll: Option<usize>,
}

/// Sets are written sorted. The file is compared against the last one written
/// to decide whether to write at all, and iteration order over a `HashSet` is
/// not stable — without this, every keystroke would look like a change.
fn sorted(set: &HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.iter().cloned().collect();
    v.sort();
    v
}

impl Shared {
    pub(super) fn of(view: &View, cursor: Target) -> Shared {
        Shared {
            collapsed: sorted(&view.collapsed),
            expanded: sorted(&view.expanded),
            reveal: sorted(&view.reveal),
            show_done: view.show_done,
            review_only: view.review_only,
            ids: view.ids,
            cursor,
            scroll: view.scroll,
        }
    }

    /// Put this back onto a view. The cursor comes back separately because it
    /// cannot be applied until the rows exist.
    pub(super) fn apply(self, view: &mut View) -> Target {
        view.collapsed = self.collapsed.into_iter().collect();
        view.expanded = self.expanded.into_iter().collect();
        view.reveal = self.reveal.into_iter().collect();
        view.show_done = self.show_done;
        view.review_only = self.review_only;
        view.ids = self.ids;
        view.scroll = self.scroll;
        self.cursor
    }

    fn to_json(&self) -> Value {
        json!({
            "collapsed": self.collapsed,
            "expanded": self.expanded,
            "reveal": self.reveal,
            "show_done": self.show_done,
            "review_only": self.review_only,
            "ids": self.ids,
            "cursor": target_to_json(&self.cursor),
            "scroll": self.scroll,
        })
    }

    fn from_json(v: &Value) -> Shared {
        let list = |k: &str| -> Vec<String> {
            v.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let flag = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
        Shared {
            collapsed: list("collapsed"),
            expanded: list("expanded"),
            reveal: list("reveal"),
            show_done: flag("show_done"),
            review_only: flag("review_only"),
            ids: flag("ids"),
            cursor: v.get("cursor").map(target_from_json).unwrap_or(Target::Nothing),
            scroll: v.get("scroll").and_then(|x| x.as_u64()).map(|n| n as usize),
        }
    }
}

fn target_to_json(t: &Target) -> Value {
    match t {
        Target::Project(id) => json!({ "kind": "project", "id": id }),
        Target::Task(id) => json!({ "kind": "task", "id": id }),
        Target::Pane(id) => json!({ "kind": "pane", "id": id }),
        Target::Overflow(key) => json!({ "kind": "overflow", "id": key }),
        Target::Inbox => json!({ "kind": "inbox" }),
        Target::Unattached => json!({ "kind": "unattached" }),
        Target::Nothing => json!({}),
    }
}

fn target_from_json(v: &Value) -> Target {
    let id = || v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    match v.get("kind").and_then(|x| x.as_str()) {
        Some("project") => Target::Project(id()),
        Some("task") => Target::Task(id()),
        Some("pane") => Target::Pane(id()),
        Some("overflow") => Target::Overflow(id()),
        Some("inbox") => Target::Inbox,
        Some("unattached") => Target::Unattached,
        _ => Target::Nothing,
    }
}

/// The file as text. Text rather than a `Shared`, because the caller decides
/// whether to adopt by comparing it against what it last agreed with — and a
/// panel must not adopt its own state back. Re-applying it would undo
/// `land_on`: the file still says where the cursor was *before* the key that
/// created the thing the cursor is supposed to follow.
pub(super) fn read(store: &Store) -> Option<String> {
    std::fs::read_to_string(store.state.join(FILE)).ok()
}

pub(super) fn parse(text: &str) -> Shared {
    match serde_json::from_str::<Value>(text) {
        Ok(v) => Shared::from_json(&v),
        Err(_) => Shared::default(),
    }
}

/// What this view looks like on disk, without putting it there. The caller
/// holds the last thing it wrote and compares against this, so a cursor moving
/// along rows it cannot land on — or any key that changes nothing durable, and
/// most of them change nothing — costs no write at all.
pub(super) fn rendered(shared: &Shared) -> String {
    serde_json::to_string_pretty(&shared.to_json()).unwrap_or_default()
}

/// `write_atomic` is a temp and a rename, so a panel reading this while another
/// writes it gets one version or the other and never half of one. No lock: one
/// keyboard means one writer, and two panels cannot disagree about what the
/// person just did.
pub(super) fn write(store: &Store, text: &str) {
    let _ = std::fs::create_dir_all(&store.state);
    let _ = crate::store::write_atomic(&store.state.join(FILE), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_with(collapsed: &[&str], done: bool) -> View {
        let mut v = View::default();
        v.collapsed = collapsed.iter().map(|s| s.to_string()).collect();
        v.show_done = done;
        v
    }

    /// The whole point: what one panel had is what the next one draws.
    #[test]
    fn a_view_survives_the_round_trip() {
        let v = view_with(&["audio", "vst"], true);
        let shared = Shared::of(&v, Target::Task("t-260815-063".into()));
        let back = Shared::from_json(&shared.to_json());

        let mut fresh = View::default();
        let cursor = back.apply(&mut fresh);
        assert_eq!(cursor, Target::Task("t-260815-063".into()));
        assert!(fresh.collapsed.contains("audio"));
        assert!(fresh.collapsed.contains("vst"));
        assert!(fresh.show_done);
    }

    /// Sets are unordered, and the write is skipped by comparing text. If the
    /// text moved with iteration order every keystroke would write a file that
    /// said the same thing.
    #[test]
    fn the_same_view_always_renders_the_same_text() {
        let a = Shared::of(&view_with(&["audio", "vst", "meta"], false), Target::Inbox);
        let b = Shared::of(&view_with(&["vst", "meta", "audio"], false), Target::Inbox);
        assert_eq!(rendered(&a), rendered(&b));
    }

    /// A click pins the row under the pointer, and that pin is the one part of
    /// where the tree sits that the cursor does not imply. Left behind, the
    /// panel you switch to shows the same rows from a different place.
    #[test]
    fn a_pinned_view_travels_with_the_rest() {
        let mut v = View::default();
        v.scroll = Some(12);
        let back = Shared::from_json(&Shared::of(&v, Target::Inbox).to_json());
        let mut fresh = View::default();
        back.apply(&mut fresh);
        assert_eq!(fresh.scroll, Some(12));

        // And an unpinned view must come back unpinned rather than as zero,
        // which would be a pin at the top of the tree.
        let loose = Shared::from_json(&Shared::of(&View::default(), Target::Inbox).to_json());
        let mut other = View::default();
        other.scroll = Some(9);
        loose.apply(&mut other);
        assert_eq!(other.scroll, None);
    }

    /// Every arm, because a cursor that comes back as `Nothing` silently gives
    /// up the row the person was on.
    #[test]
    fn every_cursor_makes_the_round_trip() {
        for t in [
            Target::Project("wsp".into()),
            Target::Task("t-260815-063".into()),
            Target::Pane("w1:p6".into()),
            Target::Overflow("wsp".into()),
            Target::Inbox,
            Target::Unattached,
            Target::Nothing,
        ] {
            assert_eq!(target_from_json(&target_to_json(&t)), t, "{t:?} did not survive");
        }
    }

    /// A missing or unreadable file is the first run, not an error.
    #[test]
    fn nothing_written_yet_is_not_a_failure() {
        let empty = Shared::from_json(&json!({}));
        assert_eq!(empty.cursor, Target::Nothing);
        assert!(empty.collapsed.is_empty());
        assert!(!empty.show_done);
    }
}
