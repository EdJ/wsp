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
#[derive(Default)]
pub(crate) struct View {
    /// Projects whose children and tasks are hidden.
    pub(super) collapsed: HashSet<String>,
    /// Projects (or the inbox) showing past `MAX_TASKS_PER_PROJECT`.
    pub(super) expanded: HashSet<String>,
    /// Include `done` tasks, and the projects that hold only those.
    pub(super) show_done: bool,
    /// Put each task's id in front of its title. Off by default: the tree is
    /// for reading, and thirteen characters of id on every row is most of a
    /// narrow pane. On when you are about to type one at a shell.
    pub(super) ids: bool,
    /// Projects to show even though they hold nothing yet. A project created
    /// from the panel is empty by definition, so the quiet-branch filter would
    /// swallow it the instant it was made — you would type a name, press
    /// return, and watch nothing appear.
    pub(super) reveal: HashSet<String>,
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
pub(crate) fn apply_key(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
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
}
