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
//! `filter` — what `/` narrows the tree to — is the fourth, and it is the one
//! that had to be argued rather than being obvious. It looks like `A` and `R`:
//! it changes which rows exist, which is this file's whole test. What separates
//! it is how long it lives. Those two are switches somebody sets and works
//! under for an afternoon; a search is a question asked a second ago and
//! answered by the next keypress, and the key that answers it is often `S` or
//! `c` — which sends the keyboard to another workspace, where a tree narrowed
//! to a phrase you typed somewhere else, about work you have just handed on,
//! is not the panel you left. It is four keystrokes to ask again and `esc` to
//! be rid of, so the cost of getting this wrong is asymmetric: a search that
//! does not travel costs a retype, and one that does costs a panel that looks
//! broken in twenty-one places at once.
//!
//! `scroll` *is* carried, which it was not at first and should have been. The
//! tree has a scroll offset of its own now, and the cursor no longer says where
//! it sits: the same row can be at the top of one pane and at the foot of
//! another. Carrying the cursor without it left the panel you switched to
//! showing the same rows from a different place, which reads as exactly the
//! reset this was supposed to end. Back when the offset was derived from the
//! cursor, only a click could produce one worth carrying — that is why this
//! field was thought of as the pointer's.
//!
//! Carrying a field is only half of it: something has to write. The mouse was
//! the half that did not, for longer than the field was missing — the wheel and
//! a click are each answered in a branch of the event loop that returns as soon
//! as it has redrawn, and the loop's own write is at the far end of the body
//! they never reach. So the keyboard travelled and the pointer did not, which
//! from a chair looks exactly like scroll being the one thing not shared.
//! [`super::run::share`] is where both now go.
//!
//! It lives in the state directory rather than the store proper, which is what
//! keeps it out of `Store::fingerprint` — that walks `projects/` and `tasks/`
//! only. A file inside the fingerprint would give every panel on the machine a
//! full rebuild on every fold and every press of `j`.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::store::Store;

use super::keys::View;
use super::rows::{Cursor, Target};

const FILE: &str = "panel-view.json";

/// What every panel agrees about.
#[derive(Default, PartialEq, Eq)]
pub(super) struct Shared {
    collapsed: Vec<String>,
    expanded: Vec<String>,
    reveal: Vec<String>,
    show_done: bool,
    review_only: bool,
    /// `w`, and here for the same reason `review_only` is: all three change
    /// which rows exist rather than which of them is selected, and a panel
    /// showing a different set of rows from the one you just left is a panel
    /// you have to re-read. It was the one field of `View` this struct did not
    /// carry — not by argument, it simply was not added when `w` was.
    agents: bool,
    ids: bool,
    /// `F`. It changes how much of a row you can read rather than which rows
    /// there are, which is why it is easy to leave out of a list of filters —
    /// but it is a dock you turn on and go on working under, not a glance. Left
    /// behind, it came off every time herdr swapped which panel was on screen,
    /// and the titles went back to their first twenty-five characters with
    /// nothing having asked for that.
    focus: bool,
    cursor: Cursor,
    /// Where the tree is scrolled to. Shared because the panel you arrive at
    /// should be looking at what the one you left was looking at, and the
    /// cursor does not say that on its own: the view has a position, and the
    /// same row can be at the top of one pane and at the foot of another.
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
    pub(super) fn of(view: &View, cursor: impl Into<Cursor>) -> Shared {
        Shared {
            collapsed: sorted(&view.collapsed),
            expanded: sorted(&view.expanded),
            reveal: sorted(&view.reveal),
            show_done: view.show_done,
            review_only: view.review_only,
            agents: view.agents,
            ids: view.ids,
            focus: view.focus,
            cursor: cursor.into(),
            scroll: view.scroll,
        }
    }

    /// Put this back onto a view. The cursor comes back separately because it
    /// cannot be applied until the rows exist.
    pub(super) fn apply(self, view: &mut View) -> Cursor {
        view.collapsed = self.collapsed.into_iter().collect();
        view.expanded = self.expanded.into_iter().collect();
        view.reveal = self.reveal.into_iter().collect();
        view.show_done = self.show_done;
        view.review_only = self.review_only;
        view.agents = self.agents;
        view.ids = self.ids;
        view.focus = self.focus;
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
            "agents": self.agents,
            "ids": self.ids,
            "focus": self.focus,
            "cursor": target_to_json(&self.cursor.target),
            "docked": self.cursor.docked,
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
            agents: flag("agents"),
            ids: flag("ids"),
            focus: flag("focus"),
            cursor: Cursor {
                target: v.get("cursor").map(target_from_json).unwrap_or(Target::Nothing),
                docked: flag("docked"),
            },
            scroll: v.get("scroll").and_then(|x| x.as_u64()).map(|n| n as usize),
        }
    }
}

