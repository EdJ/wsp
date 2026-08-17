//! What a key does to the board.
//!
//! Pure, like [`crate::panel::apply_key`] and for the same reason: state in, an
//! [`Action`] out, nothing that touches a terminal or herdr. A test can walk
//! the cursor across four columns and watch what a verb would be aimed at,
//! without a store or a pane anywhere.

use crate::input::Key;
use crate::model::Status;

use super::{Board, Lane};

/// Which card the board is pointed at. A column and a row, because that is what
/// the eye is holding — but the loop keeps the card's *id* across a rebuild, so
/// a card that moves takes the cursor with it. See [`Board::find`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) col: usize,
    pub(crate) row: usize,
}

/// What a key asked for beyond moving the cursor.
pub(crate) enum Action {
    None,
    /// A line for the footer, and nothing else. Refusals live here: a key aimed
    /// at an empty column has to say so, or it reads as a board that has
    /// stopped answering.
    Say(String),
    /// A `wsp` subcommand for the card under the cursor. Argv rather than the
    /// pieces, for the reason the panel gives: the CLI is the one
    /// implementation and this is a caller of it.
    Run { argv: Vec<String>, task: String },
    /// Open the task full-size in an editor tab.
    Edit { id: String },
    /// Show this task in the panel's detail pane, and close the board.
    ///
    /// Both halves are one gesture. A board holds nothing of its own — it is
    /// the project drawn by state, and every fact on it is a fact in the store
    /// — so there is nothing to come back to and nothing lost by going. Leaving
    /// it open behind the thing you opened from it would be a tab you have to
    /// remember to close, standing for a view you have finished with.
    Open { id: String },
    /// Rebuild from the store — the columns are about to be different.
    Refetch,
    /// Show the `done` column, or put it away.
    ShowDone,
    Quit,
}

/// Move the cursor within a column, and between columns.
///
/// Crossing keeps the row where it is and clamps: the columns are different
/// lengths, and a cursor that snapped to the top on every `l` would make
/// reading across a board impossible.
fn go(board: &Board, cur: &mut Cursor, k: Key) {
    let cols = board.columns.len();
    if cols == 0 {
        return;
    }
    match k {
        Key::Down => {
            let n = board.columns[cur.col].cards.len();
            cur.row = (cur.row + 1).min(n.saturating_sub(1));
        }
        Key::Up => cur.row = cur.row.saturating_sub(1),
        Key::Left | Key::Right => {
            cur.col = match k {
                Key::Left => cur.col.saturating_sub(1),
                _ => (cur.col + 1).min(cols - 1),
            };
            let n = board.columns[cur.col].cards.len();
            cur.row = cur.row.min(n.saturating_sub(1));
        }
        _ => {}
    }
}

/// The lane one step along from this card's, in the direction of travel.
///
/// `>` and `<` are the board's own keys — the ones that only make sense here,
/// where the columns are in a row and moving a card is pushing it along them.
/// They resolve to the same four verbs `s v d o` name outright, so there is one
/// set of rules and two ways to reach it.
fn shift(from: Status, forward: bool) -> Option<Lane> {
    let at = Lane::ALL.iter().position(|l| *l == Lane::of(from))?;
    let to = if forward { at.checked_add(1)? } else { at.checked_sub(1)? };
    Lane::ALL.get(to).copied()
}

