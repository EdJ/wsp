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
    /// Toggling a task's tags against the vocabulary the store already uses.
    ///
    /// The fourth shape, and it exists because the other three cannot do this
    /// one well. A prompt taking `+dsp -ui` can add and remove in one line, and
    /// still makes you *spell* every tag — including the one you want gone,
    /// which is a name the panel knows and you are being asked to remember. A
    /// pick points at one thing and ends; tagging is several toggles and then
    /// a decision.
    ///
    /// So: a list you can see, `␣` to flip a row, `↵` to apply the lot as one
    /// command and `esc` to walk away from all of it. Nothing is written until
    /// `↵` — which is what makes toggling safe to explore, and why a fumble
    /// that ends where it started costs no log line, no event and no commit.
    Tags(Tags),
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


/// What the tag picker is holding while it is up.
///
/// `list` is fixed the moment it opens and never reorders, because rows that
/// move under the cursor as you toggle them are how you take off the tag next
/// to the one you meant. The tags the task already had come first inside it —
/// they are what you came to remove — and the rest of the vocabulary follows
/// in the order it is used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tags {
    pub(super) task: String,
    /// What the task carried when this opened. The diff is against this, so
    /// toggling something on and off again is not a change.
    pub(super) was: Vec<String>,
    /// What reaches it from its project chain, and where each comes from.
    /// Drawn as carried — because it is — and never toggled: the only place
    /// one of these comes off is the project that holds it.
    pub(super) from_project: Vec<(String, String)>,
    /// What it will carry when this closes.
    pub(super) on: Vec<String>,
    /// Every tag on offer, in the order they are drawn.
    pub(super) list: Vec<String>,
    /// Narrows the list, and is also how a tag nobody has used yet gets its
    /// name: filter to something the list does not hold and the picker offers
    /// to make it. One line doing both, because they are the same gesture —
    /// you type what you want and take whichever of the two you are given.
    pub(super) filter: String,
    /// Which of the *filtered* rows the cursor is on.
    pub(super) sel: usize,
}

impl Tags {
    /// Open on a task: what it carries first — its own, then what its project
    /// lends it — and the rest of the vocabulary under that.
    ///
    /// Everything on the task is at the top whether or not it can be changed,
    /// because the first question a picker answers is "what has this got", and
    /// an answer that silently leaves out two thirds of what `wsp show` prints
    /// is not an answer.
    pub(super) fn new(
        task: &str,
        own: Vec<String>,
        from_project: Vec<(String, String)>,
        vocabulary: &[String],
    ) -> Tags {
        let mut list = own.clone();
        for t in from_project.iter().map(|(t, _)| t).chain(vocabulary.iter()) {
            if !list.contains(t) {
                list.push(t.clone());
            }
        }
        Tags {
            task: task.to_string(),
            was: own.clone(),
            on: own,
            from_project,
            list,
            filter: String::new(),
            sel: 0,
        }
    }

    /// Where a tag reaches this task from, when it is not the task's own.
    pub(super) fn lender(&self, tag: &str) -> Option<&str> {
        self.from_project.iter().find(|(t, _)| t == tag).map(|(_, p)| p.as_str())
    }

    /// The rows as drawn: the list narrowed to the filter, and — when the
    /// filter names something not in it — a row that would make that tag.
    ///
    /// Case-folded, and the new tag is offered lowercased: `wsp tag` takes the
    /// word as typed, so `DSP` and `dsp` would be two tags that read as one.
    pub(super) fn shown(&self) -> Vec<TagRow> {
        let f = self.filter.trim().to_ascii_lowercase();
        let mut out: Vec<TagRow> = self
            .list
            .iter()
            .filter(|t| f.is_empty() || t.to_ascii_lowercase().contains(&f))
            .map(|t| TagRow::Tag(t.clone()))
            .collect();
        if !f.is_empty() && !self.list.iter().any(|t| t.to_ascii_lowercase() == f) {
            out.push(TagRow::New(f));
        }
        out
    }

    /// Flip whatever the cursor is on. A new tag joins the list where the
    /// cursor already is, so it does not appear somewhere else on screen, and
    /// the filter clears because it has done its job.
    ///
    /// Answers what it did, so the caller can say why when it did nothing.
    pub(super) fn toggle(&mut self) -> Option<String> {
        match self.shown().get(self.sel).cloned() {
            Some(TagRow::Tag(t)) if self.lender(&t).is_some() && !self.was.contains(&t) => {
                let p = self.lender(&t).unwrap_or_default().to_string();
                return Some(format!("{t} comes from {p} · wsp project set {p} tags=…"));
            }
            Some(TagRow::Tag(t)) => {
                if let Some(i) = self.on.iter().position(|x| *x == t) {
                    self.on.remove(i);
                } else {
                    self.on.push(t);
                }
            }
            Some(TagRow::New(t)) => {
                self.list.push(t.clone());
                self.on.push(t.clone());
                self.filter.clear();
                self.sel =
                    self.shown().iter().position(|r| matches!(r, TagRow::Tag(x) if *x == t)).unwrap_or(0);
            }
            None => {}
        }
        None
    }

