//! What a key does to the board.
//!
//! Pure, like [`crate::panel::apply_key`] and for the same reason: state in, an
//! [`Action`] out, nothing that touches a terminal or herdr. A test can walk
//! the cursor across four columns and watch what a verb would be aimed at,
//! without a store or a pane anywhere.

use crate::input::Key;
use crate::model::Status;
use crate::panel::Tell;

use super::{Board, Lane};

/// Which card the board is pointed at. A column and a row, because that is what
/// the eye is holding — but the loop keeps the card's *id* across a rebuild, so
/// a card that moves takes the cursor with it. See [`Board::find`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) col: usize,
    pub(crate) row: usize,
}

impl Cursor {
    /// The nearest place the cursor can actually be, after the card it was on
    /// has gone. A column that empties under the cursor is ordinary — it is
    /// what finishing the last card in it looks like.
    ///
    /// Here rather than beside either loop because there are two of them now:
    /// the board in its own pane and the board drawn in place of the tree. Two
    /// copies would agree until one of them learned about a fifth column.
    pub(crate) fn clamped(self, board: &Board) -> Cursor {
        let col = self.col.min(board.columns.len().saturating_sub(1));
        let row = self
            .row
            .min(board.columns.get(col).map(|c| c.cards.len()).unwrap_or(0).saturating_sub(1));
        Cursor { col, row }
    }
}

/// What the board is mid-way through, when it is anything.
///
/// The panel keeps its modes on `View`; a board has no view of its own — it is
/// a pure function of the store and the census — so its modes sit beside the
/// cursor, held by whichever loop is drawing it. Both loops hold one: the board
/// in its own tab and the board drawn in place of the tree run the same keys,
/// and two mode types would be one more thing for them to keep agreeing on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Reading and steering. The resting state.
    #[default]
    Browse,
    /// `c` on a card: choosing which spare agent takes it.
    ///
    /// A mode rather than a widget so the columns stay up while you choose —
    /// you are picking an agent *for* a card, and the card has to still be
    /// visible. The rail at the foot is already the list of who is free; this
    /// lights it and hands it the arrows.
    ///
    /// The task is named rather than pointed at. The cursor cannot move while
    /// picking, but a rebuild can still slide a different card under it — an
    /// agent elsewhere claiming work is ordinary — and the deed must not follow
    /// it. Whatever ↵ does is said in the footer by id, so what it names is
    /// always what it does.
    Hand { task: String, sel: usize },
    /// The CLI refused the hand-over and said why; `y` runs the stronger form.
    ///
    /// Held here rather than re-asked through [`Action::Run`]'s `escalate`
    /// because the question has to survive keystrokes — a footer note expires,
    /// and a y/n that vanishes after four seconds is not a question.
    Confirm {
        question: String,
        argv: Vec<String>,
        then: Option<Tell>,
        task: String,
    },
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
    ///
    /// `escalate` is the stronger form of the same command, offered when the
    /// CLI refuses — `claim` refuses on work that is done, on work somebody
    /// live is holding and on a blocked task, three rules the board would
    /// otherwise keep a second copy of. Carrying `--force` here turns each
    /// refusal into the next question instead, exactly as the panel's picks do.
    ///
    /// `then` is what to say to the agent once it has worked — a claim nobody
    /// tells the agent about leaves it sitting idle on work it now holds.
    /// Withheld when the command was refused, because the sentence would be a
    /// lie; it rides here rather than in [`Mode::Confirm`] so that a claim
    /// confirmed under force still has somebody told about it.
    Run {
        argv: Vec<String>,
        escalate: Option<Vec<String>>,
        then: Option<Tell>,
        task: String,
    },
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