fn target_to_json(t: &Target) -> Value {
    match t {
        Target::Project(id) => json!({ "kind": "project", "id": id }),
        Target::Task(id) => json!({ "kind": "task", "id": id }),
        Target::Pane(id) => json!({ "kind": "pane", "id": id }),
        Target::Seat(project) => json!({ "kind": "seat", "id": project }),
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
        Some("seat") => Target::Seat(id()),
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

/// Hand this view to every other panel, if it is not already what they have.
///
/// Call this from the paths where *this pane's* person did something, and from
/// nowhere else — a key, a wheel, a click. A panel that shared on every draw
/// would write on ticks and on rebuilds too, including the rebuild where it is
/// still holding a cursor the file asked for and the rows cannot yet honour:
/// it would answer that wish by overwriting it with its own. Input is what
/// makes this pane the one that wins.
///
/// `agreed` is the last text this panel wrote or took, so a gesture that
/// changes nothing durable — and most do not — costs no write at all.
pub(super) fn share(store: &Store, view: &View, cursor: impl Into<Cursor>, agreed: &mut String) {
    let now = rendered(&Shared::of(view, cursor));
    if now != *agreed {
        write(store, &now);
        *agreed = now;
    }
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
        assert_eq!(cursor, Cursor::from(Target::Task("t-260815-063".into())));
        assert!(fresh.collapsed.contains("audio"));
        assert!(fresh.collapsed.contains("vst"));
        assert!(fresh.show_done);
    }

    /// `w` travels like `A` and `R` do. It was the one field of `View` that
    /// `Shared` did not carry, so pressing it moved one panel and left the
    /// other twenty-one on the tree — the reset this module exists to end,
    /// applied to the view you reach for when you want to know who has stopped.
    ///
    /// The second half is what an older panel writes. During an install the
    /// panels cross over one at a time, so a file with no `agents` key at all
    /// has to read as "not the agents view" rather than take the reader down.
    #[test]
    fn the_agents_view_travels_between_panels() {
        let mut v = View::default();
        v.agents = true;
        let shared = Shared::of(&v, Target::Pane("w1:p6".into()));

        let mut fresh = View::default();
        assert!(!fresh.agents);
        Shared::from_json(&shared.to_json()).apply(&mut fresh);
        assert!(fresh.agents, "w did not follow the panel you switched to");

        let old = serde_json::json!({ "show_done": true });
        let mut after = View::default();
        after.agents = true;
        Shared::from_json(&old).apply(&mut after);
        assert!(!after.agents, "a file written before this field read as the agents view");
    }

    /// `F` travels too. It is a dock you leave up while you read down a tree of
    /// long titles, so losing it on a workspace switch — or on the reload that
    /// follows every rebuild of the binary — is the same reset as losing the
    /// folds, arriving by a route nobody presses a key for.
    ///
    /// And, as with `w`, a file written before the field existed has to read as
    /// off: during an install the panels cross over one at a time, and the old
    /// one goes on writing the old shape while the new one reads it.
    #[test]
    fn the_focus_dock_travels_between_panels() {
        let mut v = View::default();
        v.focus = true;
        let shared = Shared::of(&v, Target::Task("t-260815-129".into()));

        let mut fresh = View::default();
        assert!(!fresh.focus);
        Shared::from_json(&shared.to_json()).apply(&mut fresh);
        assert!(fresh.focus, "F did not follow the panel you switched to");

        let old = serde_json::json!({ "show_done": true });
        let mut after = View::default();
        after.focus = true;
        Shared::from_json(&old).apply(&mut after);
        assert!(!after.focus, "a file written before this field left the dock up");
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

    /// Where the tree is scrolled to is the part of where it sits that the
    /// cursor does not imply — the same row can be at the top of one pane and
    /// at the foot of another. Left behind, the panel you switch to shows the
    /// same rows from a different place.
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

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wsp-{}-{}", tag, std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// What the mouse does is durable, so the mouse has to write.
    ///
    /// The wheel moves the cursor and a click pins the view, and for a while
    /// neither reached the file: both are answered in a branch of the event
    /// loop that redraws and returns, and the loop's own write is at the far
    /// end of a body they never get to. The keyboard travelled and the pointer
    /// did not, which from a chair looks exactly like the scroll being the one
    /// thing not shared.
    #[test]
    fn a_gesture_that_changes_the_view_reaches_the_file() {
        let dir = scratch("share");
        let store = Store::at(dir.clone(), dir.clone());
        let mut view = View::default();
        let mut agreed = String::new();

        share(&store, &view, Target::Task("t-1".into()), &mut agreed);
        let first = read(&store).expect("the first share writes");
        assert_eq!(first, agreed);

        // A wheel is three of what `j` does: the cursor lands somewhere else,
        // and where the cursor is *is* where the tree sits.
        share(&store, &view, Target::Task("t-4".into()), &mut agreed);
        assert_ne!(read(&store).as_deref(), Some(first.as_str()), "a moved cursor is a new view");

        // A click pins the row under the pointer, which the cursor does not
        // imply — the one part of where the tree sits that has to travel on
        // its own.
        let pinned = agreed.clone();
        view.scroll = Some(12);
        share(&store, &view, Target::Task("t-4".into()), &mut agreed);
        assert_ne!(read(&store).as_deref(), Some(pinned.as_str()), "a pin is part of the view");

        // And it arrives: the panel you switch to is looking where you left it.
        let mut theirs = View::default();
        let want = parse(&read(&store).unwrap()).apply(&mut theirs);
        assert_eq!(theirs.scroll, Some(12));
        assert_eq!(want, Cursor::from(Target::Task("t-4".into())));

        // Nothing changed, so nothing is written. A wheel against the end of
        // the tree is a burst of events that move nothing.
        let settled = agreed.clone();
        let before = std::fs::metadata(store.state.join(FILE)).unwrap();
        share(&store, &view, Target::Task("t-4".into()), &mut agreed);
        assert_eq!(agreed, settled);
        assert_eq!(
            std::fs::metadata(store.state.join(FILE)).unwrap().modified().unwrap(),
            before.modified().unwrap(),
            "a gesture that changes nothing durable costs no write",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Which side of the dock the cursor is on travels with it.
    ///
    /// A pane is drawn twice — under the task it claimed and again in the
    /// section pinned at the foot — so a panel told only "this pane" puts its
    /// cursor on the tree copy, which is the same jump that made scrolling
    /// down unusable, arriving by workspace switch instead of by tick.
    #[test]
    fn the_side_of_the_dock_travels_with_the_cursor() {
        for want in [
            Cursor { target: Target::Pane("w1:p6".into()), docked: true },
            Cursor::from(Target::Pane("w1:p6".into())),
        ] {
            let there = Shared::of(&View::default(), want.clone());
            let back = Shared::from_json(&there.to_json());
            assert_eq!(back.cursor, want, "the side did not survive the file");
        }
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

    /// The search stays in the pane it was typed in — the one field of `View`
    /// that changes which rows exist and is deliberately left behind. The
    /// argument is at the top of this file; this is the assertion that keeps it
    /// from being quietly reversed by somebody adding "the missing field".
    #[test]
    fn a_search_does_not_follow_you_to_another_panel() {
        let mut v = View::default();
        v.filter = "tuning".into();
        let shared = Shared::of(&v, Target::Task("verb-006".into()));
        assert!(!rendered(&shared).contains("tuning"), "the phrase was written to the file");

        let mut fresh = View::default();
        Shared::from_json(&shared.to_json()).apply(&mut fresh);
        assert!(fresh.filter.is_empty(), "the search travelled");

        // And adopting somebody else's view does not clear a search of your
        // own: this panel is the one being typed in, and a fold arriving from
        // next door must not take the tree back out from under it.
        let mut mine = View::default();
        mine.filter = "tuning".into();
        Shared::from_json(&Shared::of(&view_with(&["audio"], false), Target::Nothing).to_json())
            .apply(&mut mine);
        assert_eq!(mine.filter, "tuning");
        assert!(mine.collapsed.contains("audio"), "the rest of the view still travels");
    }

    /// A missing or unreadable file is the first run, not an error.
    #[test]
    fn nothing_written_yet_is_not_a_failure() {
        let empty = Shared::from_json(&json!({}));
        assert_eq!(empty.cursor, Cursor::default());
        assert!(empty.collapsed.is_empty());
        assert!(!empty.show_done);
    }
}