    /// What each row is: on, off, and the two that are about to change. Drawn
    /// apart because the whole bargain of this mode is that nothing is written
    /// until `↵`, and that is worth nothing if the frame will not say what `↵`
    /// is going to do.
    pub(super) fn state(&self, tag: &str) -> TagState {
        match (self.was.iter().any(|t| t == tag), self.on.iter().any(|t| t == tag)) {
            (true, true) => TagState::Kept,
            (false, true) => TagState::Adding,
            (true, false) => TagState::Removing,
            // Only once the task's own answer is no. A tag can be both — held
            // here *and* lent by the project — and then taking off the copy
            // this task owns still leaves the tag on it, which is worth being
            // shown rather than discovered.
            (false, false) => match self.lender(tag) {
                Some(p) => TagState::Inherited(p.to_string()),
                None => TagState::Off,
            },
        }
    }

    /// `+new -old`, net, in the order the list draws them. Empty when the
    /// picker was opened and closed without settling on anything different —
    /// and empty means no command at all, not a command that does nothing.
    pub(super) fn changes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in &self.list {
            match self.state(t) {
                TagState::Adding => out.push(format!("+{t}")),
                TagState::Removing => out.push(format!("-{t}")),
                _ => {}
            }
        }
        out
    }
}

/// A row of the picker: a tag on offer, or the offer to make one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TagRow {
    Tag(String),
    New(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TagState {
    /// On before, on after.
    Kept,
    /// Off before, on after.
    Adding,
    /// On before, off after.
    Removing,
    /// On the task, and not by this task's doing — it comes from the named
    /// project. `wsp tag` cannot touch it: `-rust` against a tag the project
    /// lends removes nothing and reports success.
    Inherited(String),
    Off,
}

/// Typing into the picker. Every printable key narrows the list — `q` included,
/// the same bargain the prompt makes — with one exception: a space, because a
/// tag with a space in it is not a tag, which leaves the key free to be the one
/// that flips a row.
pub(super) fn tags_key(k: Key, ui: &mut Ui, view: &mut View, mut t: Tags) -> Effect {
    match k {
        Key::Esc | Key::Interrupt => {
            view.mode = Mode::Browse;
            say(ui, "left alone");
            Effect::None
        }
        Key::Char(' ') => {
            if let Some(why) = t.toggle() {
                say(ui, why);
            }
            view.mode = Mode::Tags(t);
            Effect::None
        }
        Key::Enter => {
            // On the row that would make a tag, `↵` makes it and then applies:
            // typing a name and pressing return is one gesture, and refusing to
            // read it as one would be the picker being clever at you.
            if matches!(t.shown().get(t.sel), Some(TagRow::New(_))) {
                t.toggle();
            }
            let changes = t.changes();
            view.mode = Mode::Browse;
            if changes.is_empty() {
                say(ui, "unchanged");
                return Effect::None;
            }
            let mut argv = vec!["tag".to_string(), t.task.clone(), "--".to_string()];
            argv.extend(changes);
            Effect::Run { argv, escalate: None, then: None }
        }
        Key::Down | Key::Up => {
            let n = t.shown().len();
            if n > 0 {
                t.sel = match k {
                    Key::Down => (t.sel + 1).min(n - 1),
                    _ => t.sel.saturating_sub(1),
                };
            }
            view.mode = Mode::Tags(t);
            Effect::None
        }
        Key::Backspace | Key::KillLine | Key::Char(_) => {
            match k {
                Key::Backspace => {
                    t.filter.pop();
                }
                Key::KillLine => t.filter.clear(),
                Key::Char(c) => t.filter.push(c),
                _ => {}
            }
            // The rows under the cursor have just changed, so the cursor goes
            // back to the top rather than staying at an index that now names
            // something else.
            t.sel = 0;
            view.mode = Mode::Tags(t);
            Effect::None
        }
        _ => {
            view.mode = Mode::Tags(t);
            Effect::None
        }
    }
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
                ("t", "tags: ␣ picks, ↵ saves"),
                ("!", "high, low, normal"),
                ("m c f", "move, claim, find work"),
                ("u", "take the work back, clear"),
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
            if let Some(from) = verb.opened_with() {
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
    /// The click landed on a pane that was not the one being worked in. Taking
    /// the keyboard is the whole of what it did.
    Keyboard,
}

/// Decide what a click does, and move the cursor if that is what it does.
///
/// Here rather than in the event loop so it can be tested without a terminal:
/// the loop's job is to read the pane's size and act on the answer, and the
/// interesting half — select, then activate, without the tree moving — is
/// policy that a fixture can drive.
///
/// `keyboard` is whether this pane held the keyboard when the click arrived,
/// and it is as much a part of what a click means as where it landed. The mouse
/// reaches a pane nobody is working in — that is what makes the panel worth
/// pointing at — so the same pixel is two gestures: on the pane you are working
/// in it is select, or activate; on a pane you are not, it is "I am working
/// here now", and the panel has just answered it by taking focus.
///
/// Doing both is the bounce. Point at an agent the cursor is already on and the
/// click means `↵`, which goes to that agent's terminal — so the keyboard
/// arrives here and leaves again in the same gesture, and you end up in a pane
/// you had not decided to be in, by way of one you were only looking at.
pub(crate) fn click(
    ui: &mut Ui,
    view: &mut View,
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    keyboard: bool,
) -> Hit {
    // Before anything is read off the geometry: where the pointer landed does
    // not come into it, because this click was about the pane and not about a
    // row in it. The next one has the keyboard and means what it says.
    if !keyboard {
        return Hit::Keyboard;
    }
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
            Mode::Tags(t) => {
                view.mode = Mode::Tags(t.clone());
                tags_key(k, ui, view, t)
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