pub(crate) fn apply_key(k: Key, board: &Board, cur: &mut Cursor) -> Action {
    // Every verb below is about the card under the cursor, and an empty column
    // has none. Worked out once here so each of them can say what it wanted
    // rather than each of them checking.
    let card = board.card_at(cur);

    // A verb that moves the card into a named lane. The cursor follows the card
    // rather than the slot, so the id goes with it.
    let into = |lane: Lane| -> Action {
        let Some(c) = card else {
            return Action::Say(format!("nothing here to send to {}", lane.label()));
        };
        if Lane::of(c.status) == lane {
            return Action::Say(format!("already in {}", lane.label()));
        }
        Action::Run {
            argv: vec![lane.verb().to_string(), c.id.clone()],
            task: c.id.clone(),
        }
    };

    match k {
        Key::Char('q') | Key::Esc | Key::Interrupt => Action::Quit,

        Key::Down | Key::Char('j') => {
            go(board, cur, Key::Down);
            Action::None
        }
        Key::Up | Key::Char('k') => {
            go(board, cur, Key::Up);
            Action::None
        }
        Key::Left | Key::Char('h') => {
            go(board, cur, Key::Left);
            Action::None
        }
        Key::Right | Key::Char('l') => {
            go(board, cur, Key::Right);
            Action::None
        }
        Key::Char('g') | Key::Home => {
            cur.row = 0;
            Action::None
        }
        Key::Char('G') | Key::End => {
            cur.row = board.columns.get(cur.col).map(|c| c.cards.len()).unwrap_or(0).saturating_sub(1);
            Action::None
        }
        // The column a digit names. Four columns and four digits: on a board
        // this is worth a key, because crossing to `done` is three presses of
        // `l` and the columns are right there in front of you, numbered.
        Key::Char(d @ '1'..='4') => {
            let want = d as usize - '1' as usize;
            if want >= board.columns.len() {
                return Action::Say("that column is not showing".into());
            }
            cur.col = want;
            cur.row = cur.row.min(board.columns[want].cards.len().saturating_sub(1));
            Action::None
        }

        // ---- move the card ----
        Key::Char('o') => into(Lane::Todo),
        Key::Char('s') => into(Lane::Doing),
        Key::Char('v') => into(Lane::Review),
        Key::Char('d') => into(Lane::Done),
        Key::Char('>') | Key::Char('.') => match card.and_then(|c| shift(c.status, true)) {
            Some(lane) => into(lane),
            None => Action::Say(match card {
                Some(_) => "done is the far end".into(),
                None => "nothing here to move".into(),
            }),
        },
        Key::Char('<') | Key::Char(',') => match card.and_then(|c| shift(c.status, false)) {
            Some(lane) => into(lane),
            None => Action::Say(match card {
                Some(_) => "todo is the near end".into(),
                None => "nothing here to move".into(),
            }),
        },

        // ---- what comes first in this column ----
        //
        // The board is the one place the answer is visible: priority orders a
        // column and nothing else, so cycling it here moves the card up or down
        // in front of you.
        Key::Char('!') => match card {
            Some(c) => Action::Run {
                argv: vec!["prio".into(), c.id.clone(), c.priority.cycled().as_str().into()],
                task: c.id.clone(),
            },
            None => Action::Say("priority is a card's place in its column".into()),
        },

        Key::Char('E') => match card {
            Some(c) => Action::Edit { id: c.id.clone() },
            None => Action::Say("nothing here to open".into()),
        },
        // `↵` means what it means in the panel — open this — and the board
        // stands down on its way out. The task appears where the panel already
        // shows things, so you come back to the tree with it open rather than
        // to a board you now have to leave.
        Key::Enter => match card {
            Some(c) => Action::Open { id: c.id.clone() },
            None => Action::Say("nothing here to open".into()),
        },
        // The done column, taken away rather than emptied. It is the widest
        // and the least often asked about, and the other three want its
        // columns — but it is also the only record of what the week produced,
        // so it is a key rather than a decision made for you.
        Key::Char('A') => Action::ShowDone,
        Key::Char('r') => Action::Refetch,
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::{collect, Ctx, Scope};
    use crate::model::Task;
    use crate::resolve::Index;
    use std::collections::BTreeMap;

    fn board(spec: &[(&str, &str, &str)], show_done: bool) -> Board {
        let tasks: Vec<Task> = spec
            .iter()
            .map(|(id, status, prio)| {
                let mut t = Task::new("a title", id);
                t.project = Some("wsp".into());
                t.status_raw = (*status).into();
                t.priority_raw = (*prio).into();
                t
            })
            .collect();
        let ctx = Ctx {
            tasks,
            index: Index::new(vec![crate::model::Project::new("wsp")]),
            bindings: BTreeMap::new(),
            claims: BTreeMap::new(),
            panes: Vec::new(),
        };
        collect(&ctx, &Scope::Project("wsp".into()), show_done)
    }

    fn argv_of(a: Action) -> Vec<String> {
        match a {
            Action::Run { argv, .. } => argv,
            Action::Say(m) => panic!("expected a command, got: {m}"),
            _ => panic!("expected a command"),
        }
    }

    fn said(a: Action) -> String {
        match a {
            Action::Say(m) => m,
            Action::Run { argv, .. } => panic!("expected a refusal, got: {argv:?}"),
            _ => panic!("expected a refusal"),
        }
    }

    /// Reading across a board is the gesture it exists for, and the columns are
    /// never the same length. A cursor that reset to the top on every crossing
    /// would make the third column unreachable at its foot.
    #[test]
    fn crossing_keeps_the_row_and_clamps_to_what_is_there() {
        let b = board(
            &[
                ("t-01", "todo", "normal"),
                ("t-02", "todo", "normal"),
                ("t-03", "todo", "normal"),
                ("t-04", "doing", "normal"),
            ],
            true,
        );
        let mut cur = Cursor::default();
        apply_key(Key::Char('j'), &b, &mut cur);
        apply_key(Key::Char('j'), &b, &mut cur);
        assert_eq!(cur, Cursor { col: 0, row: 2 });

        // One card in `doing`, so the row has to come back to it.
        apply_key(Key::Char('l'), &b, &mut cur);
        assert_eq!(cur, Cursor { col: 1, row: 0 });
        // And an empty column takes the cursor rather than refusing it: you
        // have to be able to see you are there.
        apply_key(Key::Char('l'), &b, &mut cur);
        assert_eq!(cur, Cursor { col: 2, row: 0 });
        // The far edge holds.
        apply_key(Key::Char('l'), &b, &mut cur);
        apply_key(Key::Char('l'), &b, &mut cur);
        assert_eq!(cur.col, 3);
    }

    /// The whole of what a board is for: a card, pushed along. `>` and the
    /// named verb reach the same command, because there is one set of rules
    /// about what `done` means and it lives in the CLI.
    #[test]
    fn a_card_is_pushed_along_by_the_same_verbs_that_name_the_lanes() {
        let b = board(&[("t-01", "todo", "normal")], true);
        let mut cur = Cursor::default();
        assert_eq!(argv_of(apply_key(Key::Char('>'), &b, &mut cur)), ["start", "t-01"]);
        assert_eq!(argv_of(apply_key(Key::Char('s'), &b, &mut cur)), ["start", "t-01"]);
        assert_eq!(argv_of(apply_key(Key::Char('d'), &b, &mut cur)), ["done", "t-01"]);
        assert_eq!(argv_of(apply_key(Key::Char('v'), &b, &mut cur)), ["review", "t-01"]);
        // Already there is not a command. It would be a log line, an event and
        // a commit recording a keypress.
        assert_eq!(said(apply_key(Key::Char('o'), &b, &mut cur)), "already in todo");
        // And the near end has nowhere to go back to.
        assert_eq!(said(apply_key(Key::Char('<'), &b, &mut cur)), "todo is the near end");
    }

    /// A blocked card is in the doing column, so pushing it on means `review` —
    /// the column it is in, not the status it carries, is what "along" means.
    #[test]
    fn a_blocked_card_moves_from_the_column_it_is_drawn_in() {
        let b = board(&[("t-01", "blocked", "normal")], true);
        let mut cur = Cursor { col: 1, row: 0 };
        assert_eq!(argv_of(apply_key(Key::Char('>'), &b, &mut cur)), ["review", "t-01"]);
        assert_eq!(argv_of(apply_key(Key::Char('<'), &b, &mut cur)), ["reopen", "t-01"]);
    }

    /// Every verb has to survive being aimed at nothing. An empty column is the
    /// ordinary state of a board — that is what an empty column means — and a
    /// key that panicked there would take the pane with it.
    #[test]
    fn a_verb_aimed_at_an_empty_column_says_so_and_does_nothing() {
        let b = board(&[("t-01", "todo", "normal")], true);
        let mut cur = Cursor { col: 2, row: 0 };
        for k in ['s', 'v', 'd', 'o', '!', 'E', '>', '<'] {
            assert!(
                matches!(apply_key(Key::Char(k), &b, &mut cur), Action::Say(_)),
                "{k} should refuse rather than act",
            );
        }
        assert!(matches!(apply_key(Key::Enter, &b, &mut cur), Action::Say(_)));
    }

    /// The board is a view and holds nothing of its own, so `↵` is a way out of
    /// it as much as a way into the task: the panel shows the task, and the
    /// board — which was only ever the project drawn by state — stands down.
    #[test]
    fn opening_a_card_is_the_board_handing_over_and_going() {
        let b = board(&[("t-01", "todo", "normal")], true);
        let mut cur = Cursor::default();
        match apply_key(Key::Enter, &b, &mut cur) {
            Action::Open { id } => assert_eq!(id, "t-01"),
            _ => panic!("↵ on a card should open it"),
        }
    }

    /// `!` cycles the same three values in the same order the panel does, and
    /// this is the surface where the effect is visible: the card moves up or
    /// down its own column while you watch.
    #[test]
    fn priority_cycles_through_the_same_order_as_everywhere_else() {
        let b = board(&[("t-01", "todo", "normal")], true);
        let mut cur = Cursor::default();
        assert_eq!(argv_of(apply_key(Key::Char('!'), &b, &mut cur)), ["prio", "t-01", "high"]);
        let b = board(&[("t-01", "todo", "high")], true);
        assert_eq!(argv_of(apply_key(Key::Char('!'), &b, &mut cur)), ["prio", "t-01", "low"]);
        let b = board(&[("t-01", "todo", "low")], true);
        assert_eq!(argv_of(apply_key(Key::Char('!'), &b, &mut cur)), ["prio", "t-01", "normal"]);
    }

    /// With `done` put away there are three columns, and the digit that named
    /// the fourth has to answer rather than move the cursor off the board.
    #[test]
    fn a_digit_cannot_reach_a_column_that_is_not_showing() {
        let b = board(&[("t-01", "done", "normal")], false);
        let mut cur = Cursor::default();
        assert_eq!(said(apply_key(Key::Char('4'), &b, &mut cur)), "that column is not showing");
        assert_eq!(cur, Cursor::default());
    }
}
