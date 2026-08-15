//! Keys, modes, and what the panel is currently in the middle of.
//!
//! A key arrives as it was typed and is given meaning here, by a reducer that
//! knows the mode — `j` means "down" while browsing and means `j` while typing
//! a title, and only something holding [`Mode`] can tell those apart.
//!
//! [`apply_key`] is pure: state in, [`Effect`] out. Nothing in this file talks
//! to a terminal or to herdr, which is what lets the storyboard push scripted
//! keys through the same path the live panel runs.

use std::collections::HashSet;
use std::time::Instant;

use crate::input::Key;
use crate::util;

use super::rows::{AgentRef, Row, Ui};
use super::verbs::{browse_key, Ask, Pick};

/// What the viewer has folded, unfolded, or asked to see more of. Held by the
/// event loop and handed to `collect`, which is otherwise a pure function of
/// the store plus herdr.
impl View {
    /// The key map changes how many rows the tree gets, which changes where a
    /// click lands. Test-only, so a sweep can check both.
    #[cfg(test)]
    pub(crate) fn set_help_for_test(&mut self, on: bool) {
        self.help = on;
    }
}

#[derive(Default, Clone)]
pub(crate) struct View {
    /// Projects whose children and tasks are hidden.
    pub(super) collapsed: HashSet<String>,
    /// Projects (or the inbox) showing past `MAX_TASKS_PER_PROJECT`.
    pub(super) expanded: HashSet<String>,
    /// Include `done` tasks, and the projects that hold only those.
    pub(super) show_done: bool,
    /// Narrow the tree to work at `review` — what an agent has finished with
    /// and handed back. Every key goes on meaning what it means; the only
    /// thing that changes is which rows are there to aim them at.
    pub(super) review_only: bool,
    /// Put each task's id in front of its title. Off by default: the tree is
    /// for reading, and thirteen characters of id on every row is most of a
    /// narrow pane. On when you are about to type one at a shell.
    pub(super) ids: bool,
    /// Projects to show even though they hold nothing yet. A project created
    /// from the panel is empty by definition, so the quiet-branch filter would
    /// swallow it the instant it was made — you would type a name, press
    /// return, and watch nothing appear.
    pub(super) reveal: HashSet<String>,
    /// A scroll offset the *pointer* set, if it has. The tree normally scrolls
    /// by holding the cursor near the middle, which is right for a keyboard
    /// and wrong for a mouse: selecting a row would recentre the tree and
    /// slide that row out from under the pointer, so the second click of
    /// select-then-activate landed on whatever moved into its place.
    ///
    /// So the pointer drives the view directly and the keyboard goes on
    /// centring — any keystroke clears this and hands the view back.
    pub(super) scroll: Option<usize>,
    /// What the next keypress means.
    pub(crate) mode: Mode,
    /// What the detail pane is currently showing, so `↵` can close it.
    pub(super) showing: Option<crate::detail::Focus>,
    /// A row to select on the next rebuild — set when something is created, so
    /// the cursor follows what you just made.
    pub(super) land_on: Option<String>,
    /// The key map, docked under the tree. A line in the footer could hold four
    /// of the twenty keys, which is worse than useless: it says there is a list
    /// and then shows you a fifth of it. It takes the rows it needs and no
    /// more, because you read it to press one of the keys in it — and the row
    /// you would press it on has to still be there, and still be selected.
    pub(super) help: bool,
}

/// Management needs three shapes of input beyond a single key: a value to
/// type, a second row to point at, and a yes before something irreversible.
/// Each is a mode rather than a widget, so the cursor and the tree keep
/// working inside them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Browse,
    Prompt {
        verb: Ask,
        buffer: String,
    },
    /// Navigation still moves the cursor; `↵` takes whatever it lands on.
    Pick {
        verb: Pick,
    },
    Confirm {
        argv: Vec<String>,
        question: String,
        /// What to offer if `argv` is refused. The panel does not decide on
        /// your behalf that a refusal should be overridden — it puts the
        /// refusal on screen and asks again.
        escalate: Option<Vec<String>>,
    },
}


