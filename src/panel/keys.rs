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
use super::verbs::{browse_key, pick_tell, Ask, Pick, Tell};

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

    /// The focus dock does the same, and worse: its height depends on the row
    /// the cursor is on, so it changes while a sweep is pressing `j`.
    #[cfg(test)]
    pub(crate) fn set_focus_for_test(&mut self, on: bool) {
        self.focus = on;
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
    /// Put the agents in place of the tree: every pane running an agent, in one
    /// list, ordered by what it is waiting for. The tree answers what the work
    /// is; this answers who is on it and which of them has stopped — the
    /// question herdr's own sidebar exists for, asked without leaving the panel.
    pub(super) agents: bool,
    /// Put each task's id in front of its title. Off by default: the tree is
    /// for reading, and thirteen characters of id on every row is most of a
    /// narrow pane. On when you are about to type one at a shell.
    pub(super) ids: bool,
    /// Projects to show even though they hold nothing yet. A project created
    /// from the panel is empty by definition, so the quiet-branch filter would
    /// swallow it the instant it was made — you would type a name, press
    /// return, and watch nothing appear.
    pub(super) reveal: HashSet<String>,
    /// Where the tree is scrolled to: the row drawn on its top line. `None`
    /// until a frame has placed it, which is the only moment anything derives
    /// it from the cursor.
    ///
    /// The view has a position of its own and keeps it. What moves it is the
    /// wheel, which sets it outright, and a cursor that would otherwise walk
    /// off the pane, which pushes it by as little as will do — see
    /// [`super::render::scroll_to`]. Written by the frame that drew it, so a
    /// click reads the offset the pane in front of you is actually using.
    pub(super) scroll: Option<usize>,
    /// The cursor is what last moved, rather than the view.
    ///
    /// This is which of the two the other one follows. With it set the view
    /// follows the cursor — kept on the pane, with rows beyond it, because a
    /// cursor on the last line shows you everything you have walked past and
    /// nothing you are about to reach. Cleared, the view is where the pointer
    /// put it and the cursor is left wherever it was: on the pane if it
    /// happens to be, off it if the wheel has gone past it, and selected
    /// either way.
    ///
    /// The pointer needs both halves of that. A click needs the row it landed
    /// on to stay exactly where it was, or the second click of
    /// select-then-activate lands on whatever slid into its place; the wheel
    /// needs the view to go where it said, rather than being hauled back to a
    /// selection the reader is deliberately looking away from.
    pub(super) keyed: bool,
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
    /// The selected row's title in full, docked above the footer. A row is one
    /// line wide and a title is not, so the tree names most work by its first
    /// twenty-five characters; reading the rest meant `↵`, which opens another
    /// pane and takes the cursor out of the tree. This reads it where you are
    /// and follows the cursor, so it is scrolling rather than looking things up.
    pub(super) focus: bool,
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
        /// Carried across the question, because a confirmed claim is still a
        /// claim: the agent has to be told either way. Dropping it here left
        /// the one case that most needs the sentence — work taken by force —
        /// as the one case with nobody told about it.
        then: Option<Tell>,
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
                ("m c f", "move, claim, find work"),
                ("O S", "a terminal, an agent"),
                ("X", "remove, after y/n"),
            ],
        ),
        (
            "look",
            vec![
                ("↵ esc", "open it, close it"),
                ("F", "the title in full, docked"),
                ("E", "edit in a tab"),
                ("A i r", "show done, ids, sync"),
                ("R", "only what needs review"),
                ("w", "the agents, not the work"),
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
    /// `wsp spawn` for this row: a workspace, the claim, and an agent in it if
    /// one was asked for.
    ///
    /// Argv rather than the pieces, for the same reason [`Effect::Run`] is
    /// argv: the CLI is the one implementation and the panel is a caller of it.
    /// Unlike `Run` it does not block the loop — starting an agent means
    /// waiting for `claude` to answer, which is seconds, and a sidebar that
    /// stops repainting while it happens reads as one that has crashed. So
    /// `note` is what the footer says *now*, and what the command has to say
    /// arrives later as a [`super::run::Msg::Note`].
    Spawn { argv: Vec<String>, note: String },
    /// Show this row in the detail pane, making one if there is not one yet.
    Inspect(crate::detail::Focus),
    /// Shut the detail pane.
    CloseView,
    /// Open the row full-size in a tab of its own, to be written in.
    PopOut { argv: Vec<String>, label: String },
    /// Argv for this binary. Running the CLI rather than reimplementing it
    /// means the event log, the hooks and the git commit all still happen,
    /// because it is the same code path a person at a shell would take.
    ///
    /// `then` is what to say to an agent once it has worked — a claim that
    /// nobody tells the agent about leaves it sitting idle on work it now
    /// holds. Withheld if the command is refused: the sentence would be a lie.
    Run { argv: Vec<String>, escalate: Option<Vec<String>>, then: Option<Tell> },
    /// Type into an agent's pane. The only effect that changes nothing at all
    /// in the store — what it changes is what an agent is about to do.
    Tell(Tell),
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
        Key::KillLine => {
            buffer.clear();
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
            // A prompt that opens holding a value can be left alone and sent
            // by the same key that sends a change. Running it would write the
            // title it already has: a log line, an event and a commit that
            // record a person pressing `↵` and nothing else.
            if let Ask::Rename { from, .. } = &verb {
                if buffer.trim() == from.trim() {
                    view.mode = Mode::Browse;
                    say(ui, "unchanged");
                    return Effect::None;
                }
            }
            let argv = verb.argv(&buffer);
            if let Ask::NewProject { .. } = &verb {
                // The CLI slugifies the name; do the same so we know what to
                // keep visible without having to read the result back.
                view.reveal.insert(util::slugify(&buffer));
            }
            view.mode = Mode::Browse;
            Effect::Run { argv, escalate: None, then: None }
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
                // Worked out here, while both ends of the pick are still in
                // hand: once this returns, the pane it named is just a string
                // in an argv.
                let then = pick_tell(&verb, &ui.selected_target(), ui);
                let escalate = verb.escalate(&argv);
                view.mode = Mode::Browse;
                Effect::Run { argv, escalate, then }
            }
            // Not a destination, but not nothing either: a `⋯`, or a branch
            // that is folded, is a row whose whole purpose is to be opened —
            // and what it opens onto is what the pick is hunting for. Refusing
            // there put the tail of every project past the six-task cap out of
            // reach of a claim, because `↵` was the pick's own key and the tail
            // has no other one.
            None if opens(ui.rows.get(ui.sel)) => move_or_fold(Key::Right, ui, view),
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
    then: Option<Tell>,
) -> Effect {
    match k {
        Key::Char('y') | Key::Char('Y') => {
            view.mode = Mode::Browse;
            Effect::Run { argv, escalate, then }
        }
        Key::Char('n') | Key::Char('N') | Key::Esc | Key::Interrupt | Key::Enter => {
            view.mode = Mode::Browse;
            say(ui, "left alone");
            Effect::None
        }
        _ => Effect::None,
    }
}

/// Whether `→` on this row would show something that is not showing: the
/// overflow row, or a branch that is folded. An open branch is deliberately not
/// one — `↵` means open, and making it fold as well would give the pick's own
/// key a second meaning that depends on state you are not looking at.
fn opens(row: Option<&Row>) -> bool {
    match row {
        Some(Row::More { .. }) => true,
        Some(Row::Project { collapsed, .. }) | Some(Row::Section { collapsed, .. }) => *collapsed,
        _ => false,
    }
}

/// The next row the cursor is allowed to sit on, in the direction of travel.
///
/// Most rows take the cursor; the lines under an agent in the agents view do
/// not, because they are the row above said at length and selecting one would
/// mean the same pane three times over. Stepping over them here is what keeps
/// `j` meaning "the next thing I can act on" rather than "the next line".
pub(super) fn step(rows: &[Row], from: usize, down: bool) -> usize {
    let mut i = from;
    loop {
        let next = if down {
            if i + 1 >= rows.len() {
                return from;
            }
            i + 1
        } else {
            if i == 0 {
                return from;
            }
            i - 1
        };
        if rows[next].selectable() {
            return next;
        }
        i = next;
    }
}

/// Cursor movement and folding, shared by browse and pick.
pub(super) fn move_or_fold(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
    match k {
        Key::Down => {
            ui.sel = step(&ui.rows, ui.sel, true);
            Effect::None
        }
        Key::Up => {
            ui.sel = step(&ui.rows, ui.sel, false);
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
/// The wheel moves the view, three rows at a time, and nothing else. The
/// selection is left where it is, on the pane or off it.
///
/// It used to be three of what `j` does, because the view had no position to
/// move — the cursor was the scroll. That is why scrolling back up did nothing
/// until the overshoot was walked off: the clamp stops the view at the last
/// screen while the cursor goes on to the foot, so a wheel-up had half a pane
/// of cursor travel to undo before anything moved. Here the wheel says where
/// the view goes and the view goes there.
pub(crate) fn wheel(ui: &mut Ui, view: &mut View, w: usize, h: usize, up: bool) {
    const STEP: usize = 3;
    let g = super::render::geometry(ui, view, w, h);
    let last = g.tree_len.saturating_sub(g.tree_rows);
    let to = if up { g.scroll.saturating_sub(STEP) } else { (g.scroll + STEP).min(last) };
    view.scroll = Some(to);
    // The cursor stays where it is, even when the view leaves it behind. What
    // is selected is a thing you decided, not a consequence of where you are
    // looking: dragging it along means a scroll to check something quietly
    // moves the row the next verb will act on, and you have no way of knowing
    // it did. Off the pane is a state the panel is allowed to be in — the next
    // keystroke is what brings the view back to it.
    view.keyed = false;
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
    /// A mark in the header strip. It is not a row and there is nothing to
    /// select — the mark *is* the agent, so pointing at it goes there. One
    /// click rather than the select-then-activate a row gets: the strip is a
    /// line of single columns, there is nothing to read on the way, and the ←
    /// you are reaching for is the one you have already decided to answer.
    Focus(AgentRef),
    /// The `⋯` at the end of a clipped strip: the rest of the agents, which is
    /// what the agents view is.
    Rest,
}

/// Decide what a click does, and move the cursor if that is what it does.
///
/// Here rather than in the event loop so it can be tested without a terminal:
/// the loop's job is to read the pane's size and act on the answer, and the
/// interesting half — select, then activate, without the tree moving — is
/// policy that a fixture can drive.
pub(crate) fn click(ui: &mut Ui, view: &mut View, w: usize, h: usize, x: usize, y: usize) -> Hit {
    // The top line is the strip, and a mark on it is an agent.
    if y == 0 {
        return match super::render::strip_at(ui, w, x) {
            Some(super::render::StripHit::Agent(i)) => {
                Hit::Focus(ui.census[i].1.clone())
            }
            Some(super::render::StripHit::Rest) => Hit::Rest,
            None => Hit::Nothing,
        };
    }
    let at = super::render::geometry(ui, view, w, h).scroll;
    match super::render::row_at(ui, view, w, h, y) {
        None => Hit::Nothing,
        // A line under an agent belongs to that agent: the click lands on the
        // row it is written beneath, which is the row it is about.
        Some(i) if !ui.rows[i].selectable() => {
            let owner = step(&ui.rows, i, false);
            if !ui.rows[owner].selectable() {
                return Hit::Nothing;
            }
            view.scroll = Some(at);
            view.keyed = false;
            ui.sel = owner;
            Hit::Select
        }
        Some(i) if i == ui.sel => Hit::Activate,
        Some(i) => {
            // The view is where the frame left it, and this says so rather
            // than assuming: a click is the one gesture that can arrive before
            // any frame has drawn. `keyed` is the part that matters — a
            // pointer is owed the row staying under it, so the cursor landing
            // two rows from the foot must not scroll the tree to make room
            // beyond it, which is what a keystroke landing there is owed.
            view.scroll = Some(at);
            view.keyed = false;
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
            Mode::Confirm { argv, question, escalate, then } => {
                view.mode = Mode::Confirm {
                    argv: argv.clone(),
                    question,
                    escalate: escalate.clone(),
                    then: then.clone(),
                };
                confirm_key(k, ui, view, argv, escalate, then)
            }
        }
    };
    // Only once the cursor has actually moved: a verb, or the `↵` a click
    // turns into, must not jerk the tree out from under the pointer that asked
    // for it. The view stays where it is either way — what this says is that
    // the next frame owes the cursor rows beyond it, because somebody is
    // reading in a direction rather than pointing at a row.
    if ui.sel != sel_before {
        view.keyed = true;
    }
    effect
}