/// The key, while the board is only being read. The verbs below are all about
/// the card under the cursor, which is why they live behind [`Mode::Browse`]:
/// in a mode, the keys mean what the mode says and nothing else.
fn browse_key(k: Key, board: &Board, cur: &mut Cursor, mode: &mut Mode) -> Action {
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
            escalate: None,
            then: None,
            task: c.id.clone(),
        }
    };

    match k {
        // `K` as well as `q`, for the reason `Z` closes the tree it opened: a
        // page that opens with one key and closes with another is one you leave
        // open. It reads the same in both places this board is drawn — the tab
        // goes, or the sidebar takes its room back.
        Key::Char('q') | Key::Char('K') | Key::Esc | Key::Interrupt => Action::Quit,

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

        // ---- hand it over ----
        //
        // `c`, the panel's pick with the picking done here instead. There the
        // second act lands on a row of a full tree; here it lands on a lit rail
        // that holds nothing but the agents who could take the work, so the
        // arrows and ↵ are the whole question.
        //
        // Only spares are offered, and [`Board::spare`] has already made the
        // judgement — an idle agent parked on a blocked task is stopped, not
        // free, and an agent already holding work is `claim`'s refusal to make,
        // not this board's.
        //
        // One spare still walks through the rail rather than handing over on
        // the spot: `c` is one key, and starting an agent on the wrong task
        // costs a context window. The ↵ is where you look before you commit,
        // and looking is cheap exactly once.
        Key::Char('c') => match card {
            None => Action::Say("nothing here to hand over".into()),
            Some(c) if board.spare().is_empty() => {
                Action::Say(format!("nobody is spare · wsp spawn {} starts one", c.id))
            }
            Some(c) => {
                *mode = Mode::Hand { task: c.id.clone(), sel: 0 };
                Action::None
            }
        },

        // ---- what comes first in this column ----
        //
        // The board is the one place the answer is visible: priority orders a
        // column and nothing else, so cycling it here moves the card up or down
        // in front of you.
        Key::Char('!') => match card {
            Some(c) => Action::Run {
                argv: vec!["prio".into(), c.id.clone(), c.priority.cycled().as_str().into()],
                escalate: None,
                then: None,
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

/// The key, while the spare rail is lit.
///
/// Everything else stands down: the cursor may not move while picking (the
/// footer names the deed by id, and moving it would be two questions under one
/// pair of arrows), and every other verb waits until this one is answered.
/// `esc` walking away has to be reachable, and it is — along with the two keys
/// (`q`, `K`) that close the board itself, which here stand down the mode
/// first, because a mode you opened by accident should not cost the board.
fn hand_key(k: Key, board: &Board, mode: &mut Mode) -> Action {
    // Small values copied out, so the arms can reset the mode without fighting
    // a borrow that is still reading it.
    let Mode::Hand { task, sel } = mode else { return Action::None };
    let (task, sel) = (task.clone(), *sel);
    // Read per keypress, not once at `c`: a rebuild between the two keys can
    // have changed who is free, and the deed must answer for the rail as it is
    // now, not as it was lit.
    let spares = board.spare();
    match k {
        Key::Left | Key::Char('h') => {
            if let Mode::Hand { sel, .. } = mode {
                *sel = sel.saturating_sub(1);
            }
            Action::None
        }
        Key::Right | Key::Char('l') => {
            if sel + 1 < spares.len() {
                if let Mode::Hand { sel, .. } = mode {
                    *sel += 1;
                }
            }
            Action::None
        }
        // The confirm step, whatever the size of the rail — see the `c` above.
        // The sentence is worked out here, while both ends of the pick are
        // still in hand: past this return, the agent is just a pane id in an
        // argv.
        //
        // [`crate::panel::pick_tell`] guards its sentence on `agent && idle`;
        // this rail needs no second copy of that test, because [`Board::spare`]
        // has already applied it — spare *is* stopped-with-nothing-in-hand —
        // and a stale census between the two keystrokes is the same race the
        // panel accepts between its own read and the typing.
        Key::Enter => {
            let Some(a) = spares.get(sel).or_else(|| spares.last()) else {
                *mode = Mode::Browse;
                return Action::Say("nobody is spare any more".into());
            };
            let argv = vec!["claim".to_string(), task.clone(), "--pane".into(), a.pane.clone()];
            let mut forced = argv.clone();
            forced.push("--force".into());
            let then = crate::panel::tell_claimed(&a.who, &task);
            *mode = Mode::Browse;
            Action::Run { argv, escalate: Some(forced), then: Some(then), task }
        }
        Key::Esc | Key::Interrupt | Key::Char('q') | Key::Char('K') => {
            *mode = Mode::Browse;
            Action::Say("walked away".into())
        }
        _ => Action::None,
    }
}

/// The key, while a refusal is being offered its stronger form.
///
/// The panel's own y/n, aimed at the same command: `y` runs it, everything else
/// that means no leaves the work exactly as it was. `↵` counts as no, not yes —
/// it is the busiest key on either surface, and the one slip most likely to
/// answer a question nobody had finished reading.
fn confirm_key(k: Key, mode: &mut Mode) -> Action {
    match k {
        Key::Char('y') | Key::Char('Y') => {
            let Mode::Confirm { argv, then, task, .. } = std::mem::take(mode) else {
                return Action::None;
            };
            Action::Run { argv, escalate: None, then, task }
        }
        Key::Char('n') | Key::Char('N') | Key::Esc | Key::Interrupt | Key::Enter => {
            *mode = Mode::Browse;
            Action::Say("left alone".into())
        }
        _ => Action::None,
    }
}

pub(crate) fn apply_key(k: Key, board: &Board, cur: &mut Cursor, mode: &mut Mode) -> Action {
    match mode {
        Mode::Hand { .. } => return hand_key(k, board, mode),
        Mode::Confirm { .. } => return confirm_key(k, mode),
        Mode::Browse => {}
    }
    browse_key(k, board, cur, mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::{collect, Ctx, Scope};
    use crate::live::AgentRef;
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

    /// A census small enough to reason about: one agent working, one idle on a
    /// blocked task, and whoever else the test asks for. Only the last group
    /// ever reaches [`Board::spare`], which is the point.
    fn board_with_census(spec: &[(&str, &str, &str)], spares: &[(&str, &str)]) -> Board {
        let mut tasks: Vec<Task> = spec
            .iter()
            .map(|(id, status, prio)| {
                let mut t = Task::new("a title", id);
                t.project = Some("wsp".into());
                t.status_raw = (*status).into();
                t.priority_raw = (*prio).into();
                t
            })
            .collect();
        // What the standing pair are holding: work moving under the busy one,
        // a blocked question under the idle one. Both must exist for the join
        // to read them as anything but spare.
        let mut held = Task::new("held", "t-hold");
        held.project = Some("wsp".into());
        held.status_raw = "doing".into();
        let mut stuck = Task::new("stopped", "t-stuck");
        stuck.project = Some("wsp".into());
        stuck.status_raw = "blocked".into();
        tasks.push(held);
        tasks.push(stuck);

        let pane = |id: &str, state: &str, name: &str| AgentRef {
            pane: id.into(),
            workspace: "w9".into(),
            agent: true,
            kind: "claude".into(),
            state: state.into(),
            title: name.into(),
            ..Default::default()
        };
        let mut panes = vec![pane("w9:p1", "working", "busy"), pane("w9:p2", "idle", "stopped")];
        for (id, name) in spares {
            panes.push(pane(id, "idle", name));
        }
        let mut bindings = BTreeMap::new();
        bindings.insert("w9:p1".to_string(), serde_json::json!({ "task_id": "t-hold" }));
        bindings.insert("w9:p2".to_string(), serde_json::json!({ "task_id": "t-stuck" }));

        let ctx = Ctx {
            tasks,
            index: Index::new(vec![crate::model::Project::new("wsp")]),
            bindings,
            claims: BTreeMap::new(),
            panes,
        };
        collect(&ctx, &Scope::Project("wsp".into()), true)
    }

    /// The loop's shape in miniature: cursor and mode beside each other, keys
    /// through the same door the live boards use.
    struct Drive<'a> {
        board: &'a Board,
        cur: Cursor,
        mode: Mode,
    }

    impl<'a> Drive<'a> {
        fn new(board: &'a Board) -> Drive<'a> {
            Drive { board, cur: Cursor::default(), mode: Mode::default() }
        }

        fn press(&mut self, k: Key) -> Action {
            apply_key(k, self.board, &mut self.cur, &mut self.mode)
        }
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

    /// One key for one idea, in both places the board is drawn. `Z` closes the
    /// tree it opened; this closes the board it opened, whether that means a
    /// tab going or a sidebar taking its room back — and a page that opens with
    /// one key and closes with another is a page you leave open.
    #[test]
    fn the_key_that_opens_a_board_is_also_the_key_that_closes_it() {
        let b = board(&[("t-01", "todo", "normal")], true);
        for k in [Key::Char('K'), Key::Char('q'), Key::Esc] {
            let mut d = Drive::new(&b);
            assert!(
                matches!(d.press(k), Action::Quit),
                "{k:?} should stand the board down",
            );
        }
    }

    /// A column that empties under the cursor is ordinary — it is what
    /// finishing the last card in it looks like — and the cursor has to land
    /// somewhere that exists afterwards.
    ///
    /// Shared by the board's own pane and the board drawn in place of the tree,
    /// which is why it is a method rather than a copy beside each loop. A
    /// cursor left past the end reads no card, and every verb on this page is
    /// about the card under it: the board would go quiet rather than wrong.
    #[test]
    fn a_cursor_left_past_the_end_comes_back_to_a_card_that_is_there() {
        let full = board(
            &[("t-01", "todo", "normal"), ("t-02", "todo", "normal"), ("t-03", "doing", "normal")],
            true,
        );
        let mut d = Drive::new(&full);
        d.press(Key::Char('j'));
        assert_eq!(d.cur, Cursor { col: 0, row: 1 });

        // The card it was on has been finished, and `todo` is one shorter.
        let after = board(&[("t-01", "todo", "normal"), ("t-03", "doing", "normal")], true);
        assert_eq!(d.cur.clamped(&after), Cursor { col: 0, row: 0 });

        // And the column that goes when `done` is put away takes the cursor
        // back with it rather than leaving it off the right-hand edge, where
        // every key would be aimed at a column that is not drawn.
        let narrow = board(&[("t-01", "todo", "normal")], false);
        assert_eq!(narrow.columns.len(), 3);
        assert_eq!(Cursor { col: 3, row: 0 }.clamped(&narrow), Cursor { col: 2, row: 0 });
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
        let mut d = Drive::new(&b);
        d.press(Key::Char('j'));
        d.press(Key::Char('j'));
        assert_eq!(d.cur, Cursor { col: 0, row: 2 });

        // One card in `doing`, so the row has to come back to it.
        d.press(Key::Char('l'));
        assert_eq!(d.cur, Cursor { col: 1, row: 0 });
        // And an empty column takes the cursor rather than refusing it: you
        // have to be able to see you are there.
        d.press(Key::Char('l'));
        assert_eq!(d.cur, Cursor { col: 2, row: 0 });
        // The far edge holds.
        d.press(Key::Char('l'));
        d.press(Key::Char('l'));
        assert_eq!(d.cur.col, 3);
    }

    /// The whole of what a board is for: a card, pushed along. `>` and the
    /// named verb reach the same command, because there is one set of rules
    /// about what `done` means and it lives in the CLI.
    #[test]
    fn a_card_is_pushed_along_by_the_same_verbs_that_name_the_lanes() {
        let b = board(&[("t-01", "todo", "normal")], true);
        let mut d = Drive::new(&b);
        assert_eq!(argv_of(d.press(Key::Char('>'))), ["start", "t-01"]);
        assert_eq!(argv_of(d.press(Key::Char('s'))), ["start", "t-01"]);
        assert_eq!(argv_of(d.press(Key::Char('d'))), ["done", "t-01"]);
        assert_eq!(argv_of(d.press(Key::Char('v'))), ["review", "t-01"]);
        // Already there is not a command. It would be a log line, an event and
        // a commit recording a keypress.
        assert_eq!(said(d.press(Key::Char('o'))), "already in todo");
        // And the near end has nowhere to go back to.
        assert_eq!(said(d.press(Key::Char('<'))), "todo is the near end");
    }

    /// A blocked card is in the doing column, so pushing it on means `review` —
    /// the column it is in, not the status it carries, is what "along" means.
    #[test]
    fn a_blocked_card_moves_from_the_column_it_is_drawn_in() {
        let b = board(&[("t-01", "blocked", "normal")], true);
        let mut d = Drive::new(&b);
        d.cur = Cursor { col: 1, row: 0 };
        assert_eq!(argv_of(d.press(Key::Char('>'))), ["review", "t-01"]);
        assert_eq!(argv_of(d.press(Key::Char('<'))), ["reopen", "t-01"]);
    }

    /// Every verb has to survive being aimed at nothing. An empty column is the
    /// ordinary state of a board — that is what an empty column means — and a
    /// key that panicked there would take the pane with it.
    #[test]
    fn a_verb_aimed_at_an_empty_column_says_so_and_does_nothing() {
        let b = board(&[("t-01", "todo", "normal")], true);
        let mut d = Drive::new(&b);
        d.cur = Cursor { col: 2, row: 0 };
        for k in ['s', 'v', 'd', 'o', '!', 'E', '>', '<', 'c'] {
            assert!(
                matches!(d.press(Key::Char(k)), Action::Say(_)),
                "{k} should refuse rather than act",
            );
        }
        assert!(matches!(d.press(Key::Enter), Action::Say(_)));
    }

    /// The board is a view and holds nothing of its own, so `↵` is a way out of
    /// it as much as a way into the task: the panel shows the task, and the
    /// board — which was only ever the project drawn by state — stands down.
    #[test]
    fn opening_a_card_is_the_board_handing_over_and_going() {
        let b = board(&[("t-01", "todo", "normal")], true);
        let mut d = Drive::new(&b);
        match d.press(Key::Enter) {
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
        let mut d = Drive::new(&b);
        assert_eq!(argv_of(d.press(Key::Char('!'))), ["prio", "t-01", "high"]);
        let b = board(&[("t-01", "todo", "high")], true);
        let mut d = Drive::new(&b);
        assert_eq!(argv_of(d.press(Key::Char('!'))), ["prio", "t-01", "low"]);
        let b = board(&[("t-01", "todo", "low")], true);
        let mut d = Drive::new(&b);
        assert_eq!(argv_of(d.press(Key::Char('!'))), ["prio", "t-01", "normal"]);
    }

    /// With `done` put away there are three columns, and the digit that named
    /// the fourth has to answer rather than move the cursor off the board.
    #[test]
    fn a_digit_cannot_reach_a_column_that_is_not_showing() {
        let b = board(&[("t-01", "done", "normal")], false);
        let mut d = Drive::new(&b);
        assert_eq!(said(d.press(Key::Char('4'))), "that column is not showing");
        assert_eq!(d.cur, Cursor::default());
    }

    /// The whole of `c`, end to end at the keys: the rail offers only who is
    /// free, ↵ turns the choice into `claim --pane`, and the sentence rides
    /// behind it — because a claim nobody tells the agent about leaves an idle
    /// agent sitting on work it now holds.
    #[test]
    fn handing_a_card_over_claims_it_on_the_chosen_pane_and_tells_them() {
        // Two spares named so their rail order is known: Jolt sorts first.
        let b = board_with_census(
            &[("t-01", "todo", "normal")],
            &[("w2:p1", "Jolt"), ("w3:p1", "Verb UI")],
        );
        let mut d = Drive::new(&b);

        // `c` itself runs nothing — even with a rail under it, the deed waits
        // for ↵, because starting an agent on the wrong task costs a context
        // window and looking is cheap exactly once.
        assert!(matches!(d.press(Key::Char('c')), Action::None));
        assert_eq!(
            d.mode,
            Mode::Hand { task: "t-01".into(), sel: 0 },
            "the card being handed is named, not pointed at",
        );
        assert!(matches!(d.press(Key::Right), Action::None));
        assert!(matches!(d.mode, Mode::Hand { sel: 1, .. }), "the arrows move the light");

        match d.press(Key::Enter) {
            Action::Run { argv, escalate, then, task } => {
                assert_eq!(argv, ["claim", "t-01", "--pane", "w3:p1"]);
                // The stronger form travels with the first attempt: a refusal
                // from `claim` becomes the next question rather than a rule the
                // board keeps a second copy of.
                assert_eq!(
                    escalate,
                    Some(vec![
                        "claim".into(),
                        "t-01".into(),
                        "--pane".into(),
                        "w3:p1".into(),
                        "--force".into()
                    ])
                );
                // …and the sentence is already composed, so a claim that works
                // is never one nobody was told about.
                let tell = then.expect("a claim goes with a sentence");
                assert!(tell.text.expect("the work order").contains("claimed onto t-01"));
                assert!(tell.note.contains("→ t-01"), "{}", tell.note);
                assert_eq!(tell.pane, "w3:p1");
                assert_eq!(task, "t-01", "the cursor follows the card the command moved");
            }
            _ => panic!("↵ on a lit rail hands the card over"),
        }
        // And the mode stood down behind the deed: the next key is an ordinary
        // one again.
        assert_eq!(d.mode, Mode::Browse);
    }

    /// One spare still walks through the rail. `c` is one key; starting an
    /// agent on the wrong card costs a context window, and the ↵ is the one
    /// place you look before it happens.
    #[test]
    fn a_lone_spare_still_waits_for_enter() {
        let b = board_with_census(&[("t-01", "todo", "normal")], &[("w2:p1", "Jolt")]);
        let mut d = Drive::new(&b);
        assert!(matches!(d.press(Key::Char('c')), Action::None));
        assert!(matches!(d.mode, Mode::Hand { sel: 0, .. }));

        // The arrows hold at the ends of a one-agent rail.
        d.press(Key::Left);
        assert!(matches!(d.mode, Mode::Hand { sel: 0, .. }));
    }

    /// Nobody free is a sentence, not a mode. Both reasons have their own word
    /// downstream — everyone busy, or nobody there at all — but on the board
    /// they end the same way: no rail, no pick, and a pointer at the verb that
    /// would make some.
    #[test]
    fn c_with_nobody_free_says_so_instead_of_opening_an_empty_rail() {
        // Everyone accounted for: working, or idle *on* something.
        let b = board_with_census(&[("t-01", "todo", "normal")], &[]);
        let mut d = Drive::new(&b);
        let m = said(d.press(Key::Char('c')));
        assert!(m.contains("nobody is spare"), "{m}");
        assert!(m.contains("spawn"), "{m} should say how to make an agent");
        assert_eq!(d.mode, Mode::Browse);
    }

    /// Picking owns the keyboard, except for the ways out. Movement would put
    /// a different card under the dock while the footer names the one being
    /// handed, and `q` standing the whole board down over a mode entered by
    /// accident would be a steep price for a stray keystroke — so it stands
    /// the mode down instead, exactly as `esc` does.
    #[test]
    fn while_picking_everything_waits_except_the_way_out() {
        let b = board_with_census(
            &[("t-01", "todo", "normal"), ("t-02", "todo", "normal")],
            &[("w2:p1", "Jolt")],
        );
        let mut d = Drive::new(&b);
        d.press(Key::Char('c'));

        // No movement, no verbs, no digits: the deed is named in the footer and
        // the cursor staying still is part of that bargain.
        for k in [Key::Down, Key::Up, Key::Char('j'), Key::Char('k'), Key::Char('1'), Key::Char('s')] {
            assert!(matches!(d.press(k), Action::None), "{k:?} should wait");
        }
        assert_eq!(d.cur, Cursor::default());
        assert!(matches!(d.press(Key::Char('q')), Action::Say(_)));
        assert_eq!(d.mode, Mode::Browse, "q stands the pick down before it stands the board down");
    }

    /// And `esc` walks away without handing anything over.
    #[test]
    fn esc_walks_away_from_the_rail_and_leaves_the_work_alone() {
        let b = board_with_census(&[("t-01", "todo", "normal")], &[("w2:p1", "Jolt")]);
        let mut d = Drive::new(&b);
        d.press(Key::Char('c'));
        assert_eq!(said(d.press(Key::Esc)), "walked away");
        assert_eq!(d.mode, Mode::Browse);
    }

    /// The refusal half. The board never decides whether a claim should be
    /// forced — `claim` says why it said no, and that sentence comes back as
    /// the question `y` answers by running the stronger form. `n` leaves
    /// everything as it was, and so does `↵`, which is the busiest key on
    /// either surface and the likeliest slip.
    #[test]
    fn a_refused_claim_comes_back_as_the_question_that_force_answers() {
        let b = board(&[("t-01", "todo", "normal")], true);
        let mut d = Drive::new(&b);
        // What the loop puts up when the CLI refuses: its words, and the same
        // command carrying --force.
        d.mode = Mode::Confirm {
            question: "already done — claiming it would reopen it".into(),
            argv: vec!["claim".into(), "t-01".into(), "--pane".into(), "w2:p1".into(), "--force".into()],
            then: Some(crate::panel::tell_claimed(
                &AgentRef {
                    pane: "w2:p1".into(),
                    agent: true,
                    kind: "claude".into(),
                    state: "idle".into(),
                    ..Default::default()
                },
                "t-01",
            )),
            task: "t-01".into(),
        };

        // Anything that is not yes is no.
        for k in [Key::Char('n'), Key::Char('N'), Key::Esc, Key::Enter] {
            let mut probe = Drive::new(&b);
            probe.mode = d.mode.clone();
            assert_eq!(said(probe.press(k)), "left alone");
            assert_eq!(probe.mode, Mode::Browse);
        }

        let mut d = Drive::new(&b);
        d.mode = Mode::Confirm {
            question: "refused".into(),
            argv: vec!["claim".into(), "t-01".into(), "--force".into()],
            then: None,
            task: "t-01".into(),
        };
        match d.press(Key::Char('y')) {
            Action::Run { argv, escalate, task, .. } => {
                assert_eq!(argv, ["claim", "t-01", "--force"]);
                // Nothing stronger above the force: the question was the last
                // stop.
                assert_eq!(escalate, None);
                assert_eq!(task, "t-01", "the cursor follows the card the command moved");
            }
            _ => panic!("y runs it"),
        }
        assert_eq!(d.mode, Mode::Browse);
    }

    /// A rebuild between `c` and `↵` can empty the rail — every spare picked
    /// work up elsewhere. The pick stands down rather than aiming at a name
    /// that is no longer drawn.
    #[test]
    fn enter_on_a_rail_that_has_gone_empty_stands_down_instead() {
        let b = board_with_census(&[("t-01", "todo", "normal")], &[("w2:p1", "Jolt")]);
        let mut d = Drive::new(&b);
        d.press(Key::Char('c'));
        // The world moved: the census the mode was lit against is not the one
        // answering now.
        let empty = board_with_census(&[("t-01", "todo", "normal")], &[]);
        d.board = &empty;
        assert_eq!(said(d.press(Key::Enter)), "nobody is spare any more");
        assert_eq!(d.mode, Mode::Browse);
    }
}