/// Every key the panel answers to. Keys that do the same kind of thing share a
/// line — `s v` is one idea, not two — because the map is read in a column
/// thirty-four wide, and length is what pushes the tree off the screen.
///
/// The verbs come first. Movement is the half you can find by pressing an arrow
/// and watching; `X` is not. So when a short pane can only fit part of this,
/// what it fits is the part that cannot be guessed.
///
/// Descriptions are written to fit that column. Anything longer is a sentence,
/// and a sentence belongs on the row it describes, not here.
pub(crate) fn keymap() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            "change",
            vec![
                ("a P", "add task, project"),
                ("s v", "start, review"),
                ("d o", "done, reopen"),
                ("b e n", "block, retitle, note"),
                ("m c", "move, claim"),
                ("O", "open a workspace"),
                ("X", "remove, after y/n"),
            ],
        ),
        (
            "look",
            vec![
                ("↵ esc", "open it, close it"),
                ("E", "edit in a tab"),
                ("A i r", "show done, ids, sync"),
                ("R", "only what needs review"),
                ("q", "quit"),
            ],
        ),
        (
            "move",
            vec![
                ("j k ↑ ↓", "up, down"),
                ("h l ← →", "fold, unfold"),
                ("g G ⇱ ⇲", "first, last row"),
                ("1-9", "jump to a terminal"),
            ],
        ),
    ]
}

/// What a key asked for beyond changing the view.
pub(crate) enum Effect {
    None,
    Refetch,
    Focus(AgentRef),
    Sync,
    Quit,
    /// Open a herdr workspace for this row, then claim the task into it if
    /// there is one.
    Open { label: String, cwd: Option<String>, project: Option<String>, task: Option<String> },
    /// Show this row in the detail pane, making one if there is not one yet.
    Inspect(crate::detail::Focus),
    /// Shut the detail pane.
    CloseView,
    /// Open the row full-size in a tab of its own, to be written in.
    PopOut { argv: Vec<String>, label: String },
    /// Argv for this binary. Running the CLI rather than reimplementing it
    /// means the event log, the hooks and the git commit all still happen,
    /// because it is the same code path a person at a shell would take.
    Run { argv: Vec<String>, escalate: Option<Vec<String>> },
}

pub(super) fn say(ui: &mut Ui, m: impl Into<String>) {
    ui.message = Some((m.into(), Instant::now()));
}

/// Typing a value. Every printable key is text here — including `q`, which is
/// why the input layer stopped deciding what keys mean.
pub(super) fn prompt_key(k: Key, ui: &mut Ui, view: &mut View, verb: Ask, mut buffer: String) -> Effect {
    match k {
        Key::Char(c) => {
            buffer.push(c);
            view.mode = Mode::Prompt { verb, buffer };
            Effect::None
        }
        Key::Backspace => {
            buffer.pop();
            view.mode = Mode::Prompt { verb, buffer };
            Effect::None
        }
        Key::Esc | Key::Interrupt => {
            view.mode = Mode::Browse;
            say(ui, "cancelled");
            Effect::None
        }
        Key::Enter => {
            if buffer.trim().is_empty() {
                view.mode = Mode::Browse;
                say(ui, "nothing typed");
                return Effect::None;
            }
            let argv = verb.argv(&buffer);
            if let Ask::NewProject { .. } = &verb {
                // The CLI slugifies the name; do the same so we know what to
                // keep visible without having to read the result back.
                view.reveal.insert(util::slugify(&buffer));
            }
            view.mode = Mode::Browse;
            Effect::Run { argv, escalate: None }
        }
        _ => Effect::None,
    }
}

/// Pointing at a second row. Navigation is untouched, so the tree itself is
/// the picker — no separate list to build, and folding still works while you
/// hunt for the project you want.
pub(super) fn pick_key(k: Key, ui: &mut Ui, view: &mut View, verb: Pick) -> Effect {
    match k {
        Key::Esc | Key::Interrupt => {
            view.mode = Mode::Browse;
            say(ui, "cancelled");
            Effect::None
        }
        Key::Enter => match verb.argv(&ui.selected_target()) {
            Some(argv) => {
                view.mode = Mode::Browse;
                Effect::Run { argv, escalate: None }
            }
            None => {
                say(ui, "not a valid destination");
                Effect::None
            }
        },
        // Movement and folding stay live inside the pick.
        Key::Up | Key::Down | Key::Left | Key::Right | Key::Char('j') | Key::Char('k')
        | Key::Char('h') | Key::Char('l') => {
            let nav = match k {
                Key::Char('j') => Key::Down,
                Key::Char('k') => Key::Up,
                Key::Char('h') => Key::Left,
                Key::Char('l') => Key::Right,
                other => other,
            };
            move_or_fold(nav, ui, view)
        }
        _ => Effect::None,
    }
}

pub(super) fn confirm_key(
    k: Key,
    ui: &mut Ui,
    view: &mut View,
    argv: Vec<String>,
    escalate: Option<Vec<String>>,
) -> Effect {
    match k {
        Key::Char('y') | Key::Char('Y') => {
            view.mode = Mode::Browse;
            Effect::Run { argv, escalate }
        }
        Key::Char('n') | Key::Char('N') | Key::Esc | Key::Interrupt | Key::Enter => {
            view.mode = Mode::Browse;
            say(ui, "left alone");
            Effect::None
        }
        _ => Effect::None,
    }
}

/// Cursor movement and folding, shared by browse and pick.
pub(super) fn move_or_fold(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
    let n = ui.rows.len();
    match k {
        Key::Down => {
            if ui.sel + 1 < n {
                ui.sel += 1;
            }
            Effect::None
        }
        Key::Up => {
            ui.sel = ui.sel.saturating_sub(1);
            Effect::None
        }
        Key::Left | Key::Right => match ui.rows.get(ui.sel) {
            Some(Row::Project { id: key, .. }) | Some(Row::Section { key, .. }) => {
                if k == Key::Left {
                    view.collapsed.insert(key.clone());
                    view.expanded.remove(key);
                } else {
                    view.collapsed.remove(key);
                }
                Effect::Refetch
            }
            Some(Row::More { key, .. }) if k == Key::Right => {
                view.expanded.insert(key.clone());
                Effect::Refetch
            }
            _ => Effect::None,
        },
        _ => Effect::None,
    }
}

/// The reducer. Deliberately free of I/O — it moves the cursor, changes the
/// mode, and reports what else it wants done, so the storyboard can drive the
/// same transitions the terminal does and get the same frames out.
/// The wheel moves the selection, three rows at a time.
///
/// Deliberately the same thing `j`/`k` do, three times over — not a scroll
/// offset of its own. The tree scrolls by holding the cursor near the middle
/// of the pane, and that centring *is* the scrolling: move the cursor and the
/// view follows. An offset the pointer owned separately made the highlight
/// wander off the pane, which is a different feature nobody asked for.
pub(crate) fn wheel(ui: &mut Ui, view: &mut View, up: bool) {
    for _ in 0..3 {
        let _ = apply_key(if up { Key::Up } else { Key::Down }, ui, view);
    }
}

/// What a click at screen row `y` amounts to.
#[derive(Debug, PartialEq)]
pub(crate) enum Hit {
    /// Furniture — the title, a rule, the blank tail, the footer.
    Nothing,
    /// The cursor moved to that row, and the view was pinned so the row stays
    /// under the pointer.
    Select,
    /// A click on the row already under the cursor: what `↵` means.
    Activate,
}

/// Decide what a click does, and move the cursor if that is what it does.
///
/// Here rather than in the event loop so it can be tested without a terminal:
/// the loop's job is to read the pane's size and act on the answer, and the
/// interesting half — select, then activate, without the tree moving — is
/// policy that a fixture can drive.
pub(crate) fn click(ui: &mut Ui, view: &mut View, w: usize, h: usize, y: usize) -> Hit {
    let at = super::render::geometry(ui, view, w, h).scroll;
    match super::render::row_at(ui, view, w, h, y) {
        None => Hit::Nothing,
        Some(i) if i == ui.sel => Hit::Activate,
        Some(i) => {
            // Pin the view before moving the cursor. Selecting normally
            // recentres the tree, which would slide the row out from under the
            // pointer that chose it — and the second click of
            // select-then-activate would land on whatever replaced it.
            view.scroll = Some(at);
            ui.sel = i;
            Hit::Select
        }
    }
}

pub(crate) fn apply_key(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
    let sel_before = ui.sel;
    let effect = {
        // Taken rather than borrowed: every branch may replace it.
        match std::mem::take(&mut view.mode) {
            Mode::Browse => browse_key(k, ui, view),
            Mode::Prompt { verb, buffer } => {
                view.mode = Mode::Prompt { verb: verb.clone(), buffer: buffer.clone() };
                prompt_key(k, ui, view, verb, buffer)
            }
            Mode::Pick { verb } => {
                view.mode = Mode::Pick { verb: verb.clone() };
                pick_key(k, ui, view, verb)
            }
            Mode::Confirm { argv, question, escalate } => {
                view.mode = Mode::Confirm {
                    argv: argv.clone(),
                    question,
                    escalate: escalate.clone(),
                };
                confirm_key(k, ui, view, argv, escalate)
            }
        }
    };
    // The keyboard gives the view back to the cursor, but only once the cursor
    // has actually moved: a verb, or the `↵` a click turns into, must not
    // jerk the tree out from under the pointer that asked for it.
    if ui.sel != sel_before {
        view.scroll = None;
    }
    effect
}
