//! Keys, modes, and what the panel is currently in the middle of.
//!
//! A key arrives as it was typed and is given meaning here, by a reducer that
//! knows the mode — `j` means "down" while browsing and means `j` while typing
//! a title, and only something holding [`Mode`] can tell those apart.
//!
//! [`apply_input`] is pure: state in, [`Effect`] out. Nothing in this file
//! talks to a terminal or to herdr, which is what lets the storyboard push
//! scripted input through the same path the live panel runs.
//!
//! Every input goes through that one door — a key, a click, the wheel, and the
//! keyboard arriving or leaving. The three that are not keys used to be
//! translated by the event loop instead, which meant the sentence they spell
//! ("a click on the selected row *is* `↵`") could only be run by a person at a
//! trackpad. [`apply_key`] is still the inner half, for a key alone.

use std::collections::HashSet;
use std::time::Instant;

use crate::input::Key;
use crate::live::AgentRef;
use crate::util;

use super::rows::{Card, Request, Row, Target, Ui};
use super::verbs::{browse_key, pick_tell, Ask, Pick, Tell};

/// What the viewer has folded, unfolded, or asked to see more of. Held by the
/// event loop and handed to `collect`, which is otherwise a pure function of
/// its [`Snapshot`]: the store, joined with what is running.
///
/// [`Snapshot`]: super::rows::Snapshot
impl View {
    /// Take the shape of the pane this panel is drawn in.
    ///
    /// Which rows exist depends on it — see [`View::wide`] — so it is taken
    /// before the rows are built rather than at the moment they are drawn, and
    /// it is one function so that the live loop and the storyboard cannot come
    /// to different views of what counts as a page.
    pub(crate) fn fit_to_pane(&mut self, w: usize) {
        self.wide = super::render::is_page(w);
    }

    /// This panel is the tab `Z` opened rather than the sidebar. Set from the
    /// command line at startup and never changed, because it is what this
    /// process *is*; see [`View::full`].
    pub(crate) fn takes_the_tab(&mut self, full: bool) {
        self.full = full;
    }

    /// Stand the panel at a width it has already commanded, so a scene can
    /// press a key from somewhere other than the sidebar. Test-only: the live
    /// path gets here through [`super::verbs::expand`], which also has a host to
    /// tell.
    #[cfg(test)]
    pub(crate) fn asked_for_width(&mut self, cols: Option<usize>) {
        self.asked_width = cols;
    }

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

    /// And read back, for the one menu row whose whole effect is a field on
    /// here rather than an [`Effect`] the caller can match on.
    #[cfg(test)]
    pub(crate) fn help_for_test(&self) -> bool {
        self.help
    }

    /// Where the word that opens the menu is drawn, so a script can point at
    /// the button rather than at a coordinate — the reason `strip_column`
    /// exists, and the same failure it avoids: a pair of numbers written down
    /// here describes the pane the scene was written at and goes on passing
    /// after the footer changes shape under it.
    pub(crate) fn menu_button(&self, w: usize, h: usize) -> (usize, usize) {
        (w.saturating_sub(1), h.saturating_sub(2))
    }

    /// And where one row of the open menu is. `None` when no menu is up, which
    /// is a script pointing at something that is not on the screen.
    pub(crate) fn menu_row(&self, i: usize, w: usize, h: usize) -> Option<(usize, usize)> {
        let Mode::Menu(menu) = &self.mode else { return None };
        let (top, left, _) = super::render::menu_box(menu, w, h);
        (i < menu.items.len()).then(|| (left + 1, top + 1 + i))
    }

    /// What the panel is in the middle of, as one word.
    ///
    /// The mode is the half of the state a frame is worst at reporting: a
    /// prompt and a pick both draw a line in the footer, and a tag picker over
    /// a task looks like a task with rows under it. So a storyboard scene that
    /// means "the keyboard is now the card's" has to be able to *say* it, and
    /// this is the name it says. Not [`Mode`] itself, because outside this
    /// module nothing has business matching on one.
    pub(crate) fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::Browse => "browse",
            Mode::Prompt { .. } => "a prompt",
            Mode::Pick { .. } => "a pick",
            Mode::Tags(_) => "the tag picker",
            Mode::Find { .. } => "a search",
            Mode::Card(_) => "a card",
            Mode::Confirm { .. } => "a confirmation",
            Mode::Menu(_) => "the menu",
        }
    }
}

#[derive(Default, Clone)]
pub(crate) struct View {
    /// Projects whose children and tasks are hidden.
    pub(super) collapsed: HashSet<String>,
    /// Projects (or the inbox) showing past `MAX_TASKS_PER_PROJECT`.
    pub(super) expanded: HashSet<String>,
    /// This panel *is* the tab: the one `Z` opened, drawn at the width of the
    /// workspace.
    ///
    /// What it changes is the way out. The sidebar is installed furniture and
    /// refuses to be quit by a stray `q` — losing it costs a reinstall and buys
    /// nothing. This one is a tab somebody opened a moment ago and will close
    /// again, so `q`, `esc` and `Z` all close it, and the footer says so.
    pub(super) full: bool,
    /// The pane is a page rather than a sidebar — wide enough to be read rather
    /// than glanced at, which is what `Z` makes it.
    ///
    /// A fact about this pane and not about the work, so it is never shared: two
    /// panels open on the same tree, one zoomed and one not, are looking at the
    /// same folds through different windows. The loop sets it from the terminal
    /// before the rows are built, because it is the rows that change — a page
    /// shows a project's tasks all of them, where a sidebar shows six and a
    /// count of the rest.
    pub(super) wide: bool,
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
    /// Narrow the tree to the tasks a phrase is in — the title, the id or the
    /// prose. Empty is off.
    ///
    /// The one filter that is not a mode you settle into: `A`, `R` and `w` are
    /// pressed once and worked under, and this is a question you asked a second
    /// ago and are about to answer. That is also why it is the one thing in
    /// [`View`] the panels do not share — see `super::shared`.
    ///
    /// While it is up the folds and the six-task cap are ignored, because a
    /// search whose answer is behind a fold is a search that says there is
    /// nothing. Nothing is unfolded: the folds are left exactly as they were
    /// and come back the moment the filter goes.
    pub(super) filter: String,
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
    /// What the last frame drew that offset against: the row the cursor was
    /// on, and which line of the tree it was drawn on.
    ///
    /// [`scroll`](Self::scroll) is a row *number*, and a row number is not what
    /// a reader is anchored to — they are looking at a row. The number's
    /// meaning changes underneath them every time the tree's shape does: the
    /// pane is a row taller or shorter (the focus dock rewrapping at a new
    /// width, the map opening, an agent joining or leaving the dock), or the
    /// rows themselves are rebuilt and everything below the change shifts. A
    /// stored offset that survives any of those and is merely clamped back
    /// into range drags the view to whichever end of the clamp it now falls
    /// outside, while the reader has not asked for anything.
    ///
    /// So this is the anchor to re-place it against — see
    /// [`super::render::geometry`], which uses it whenever the cursor is on
    /// the row it was on last frame, and ignores it the moment the cursor
    /// moves, because then the cursor is the thing that is asking.
    ///
    /// Not shared and not persisted: it is a fact about the frame in front of
    /// this reader, and a panel that has not drawn yet has no anchor to keep.
    pub(super) placed: Option<super::render::Anchor>,
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
    /// What is open, so `↵` on the same row means close it.
    ///
    /// One field for both ways of showing it — the page the panel draws in its
    /// own room, and the pane it splits when there is no host to ask. `↵` means
    /// the same thing either way, so the state it reads is the same state; what
    /// differs is only who is holding the cells, and that is the loop's to know.
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
    /// twenty-five characters; reading the rest meant `↵`, which puts the tree
    /// away and draws the whole task in its place. This reads it where you are
    /// and follows the cursor, so it is scrolling rather than looking things up.
    ///
    /// Both are still worth having, and the reason survived `↵` becoming a page
    /// rather than a second pane: what `F` costs is three rows, and what `↵`
    /// costs is the tree.
    pub(super) focus: bool,
    /// The width this panel has asked its host for, if it is asking.
    ///
    /// Not the width it *has* — that is [`super::run::Screen::size`], which is
    /// what the terminal could actually spare. This is the width that was
    /// commanded, and it is therefore the position in `Z`'s cycle outright: a
    /// press moves it whether or not the screen was wide enough for the move to
    /// show. See [`super::verbs::cycle`] for the order and
    /// [`super::run::Screen::ask_width`] for the seam.
    ///
    /// A fact about this pane and not about the work, so it is not carried
    /// between panels — the same reason [`View::wide`] is not. A second panel
    /// on the same tree has its own rect and its own host.
    pub(super) asked_width: Option<usize>,
    /// The ask this panel last put up, so it puts it up once.
    ///
    /// Without it a card that was dismissed would come straight back: the flag
    /// is marked read by a command, the rows are rebuilt from a snapshot taken
    /// before it ran, and the next frame would find the same unread ask waiting
    /// and open it again. It is also what stops a card the *CLI* failed to mark
    /// becoming a popup that cannot be closed — the panel asks once, and a hand
    /// it could not put away stays in the section, where a row is all it was
    /// ever going to be.
    pub(super) asked: Option<String>,
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
        /// One `esc` has been pressed on a line with something on it, and the
        /// next one throws the line away.
        ///
        /// It rides on the mode rather than on [`View`] so that it cannot
        /// outlive the prompt it belongs to: a prompt that closes takes the
        /// half-pressed cancel with it, and there is no flag left set for the
        /// *next* prompt's first `esc` to fire.
        armed: bool,
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
    /// Typing a phrase, with the tree narrowing to it on every keystroke.
    ///
    /// A mode rather than a prompt because the answer is the tree itself: a
    /// prompt collects a value and *then* does something, and what makes a
    /// search worth having is watching two hundred and seventy-six rows become
    /// four while you are still typing. `↵` is only the moment you stop typing
    /// and start pressing keys at what you found — the filter stays on, and
    /// every verb goes on meaning what it means.
    Find {
        buffer: String,
    },
    /// A raised hand, over the tree, waiting to be answered.
    ///
    /// The one mode nobody asked for: every other is entered by a key, and this
    /// arrives because an agent somewhere else raised its hand. That is what
    /// makes it a mode rather than a dock — it holds the keyboard until it is
    /// answered, so `y` cannot land on the tree behind it, and it is the only
    /// honest way to draw something over the rows a key would otherwise act on.
    ///
    /// It only ever comes up over [`Mode::Browse`]. Interrupting a half-typed
    /// title with somebody else's question would cost the typing and answer the
    /// question badly, so a card waits for the panel to be idle — the row and
    /// the footer count are already there in the meantime, and it comes up the
    /// moment the prompt closes.
    Card(Card),
    /// A yes before something that cannot be un-pressed.
    ///
    /// It holds the [`Effect`] itself rather than an argv, and that is what
    /// lets one mode stand in front of every key that needs it. The first one
    /// guarded `X`, whose whole content is a command, so an argv was the
    /// obvious payload — and it then could not be reached for by `S`, which
    /// starts an agent ([`Effect::Spawn`]), or by `f`, which types into a pane
    /// ([`Effect::Tell`]) and runs no command at all. Carrying the deed means
    /// the question is about *the thing the key was going to do*, whatever
    /// shape that is, and `y` is the key pressing itself again.
    ///
    /// Everything that used to be spelled out beside the argv — the stronger
    /// form to offer if the CLI refuses, the sentence an agent is owed once a
    /// claim lands — is inside [`Effect::Run`] already, and travels across the
    /// question by travelling with the deed.
    Confirm {
        question: String,
        deed: Box<Effect>,
    },
    /// A list of things to do that are not about any row.
    Menu(Menu),
}

/// The list a menu is holding while it is up.
///
/// Every other thing the panel does is a letter pressed at whatever the cursor
/// is standing on, and for work that is the right shape: `s`, `c`, `X` are all
/// about a task, a project or an agent. It is the wrong shape for the two or
/// three things that are about **the terminal itself** — they belong to no row,
/// so there is no row to press a letter at, and nobody finds them by trying.
///
/// It is also the answer to a guard rather than a hole in one, and that is
/// worth saying here because the next reader will otherwise find a `q` that
/// refuses to quit sitting beside a menu that offers to, and take one of them
/// for a mistake. The rule in [`View::full`] is that the sidebar is installed
/// furniture and must not be lost to a stray keystroke. That was the whole
/// story while the panel was a pane and herdr's own menu was still there; the
/// fork made this surface *be* herdr's sidebar, so the guard that stopped the
/// stray `q` also removed the last door out of the terminal — Ed, on the day:
/// *"annoying, since we lost the herdr menu."*
///
/// A menu is what the guard was protecting *for*: opening a list and picking a
/// row is deliberate, two-step and readable, which is exactly what a stray
/// keystroke is not. So the guard stays, `q` with nothing left to close opens
/// this instead of refusing — the key you press when you want out is the key
/// that shows you the ways out — and the one row that ends anything asks `y`
/// after that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Menu {
    /// What is on offer, in the order they are drawn. Fixed when the menu
    /// opens: a list that reordered under the cursor is how you press the row
    /// next to the one you meant, and one of these rows ends the terminal.
    pub(super) items: Vec<Item>,
    pub(super) sel: usize,
}

impl Menu {
    /// The menu reached from nowhere in particular — herdr's
    /// `render_global_launcher_menu`, and the half of the pair this task built.
    /// The other half is a menu about the row under the pointer, and it is a
    /// different `Vec<Item>` handed to this same struct: same reducer, same
    /// box, same click. That is the whole reason the payload is a list rather
    /// than a mode of its own.
    pub(crate) fn global() -> Menu {
        Menu { items: vec![Item::QuitHerdr, Item::ReloadConfig, Item::Keys], sel: 0 }
    }

    /// What is on it, for the test that holds the menu to the shape it was
    /// asked for: close first, and no row named by a word on its own.
    #[cfg(test)]
    pub(crate) fn items(&self) -> &[Item] {
        &self.items
    }

    /// The widest label, which is what the box is drawn to.
    pub(super) fn width(&self) -> usize {
        self.items.iter().map(|i| i.label().chars().count()).max().unwrap_or(0)
    }
}

/// One row of a menu: what it says, and what picking it does.
///
/// A closed enum rather than a label beside an [`Effect`], for the reason
/// [`Request`] is closed — what a row of a menu does is the panel's to decide,
/// so the payload names something the panel already knows how to do. Picking is
/// [`choose`], which is the one place a row becomes a deed.
///
/// **What is not here, and why.** herdr's own global menu offered five things:
/// `settings`, `keybinds`, `reload config`, `what's new`, and `detach`. Two of
/// them draw herdr's own screens — a settings form and a release-notes pane —
/// and there is no socket call that opens either, so a row for them would be a
/// row that cannot work; `what's new` needs herdr's update state, which nothing
/// on this side of the pipe is told. `keybinds` is herdr's key map, and wsp has
/// its own and a better claim to yours: [`Item::Keys`] is that one. What is
/// left is the two that are one call each, and they are the two that were
/// actually lost.
///
/// **`close` is not one word.** There is a pane, a tab, a workspace, and herdr
/// itself, and a menu row that said `close` would be a promise nobody could
/// read. Every row here names its object. The workspace is deliberately absent:
/// closing *this* workspace is a question about the row under the cursor, not
/// about nothing in particular — a surface has no workspace of its own at all
/// (`Where::nowhere`) — so it belongs to the context menu this is the mechanism
/// for, not to the global one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    /// End herdr: every workspace, every pane, and every agent running in one.
    ///
    /// First because Ed asked for it first — *"a sidebar menu, where close can
    /// be the first option"* — and because it is the thing whose absence built
    /// the menu. Never without a `y`: it is the one row here that cannot be
    /// un-pressed, and the question it raises counts what is running.
    QuitHerdr,
    /// Have herdr read its config file again.
    ReloadConfig,
    /// The key map, docked under the tree — the same thing `?` does.
    ///
    /// Here because a menu is the one part of the panel anybody can find
    /// without knowing a key, so it owes them the list of the keys. It is the
    /// only row that is also a keystroke, and that is the point of it.
    Keys,
}

impl Item {
    /// What the row says. Each names what it acts on, because `close` on its
    /// own is four different promises — see the enum's own docs.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Item::QuitHerdr => "quit herdr",
            Item::ReloadConfig => "reload herdr's config",
            Item::Keys => "the key map",
        }
    }
}

/// Picking a row. The menu closes either way: a menu that stayed up behind its
/// own answer would be a list you press twice.
pub(super) fn choose(item: Item, ui: &Ui, view: &mut View) -> Effect {
    view.mode = Mode::Browse;
    match item {
        Item::QuitHerdr => {
            // The question carries the consequence, the way `X`'s does, and
            // the consequence here is not on the screen anywhere: what quitting
            // costs is the agents, and the count is the one number that makes
            // `y` a decision rather than a reflex. Read off the census, which
            // is every agent on the machine whatever the tree is filtered to.
            // Short enough to survive the narrowest pane a panel is installed
            // in: the footer gives a question `w - 6` columns, and a question
            // that is cut before its own count is a question that has thrown
            // away the reason it was asked.
            let question = match ui.census.len() {
                0 => "quit herdr? nothing running".to_string(),
                1 => "quit herdr? 1 agent running".to_string(),
                n => format!("quit herdr? {n} agents running"),
            };
            view.mode = Mode::Confirm { question, deed: Box::new(Effect::Herdr(Chore::Quit)) };
            Effect::None
        }
        Item::ReloadConfig => Effect::Herdr(Chore::ReloadConfig),
        // Shown rather than toggled. A row picked out of a list is a request
        // for the thing named on it, and a `?` that put the map away because it
        // happened to be up already would be the one row here that did the
        // opposite of what it says.
        Item::Keys => {
            view.help = true;
            Effect::None
        }
    }
}

/// Put the global menu up. One implementation, because the word in the footer
/// and the key that falls through to it must not be able to open two different
/// menus — and because the context menu will arrive as a third caller with a
/// different list, not as a second mechanism.
pub(super) fn open_menu(view: &mut View) -> Effect {
    view.mode = Mode::Menu(Menu::global());
    Effect::None
}

/// What the keys mean while a menu is up.
///
/// Everything that is not moving, picking or leaving is swallowed, for the
/// reason the key map swallows it: the menu is open because you were not sure
/// what to do, which is the worst possible moment for a letter to fire at the
/// row behind it.
pub(super) fn menu_key(k: Key, ui: &mut Ui, view: &mut View, mut menu: Menu) -> Effect {
    let n = menu.items.len();
    match k {
        // `q` closes it as well as opening it, so the key that went looking for
        // a way out is also the way back. `esc` and `ctrl-c` for the same
        // reason every other mode takes them.
        Key::Esc | Key::Char('q') | Key::Interrupt => {
            view.mode = Mode::Browse;
            Effect::None
        }
        Key::Down | Key::Char('j') if n > 0 => {
            menu.sel = (menu.sel + 1) % n;
            view.mode = Mode::Menu(menu);
            Effect::None
        }
        Key::Up | Key::Char('k') if n > 0 => {
            menu.sel = (menu.sel + n - 1) % n;
            view.mode = Mode::Menu(menu);
            Effect::None
        }
        Key::Enter => match menu.items.get(menu.sel).copied() {
            Some(item) => choose(item, ui, view),
            None => {
                view.mode = Mode::Browse;
                Effect::None
            }
        },
        _ => Effect::None,
    }
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

/// What the cursor has to be on for a row's keys to do more than print a
/// refusal. `browse_key` already knows this, row by row — "only a task can be
/// blocked", "a board is a project's" — and [`keymap`] reads the same rule
/// back, so the popout stops advertising a fifth of itself as keys that do
/// nothing from here.
///
/// A pair sharing a line — `c f` is a claim made either end of it — is scoped
/// to the union of what its keys need, because the line is read and pressed
/// as the one idea the doc comment beside it already grouped them as.
#[derive(Clone, Copy)]
enum Scope {
    /// Every target, including the group headings and the empty selection —
    /// the only honest scope for a key like `P`, which never refuses.
    Always,
    Task,
    TaskOrProject,
    /// A task, or the agent's own pane — the two ends of a claim.
    TaskOrPane,
    /// A board is a project's; a task or the inbox hand one over instead of
    /// refusing.
    Board,
    Seat,
    /// An agent, wherever it is standing — a pane in the tree, or a seat it
    /// fills.
    Agent,
    Flagged,
}

impl Scope {
    fn shown(self, target: &Target, flagged: bool) -> bool {
        match self {
            Scope::Always => true,
            Scope::Flagged => flagged,
            Scope::Task => matches!(target, Target::Task(_)),
            Scope::TaskOrProject => matches!(target, Target::Task(_) | Target::Project(_)),
            Scope::TaskOrPane => matches!(target, Target::Task(_) | Target::Pane(_)),
            Scope::Board => matches!(target, Target::Task(_) | Target::Project(_) | Target::Inbox),
            Scope::Seat => matches!(target, Target::Seat(_)),
            Scope::Agent => matches!(target, Target::Pane(_) | Target::Seat(_)),
        }
    }
}

/// Every key the panel answers to, cut down to the ones that mean something
/// where the cursor is standing. Keys that do the same kind of thing share a
/// line — `s v` is one idea, not two — because the map is read in a column
/// thirty-four wide, and length is what pushes the tree off the screen; the
/// same reasoning is why an inert third of it is worth hiding rather than
/// scrolling past.
///
/// The verbs come first. Movement is the half you can find by pressing an arrow
/// and watching; `X` is not. So when a short pane can only fit part of this,
/// what it fits is the part that cannot be guessed.
///
/// Descriptions are written to fit that column. Anything longer is a sentence,
/// and a sentence belongs on the row it describes, not here.
pub(crate) fn keymap(target: &Target, flagged: bool) -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    let sections: Vec<(&'static str, Vec<(&'static str, &'static str, Scope)>)> = vec![
        (
            "change",
            vec![
                ("a P", "add task, project", Scope::Always),
                ("s v", "start, review", Scope::Task),
                ("d o", "done, reopen", Scope::Task),
                ("b p", "block: why · park: until", Scope::Task),
                ("e n", "retitle, note", Scope::TaskOrProject),
                ("t", "tags: ␣ picks, ↵ saves", Scope::Task),
                ("!", "high, low, normal", Scope::Task),
                ("m", "move it", Scope::TaskOrProject),
                ("c f", "claim, find work · y/n", Scope::TaskOrPane),
                ("C", "hand to a spare · y/n", Scope::Task),
                ("u", "take the work back · y/n", Scope::TaskOrPane),
                ("O", "a terminal here", Scope::TaskOrProject),
                ("S", "an agent on it · y/n", Scope::TaskOrProject),
                ("T", "say it to a project's governor", Scope::Seat),
                ("x", "lower a raised flag", Scope::Flagged),
                ("↵", "on a flag: the card again", Scope::Flagged),
                ("X", "remove, after y/n", Scope::TaskOrProject),
            ],
        ),
        (
            "look",
            vec![
                ("↵ esc", "open it, close it", Scope::Always),
                ("/", "find: any word, anywhere", Scope::Always),
                ("F", "the title in full, docked", Scope::Always),
                ("Z", "wider · wider · back", Scope::Always),
                ("E", "edit in a tab", Scope::TaskOrProject),
                ("K", "the board, and back", Scope::Board),
                ("A i r", "show done, ids, sync", Scope::Always),
                ("R", "only what needs review", Scope::Always),
                ("w", "the agents, not the work", Scope::Always),
                ("W", "its task, in the tree", Scope::Agent),
                ("q", "close it · then the menu", Scope::Always),
            ],
        ),
        (
            "move",
            vec![
                ("j k ↑ ↓", "up, down", Scope::Always),
                ("h l ← →", "fold, unfold", Scope::Always),
                ("< >", "…and all inside it", Scope::Always),
                ("H L", "…the whole tree", Scope::Always),
                ("g G ⇱ ⇲", "first, last row", Scope::Always),
                ("1-9", "jump to a terminal", Scope::Always),
            ],
        ),
    ];

    sections
        .into_iter()
        .filter_map(|(section, keys)| {
            let keys: Vec<(&'static str, &'static str)> = keys
                .into_iter()
                .filter(|(_, _, scope)| scope.shown(target, flagged))
                .map(|(k, what, _)| (k, what))
                .collect();
            (!keys.is_empty()).then_some((section, keys))
        })
        .collect()
}

/// What a key asked for beyond changing the view.
///
/// Comparable and cloneable because [`Mode::Confirm`] holds one: a deed put
/// behind a y/n is the same value the key would have returned, kept until the
/// answer comes.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The board, in place of the tree, at whatever width the host will give.
    ///
    /// `scope` is what the board is a board of, and it is also the argument the
    /// tab fallback runs `wsp kanban` with — see [`super::verbs::open_board`].
    /// One value rather than two, because a page and the tab that stands in for
    /// it showing different projects is a bug nobody would find twice.
    Board { scope: crate::kanban::Scope, label: String },
    /// The whole tree, in a tab of its own — or the one already open, brought
    /// to the front.
    ///
    /// A tab rather than a zoom of this pane: a zoomed pane makes the terminal
    /// beside it unreachable, and the sidebar is furniture and has no business
    /// doing that to a workspace. It is a second panel and that costs nothing —
    /// the folds and the cursor live in the store, so the two are the same panel
    /// at two widths.
    ///
    /// In a sidebar it is a step round [`super::verbs::cycle`] instead, because
    /// there is a host to ask: three widths and a key that only goes forward.
    Full,
    /// All the room back at once, whichever width the cycle was on.
    ///
    /// `Z` goes one way round and `esc` is the way out, which is the answer to
    /// the only question three states raise that two did not: getting back to a
    /// sidebar must not cost three presses. It is the same `esc` that closes
    /// the map, the search and the detail pane — the nearest thing you opened —
    /// and the room a panel is borrowing is the nearest thing there is.
    ///
    /// Separate from [`Effect::Full`] rather than a width carried on it: the
    /// reducer is handed no screen, so it cannot know what half of one is. It
    /// says which way, and the loop knows how wide.
    Sidebar,
    /// Argv for this binary. Running the CLI rather than reimplementing it
    /// means the event log, the hooks and the git commit all still happen,
    /// because it is the same code path a person at a shell would take.
    ///
    /// `escalate` is the stronger form to offer if the CLI refuses this one.
    /// The panel does not decide on your behalf that a refusal should be
    /// overridden — it puts what the CLI said on screen as the next question,
    /// which is why the refusal is worded by the code that knows the rule.
    ///
    /// `then` is what to say to an agent once it has worked — a claim that
    /// nobody tells the agent about leaves it sitting idle on work it now
    /// holds. Withheld if the command is refused: the sentence would be a lie.
    /// It rides here rather than beside the question in [`Mode::Confirm`], so
    /// that a claim confirmed is still a claim with somebody told about it —
    /// dropping it across the question once left the one case that most needs
    /// the sentence, work taken by force, as the one case with nobody told.
    Run { argv: Vec<String>, escalate: Option<Vec<String>>, then: Option<Tell> },
    /// Type into an agent's pane. The only effect that changes nothing at all
    /// in the store — what it changes is what an agent is about to do.
    Tell(Tell),
    /// Something asked of herdr itself rather than of the work.
    ///
    /// Every other effect that *does* something runs `wsp`, because the CLI is
    /// the one implementation and the panel is a caller of it. These have no
    /// CLI to run and never will: they are about the terminal the work happens
    /// in, and wsp keeps no record of a terminal. So they go where
    /// [`Effect::Focus`] already goes — straight down herdr's socket from the
    /// loop — and the loop is where the one call lives.
    Herdr(Chore),
}

/// What the panel asks of herdr on its own account.
///
/// Two, and the shortness is the point: wsp is not a remote control for herdr,
/// and every arm here is a thing the fork took away rather than a thing worth
/// adding. See [`Item`] for what was left out and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chore {
    /// `server.stop`: herdr sets `should_quit` and exits, taking every pane and
    /// every agent with it. What herdr's own menu called `detach`, named for
    /// what it does here rather than for what herdr calls it — herdr's row only
    /// quits when it is configured to, and this call always does.
    Quit,
    /// `server.reload_config`: herdr re-reads its config file.
    ReloadConfig,
}

impl Chore {
    /// What was asked, as a phrase — for a storyboard scene, which has to be
    /// able to say what a keypress *did* where no frame can show it.
    pub(crate) fn said(self) -> &'static str {
        match self {
            Chore::Quit => "quit",
            Chore::ReloadConfig => "reload its config",
        }
    }
}

pub(super) fn say(ui: &mut Ui, m: impl Into<String>) {
    ui.message = Some((m.into(), Instant::now()));
}

/// Typing a value. Every printable key is text here — including `q`, which is
/// why the input layer stopped deciding what keys mean.
///
/// `esc` on a line with something typed on it does not cancel: it arms, the
/// line says `esc again`, and the second press throws the typing away. The
/// hands that reach this prompt come from vim, where `esc` is how you *stop*
/// typing — so it arrives here as a reflex, aimed at a mode this panel does
/// not have, and it was costing whole titles and notes that had to be typed
/// again from memory. A press that undoes a reflex has to be a press you meant.
///
/// Two things keep the cost of that at nothing. An empty line cancels on the
/// first press, because there is nothing to lose and "I opened this by
/// mistake" is the other common reason to hit `esc` here. And `ctrl-c` always
/// cancels outright: nobody types it by reflex, so it stays the one-key way
/// out for anyone who wants one.
pub(super) fn prompt_key(
    k: Key,
    ui: &mut Ui,
    view: &mut View,
    verb: Ask,
    mut buffer: String,
    armed: bool,
) -> Effect {
    match k {
        Key::Char(c) => {
            buffer.push(c);
            view.mode = Mode::Prompt { verb, buffer, armed: false };
            Effect::None
        }
        Key::Backspace => {
            buffer.pop();
            view.mode = Mode::Prompt { verb, buffer, armed: false };
            Effect::None
        }
        Key::KillLine => {
            buffer.clear();
            view.mode = Mode::Prompt { verb, buffer, armed: false };
            Effect::None
        }
        // Armed by the press before this one, so this is the second `esc` and
        // it means it. Anything else in between disarms — see the arms above
        // and the fall-through below — which makes the rule one sentence: the
        // two presses have to be next to each other.
        Key::Esc if armed || buffer.is_empty() => {
            view.mode = Mode::Browse;
            say(ui, "cancelled");
            Effect::None
        }
        Key::Esc => {
            view.mode = Mode::Prompt { verb, buffer, armed: true };
            Effect::None
        }
        Key::Interrupt => {
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
        // A key the prompt has no use for still disarms. It does nothing to the
        // line, but it is a press between the two `esc`s, and a rule with an
        // exception in it — "the next key, unless it was one I ignored" — is a
        // rule nobody can hold while typing.
        _ => {
            view.mode = Mode::Prompt { verb, buffer, armed: false };
            Effect::None
        }
    }
}

/// Typing a search. Every printable key narrows the tree, `q` included — the
/// same bargain the prompt and the tag picker make.
///
/// Every edit asks for a refetch, which is what makes the tree the answer
/// rather than something you get after pressing return. It costs a rebuild per
/// keystroke, at the same price `A` and `R` pay for a press: the store is read
/// whole by every command here, and a person types at ten keys a second.
///
/// `esc` clears the filter rather than putting back whatever was there before.
/// One way out, and it is the same one `esc` has in [`Mode::Browse`] while a
/// filter is up — a search you abandon is a search that leaves no tree behind
/// it.
pub(super) fn find_key(k: Key, ui: &mut Ui, view: &mut View, mut buffer: String) -> Effect {
    match k {
        Key::Esc | Key::Interrupt => {
            view.mode = Mode::Browse;
            view.filter.clear();
            say(ui, "the whole tree");
            Effect::Refetch
        }
        Key::Enter => {
            view.mode = Mode::Browse;
            if view.filter.is_empty() {
                say(ui, "nothing typed");
                return Effect::None;
            }
            // What the tree is now, said once on the way out: from here the
            // footer carries it, and the reader is looking at rows again
            // rather than at what they typed.
            say(ui, format!("showing what matches \"{}\" · esc clears", view.filter));
            Effect::None
        }
        Key::Backspace | Key::KillLine | Key::Char(_) => {
            match k {
                Key::Backspace => {
                    buffer.pop();
                }
                Key::KillLine => buffer.clear(),
                Key::Char(c) => buffer.push(c),
                _ => {}
            }
            view.filter = buffer.clone();
            view.mode = Mode::Find { buffer };
            // The rows under the cursor have just changed. The cursor is kept
            // by identity across a rebuild, so it stays on its row for as long
            // as the row is still a hit and falls to the nearest one when it
            // stops being — which is what typing into a list should feel like.
            Effect::Refetch
        }
        _ => {
            view.mode = Mode::Find { buffer };
            Effect::None
        }
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
                let question = verb.confirm(&ui.selected_target());
                let deed = Effect::Run { argv, escalate, then };
                match question {
                    // The pick closes either way. A y/n drawn over a tree that
                    // is still hunting would be two questions on one line, and
                    // `n` has to land somewhere that is not another `↵`.
                    Some(q) => {
                        view.mode = Mode::Confirm { question: q, deed: Box::new(deed) };
                        Effect::None
                    }
                    None => {
                        view.mode = Mode::Browse;
                        deed
                    }
                }
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
        // Folding by more than a row stays live too, and earns its place here
        // more than anywhere: a pick is a hunt for one project among thirty,
        // and `H` then `>` is that hunt with the whole tree out of the way and
        // one branch opened onto.
        Key::Char('<') => fold_branch(true, ui, view),
        Key::Char('>') => fold_branch(false, ui, view),
        Key::Char('H') => fold_tree(true, ui, view),
        Key::Char('L') => fold_tree(false, ui, view),
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

/// Keep the card on this panel equal to what the store says is being asked.
///
/// Both directions, and the second one is the half that was missing. Putting a
/// card *up* is the obvious job: an unread flag arrives and the panel asks. But
/// a card is up in **every** panel at once — they all read the same file and
/// they all drew it — and answering it happens in exactly one of them. Nothing
/// told the other twenty-one, so they sat holding a question that had already
/// been settled: the flag lowered, the task claimed, and a card still standing
/// over the tree with `y` on it. Since a card holds the keyboard, switching to
/// one of those panels meant arriving at a modal question about work that was
/// already in somebody's hands.
///
/// So the card is *derived* rather than remembered. `flags.json` is the record
/// every panel reads, `pending` is what it currently says is being asked, and
/// this makes the panel agree with it on every frame — which is the same
/// bargain the folds and the cursor make through `panel-view.json`, one file
/// down. A card that is no longer the ask goes; one whose words have changed —
/// an agent that raised its hand again with more to say — is replaced where it
/// stands rather than closed and re-opened.
///
/// Putting one up has two conditions, and both are about not talking over
/// somebody: the panel has to be browsing — a prompt, a pick or a confirm is a
/// sentence half-said, and a card landing on it would take the keys meant for
/// it — and it must not be the one this panel has already asked about.
///
/// Not gated on this panel being the one on screen. There are twenty-two of
/// them and only one is ever in front of you; a card that came up on the
/// focused panel alone would be one you could dodge by switching workspaces,
/// which is exactly when you would most like to have been asked.
pub(crate) fn pop_pending(ui: &Ui, view: &mut View) {
    // What is up, against what is being asked.
    if let Mode::Card(up) = &view.mode {
        match &ui.pending {
            // Settled somewhere else — answered, lowered, or read on another
            // panel. The question is gone, so the card is.
            None => view.mode = Mode::Browse,
            Some(now) if now.task != up.task => view.mode = Mode::Browse,
            // The same ask, said differently. Replaced in place: closing and
            // re-opening would count as this panel having asked, and the guard
            // below would then refuse to put the new words up at all.
            Some(now) if now != up => view.mode = Mode::Card(now.clone()),
            Some(_) => {}
        }
    }

    let Some(card) = ui.pending.clone() else {
        // Nothing waiting. Forget what was asked, so a hand raised on the same
        // task again — which is an agent asking twice, and worth reading twice
        // — comes up rather than being taken for the one already answered.
        view.asked = None;
        return;
    };
    if !matches!(view.mode, Mode::Browse) || view.asked.as_deref() == Some(card.task.as_str()) {
        return;
    }
    view.asked = Some(card.task.clone());
    view.mode = Mode::Card(card);
}

/// What the keys mean while a card is up.
///
/// Three answers and a way out, and the difference between them is the whole
/// design: `esc` is *not now* — the card goes and the hand stays raised, so the
/// section still says somebody is waiting. `x` is *dealt with*, which takes the
/// flag down everywhere. Answering the question does both, because a question
/// that has been answered is not still waiting on an answer.
///
/// `o` opens the task in the detail pane and leaves the card standing. A card
/// is three lines about a piece of work and the decision it asks for is often
/// not answerable from three lines — without this, deciding meant dismissing
/// the question first, which is how a `y` gets pressed on a card nobody read.
pub(super) fn card_key(k: Key, ui: &mut Ui, view: &mut View, card: Card) -> Effect {
    let seen = |view: &mut View| {
        view.mode = Mode::Browse;
        Effect::Run {
            argv: vec!["flag".into(), card.task.clone(), "--seen".into()],
            escalate: None,
            then: None,
        }
    };
    match k {
        // Not now. The card goes; the hand stays up.
        //
        // `ctrl-c` too. In [`Mode::Browse`] it quits the panel, and every mode
        // here takes it to mean "out of this" instead — a reflex that would
        // otherwise take down a sidebar because somebody wanted rid of a
        // question.
        Key::Esc | Key::Char('q') | Key::Interrupt => seen(view),
        // `↵` is the answer to a card that only wanted to be looked at, and is
        // deliberately not an answer to one that asked for something: a return
        // pressed out of habit must never hand a task over.
        Key::Enter if card.ask.is_none() => seen(view),
        Key::Enter => {
            say(ui, "y or n · esc leaves it raised");
            Effect::None
        }
        // Dealt with, whatever it was asking.
        Key::Char('x') => {
            view.mode = Mode::Browse;
            Effect::Run {
                argv: vec!["flag".into(), "--clear".into(), card.task.clone()],
                escalate: None,
                then: None,
            }
        }
        // Read the work before answering for it. The card stays up: this is
        // what you pressed to be able to answer, not instead of answering.
        Key::Char('o') => Effect::Inspect(crate::detail::Focus::Task(card.task.clone())),
        Key::Char('y') | Key::Char('Y') if card.ask == Some(Request::Claim) => {
            view.mode = Mode::Browse;
            // The same argv `c` builds, arriving from the other direction: the
            // agent picked the task and you picked the agent, and a claim is
            // thirty lines of guards that must have one implementation.
            //
            // Nothing lowers the flag here, and nothing needs to: `claim`
            // lowers the flags on the task it claims, because a hand raised
            // about work somebody has just been handed has been answered.
            let argv = vec![
                "claim".into(),
                card.task.clone(),
                "--pane".into(),
                card.who.clone(),
            ];
            Effect::Run { argv, escalate: None, then: super::verbs::tell_for_pane(ui, &card.who, &card.task) }
        }
        Key::Char('n') | Key::Char('N') if card.ask == Some(Request::Claim) => {
            view.mode = Mode::Browse;
            // A no is worth typing back. The agent asked and then went on with
            // something else; silence reads as an answer that never came, and
            // an agent that is still waiting on one is an agent doing nothing.
            Effect::Run {
                argv: vec!["flag".into(), "--clear".into(), card.task.clone()],
                escalate: None,
                then: super::verbs::tell_refused(ui, &card.who, &card.task),
            }
        }
        _ => Effect::None,
    }
}

pub(super) fn confirm_key(k: Key, ui: &mut Ui, view: &mut View, deed: Effect) -> Effect {
    match k {
        Key::Char('y') | Key::Char('Y') => {
            view.mode = Mode::Browse;
            deed
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

/// The key a row folds under, when it folds at all — a project, or one of the
/// headings that is not a project. The same pair [`move_or_fold`] acts on, and
/// deliberately read off the row rather than off the store: what the fold keys
/// below can name is what the tree is currently drawing.
fn fold_key(row: &Row) -> Option<&String> {
    match row {
        Row::Project { id, .. } => Some(id),
        Row::Section { key, .. } => Some(key),
        _ => None,
    }
}

/// What to call that branch in the footer: the word the row is drawn with,
/// which for a section is not the key it folds under — the inbox folds under
/// `(inbox)`, and a message naming it that is the panel talking about its own
/// insides.
fn fold_label(row: &Row) -> &str {
    match row {
        Row::Section { label, .. } => label,
        _ => fold_key(row).map(String::as_str).unwrap_or_default(),
    }
}

/// How far in a row is drawn, which is what says where a branch stops.
///
/// A section heading carries no depth of its own and is one of the two rows a
/// branch can start at, so it answers 0 — everything it holds is drawn at 1,
/// and the next heading along ends it. `None` is a row that belongs to the one
/// above it, and those are stepped over rather than taken for the end of
/// anything, for the same reason [`step`] steps over them.
fn depth_of(row: &Row) -> Option<usize> {
    match row {
        Row::Section { .. } => Some(0),
        Row::Project { depth, .. }
        | Row::Task { depth, .. }
        | Row::More { depth, .. }
        | Row::Agent { depth, .. }
        | Row::Group { depth, .. }
        | Row::Seat { depth, .. } => Some(*depth),
        _ => None,
    }
}

/// The row a branch hangs from: this one if it folds, and otherwise the
/// nearest row above it that does.
///
/// Standing on a task in `render` is standing *in* `render`, and a key that
/// refused there would be asking for a walk up to the heading it can work out
/// for itself — which is the walk `<` and `>` exist to save. Read by depth
/// rather than by counting rows back, because a task's own sub-tasks sit
/// between it and its project.
fn branch_head(rows: &[Row], from: usize) -> Option<usize> {
    if fold_key(rows.get(from)?).is_some() {
        return Some(from);
    }
    let mut depth = depth_of(rows.get(from)?)?;
    for i in (0..from).rev() {
        let Some(above) = depth_of(&rows[i]) else { continue };
        if above < depth {
            depth = above;
            if fold_key(&rows[i]).is_some() {
                return Some(i);
            }
        }
    }
    None
}

/// The branch the cursor is standing in: the row it hangs from, and the range
/// of rows drawn inside it.
fn branch(rows: &[Row], from: usize, end: usize) -> Option<(usize, std::ops::Range<usize>)> {
    let head = branch_head(rows, from)?;
    let depth = depth_of(&rows[head])?;
    let mut last = head + 1;
    for i in head + 1..end {
        match depth_of(&rows[i]) {
            Some(d) if d > depth => last = i + 1,
            Some(_) => break,
            None => {}
        }
    }
    Some((head, head + 1..last))
}

/// One row folded, and folded the way `h` folds it: shut, and with the cap back
/// on the list inside it. A branch that came out of a fold still showing its
/// fortieth task would be a fold that half-remembered what it had put away.
fn fold_one(view: &mut View, key: &str) -> bool {
    let capped = view.expanded.remove(key);
    view.collapsed.insert(key.to_string()) || capped
}

/// `<` and `>`: everything inside the branch the cursor is standing in, in one
/// press.
///
/// Both work off the rows on screen, and between them that settles two things
/// that would otherwise look arbitrary.
///
/// `<` leaves the head row open, because shutting it as well is `h` — and
/// because a branch closed over its own contents is one `>` can no longer read:
/// the rows it would have to name are the ones the fold just took away, so a
/// press would take a press per level to undo. `<` then `h` is "tidy this, then
/// put it away", and `l` brings it back tidied.
///
/// And `<` folds the head's *children* rather than every fold under it. Shutting
/// a child takes everything below it off the screen too, so the tree looks the
/// same either way; what differs is what is remembered, and remembering less is
/// what lets `>` put the whole branch back in one press. A fold nobody can see
/// is a fold nobody asked for.
pub(super) fn fold_branch(fold: bool, ui: &mut Ui, view: &mut View) -> Effect {
    // The dock is its own list, pinned under a rule, and a branch in the tree
    // does not run into it.
    let end = match ui.sel < ui.tree_len() {
        true => ui.tree_len(),
        false => ui.rows.len(),
    };
    let Some((head, inside)) = branch(&ui.rows, ui.sel, end) else {
        say(ui, "nothing there to fold");
        return Effect::None;
    };
    let key = fold_key(&ui.rows[head]).cloned().unwrap_or_default();
    let name = fold_label(&ui.rows[head]).to_string();
    let depth = depth_of(&ui.rows[head]).unwrap_or(0);
    // Opening takes the head with it: `>` on a branch that is shut has to open
    // the thing you are pointing at, or there is nothing inside it to open.
    let mut moved = !fold && view.collapsed.remove(&key);
    for i in inside {
        if fold && depth_of(&ui.rows[i]) != Some(depth + 1) {
            continue;
        }
        let Some(k) = fold_key(&ui.rows[i]).cloned() else { continue };
        let did = match fold {
            true => fold_one(view, &k),
            false => view.collapsed.remove(&k),
        };
        moved = moved || did;
    }
    if !moved {
        // Both the branch that holds nothing that folds and the one already
        // folded, said as the one thing they amount to: this key has nothing
        // left to do here.
        say(ui, format!("nothing left to {} in {name}", if fold { "fold" } else { "open" }));
        return Effect::None;
    }
    say(
        ui,
        match fold {
            true => format!("folded everything in {name}"),
            false => format!("opened everything in {name}"),
        },
    );
    Effect::Refetch
}

/// `H` and `L`: every fold on the panel, shut or open.
///
/// Unfolding is exact and folding is not, and the difference is what each has
/// to work from. Unfolded is the empty set, so `L` clears it and nothing has to
/// be enumerated; folding can only name the rows that are drawn, and a branch
/// left open *inside* one that was already shut is not one of them. What is on
/// the screen folds, which is what the key is for, and the fold underneath is
/// still there if you go back down to it — the alternative is this reducer
/// holding a second copy of the project tree to walk, which is the store's
/// answer to a question about the view.
///
/// **The tree, and not the dock pinned under it.** `H` is the key for putting
/// thirty-one projects away, and it used to take the census at the foot with
/// them — the one list on the panel that exists so that who is running survives
/// whatever the tree is doing. Nothing said so afterwards but a `▸` on one row,
/// and folds are shared and persisted, so a single press left every panel on
/// the machine with no view of the fleet until somebody guessed. Reported by Ed
/// 2026-08-18 as "I can't see any agents in the inline agents panel", and the
/// stored view had `(agents)` sitting in the same set as every project he had
/// just folded. The flags share the dock and so shared the fault: `H` could put
/// a raised hand off the screen, which is the one thing that section exists to
/// make impossible. `h` on the section itself still puts either away, which is
/// a person answering for the dock rather than for the tree, and `L` still
/// opens it.
pub(super) fn fold_tree(fold: bool, ui: &mut Ui, view: &mut View) -> Effect {
    let mut moved = false;
    if fold {
        let tree = ui.tree_len();
        let keys: Vec<String> = ui.rows[..tree].iter().filter_map(fold_key).cloned().collect();
        for k in keys {
            moved = fold_one(view, &k) || moved;
        }
    } else {
        moved = !view.collapsed.is_empty();
        view.collapsed.clear();
    }
    if !moved {
        say(ui, format!("the tree is already {}", if fold { "folded" } else { "open" }));
        return Effect::None;
    }
    say(ui, if fold { "the whole tree, folded" } else { "the whole tree, open" });
    Effect::Refetch
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
    /// The word `menu` in the footer.
    Opens,
    /// A row of the menu that is already open.
    ///
    /// One click and it is taken, where a row of the tree gets select and then
    /// activate. The two are different for the reason the strip's marks are:
    /// there is nothing to read on the way, the list is three rows you opened
    /// on purpose, and the row that costs anything asks `y` afterwards. A menu
    /// you have to click twice is one that reads as broken.
    Menu(usize),
    /// A click anywhere else while the menu is up: put it away.
    ///
    /// A popover closes when you click off it — that is what makes it a
    /// popover rather than a mode you are stuck in, and it is the behaviour a
    /// pointer has everywhere else it meets one. It covers the word in the
    /// footer too, so clicking `menu` a second time shuts what it opened; the
    /// word is outside the box, so it arrives here rather than as
    /// [`Hit::Opens`], which only fires while browsing. A click on the box's own
    /// frame is a miss and not a dismissal — see [`super::render::menu_holds`].
    ///
    /// Deliberately not the click passing through to whatever is underneath.
    /// The tree behind a popup is not what the click meant — the same rule
    /// [`Mode::Card`] is held to — so the first click dismisses and a second
    /// one selects.
    Shuts,
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
    // A card is over the rows, so a click cannot mean the row under it — the
    // pointer is on a box, and the box takes keys rather than clicks. Reading
    // through it would select whatever the popup happens to be covering, which
    // is a row nobody can see.
    if matches!(view.mode, Mode::Card(_)) {
        return Hit::Nothing;
    }
    // The menu is over them too, and is the one popup a pointer is *for*: it
    // was opened from a word in the footer, and the row under the pointer is a
    // row somebody is pointing at. Off the box and it is the same miss the card
    // gives — the tree behind is not what the click meant.
    if let Mode::Menu(menu) = &view.mode {
        return match super::render::menu_at(menu, w, h, x, y) {
            Some(i) => Hit::Menu(i),
            // On the box but not on a row: the frame. A miss, and the menu
            // stays — the edge of a box is not a way out of it.
            None if super::render::menu_holds(menu, w, h, x, y) => Hit::Nothing,
            None => Hit::Shuts,
        };
    }
    // The word in the footer, and only while browsing. A click that opened a
    // menu over a half-typed title would cost the typing, which is the rule
    // [`Mode::Card`] is already held to.
    if matches!(view.mode, Mode::Browse)
        && y + 2 == h
        && x + super::render::MENU_BUTTON.chars().count() >= w
    {
        return Hit::Opens;
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
        // row it is written beneath, which is the row it is about. A group
        // heading belongs to the run under it, so the pointer walks the other
        // way — clicking `verb` and landing on the last agent of the group
        // above it would be the one click on the panel that goes backwards.
        Some(i) if !ui.rows[i].selectable() => {
            let owner = step(&ui.rows, i, matches!(ui.rows[i], Row::Group { .. }));
            if !ui.rows[owner].selectable() {
                return Hit::Nothing;
            }
            // From here it is a click on that row, including the second one:
            // point at a heading twice and you meant the agent it stands over
            // both times, so the first selects it and the next goes there.
            // Anything else makes a line you can click forever without it ever
            // meaning `↵`.
            if owner == ui.sel {
                return Hit::Activate;
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
            Mode::Prompt { verb, buffer, armed } => {
                view.mode = Mode::Prompt { verb: verb.clone(), buffer: buffer.clone(), armed };
                prompt_key(k, ui, view, verb, buffer, armed)
            }
            Mode::Pick { verb } => {
                view.mode = Mode::Pick { verb: verb.clone() };
                pick_key(k, ui, view, verb)
            }
            Mode::Tags(t) => {
                view.mode = Mode::Tags(t.clone());
                tags_key(k, ui, view, t)
            }
            Mode::Find { buffer } => {
                view.mode = Mode::Find { buffer: buffer.clone() };
                find_key(k, ui, view, buffer)
            }
            Mode::Card(card) => {
                view.mode = Mode::Card(card.clone());
                card_key(k, ui, view, card)
            }
            Mode::Confirm { question, deed } => {
                view.mode = Mode::Confirm { question, deed: deed.clone() };
                confirm_key(k, ui, view, *deed)
            }
            Mode::Menu(menu) => {
                view.mode = Mode::Menu(menu.clone());
                menu_key(k, ui, view, menu)
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

/// Everything that can arrive at a panel from outside it.
///
/// [`apply_key`] was the whole alphabet while the only input was a keystroke.
/// It is not: a click has to be turned into a row before it means anything, a
/// wheel moves the view rather than the cursor, and whether the pane holds the
/// keyboard changes what the *next* click does. Those three lived in the event
/// loop, which is the one place a fixture cannot reach — so the click that
/// selects, the click that opens, and the click that only takes the keyboard
/// were all verified by pointing at a real pane and looking.
///
/// This is that alphabet with nothing left in the loop but the parts that are
/// genuinely about a terminal: reading the pane's size, and telling herdr where
/// the reader now is. See [`apply_input`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Input {
    /// A key as it was typed. [`Key`] carries the mouse too, because that is
    /// how a mouse arrives from a terminal — an SGR report parsed into
    /// [`Key::Click`] or [`Key::Wheel`] by `crate::input`.
    Key(Key),
    /// The keyboard arrived in this pane, or left it.
    ///
    /// Not a key, and not drawn — `crate::draw`'s "focus is not an input" holds
    /// here too, which is why it is a message to the reducer rather than a
    /// field on [`View`]. Its only consequence is what a click means: on the
    /// pane you are working in a click selects, and on one you are only looking
    /// at it says "I am working here now" and nothing else.
    Focus(bool),
}

/// One input, applied.
///
/// `keyboard` is the panel's answer to "is this the pane being worked in", and
/// it is `&mut` because a click changes it: pointing at a pane is a statement
/// that you are working in it, and the pane has answered by taking focus. The
/// loop still owns the round-trip that tells herdr — that is a socket call on a
/// clock, not a state transition — and this owns the fact.
///
/// It lives beside [`apply_key`] rather than in the loop for the reason the
/// whole module doc gives: state in, [`Effect`] out, nothing here talks to a
/// terminal. What a click is worth testing for is not the arithmetic of which
/// row is under the pointer — that is [`click`], and it was already reachable —
/// but the sentence the loop was spelling around it: that a click on the
/// selected row *is* `↵`, that the `⋯` in the strip *is* `w`, and that a click
/// into an unfocused panel is neither.
pub(crate) fn apply_input(
    input: Input,
    ui: &mut Ui,
    view: &mut View,
    w: usize,
    h: usize,
    keyboard: &mut bool,
) -> Effect {
    let k = match input {
        Input::Focus(f) => {
            *keyboard = f;
            return Effect::None;
        }
        Input::Key(k) => k,
    };
    // Whether the pane was being worked in is read before the line that changes
    // it, because that is the question the click is answering.
    let had = *keyboard;
    if matches!(k, Key::Click { .. } | Key::Wheel { .. }) {
        *keyboard = true;
    }
    match k {
        Key::Wheel { up } => {
            wheel(ui, view, w, h, up);
            Effect::None
        }
        // Select, then activate. One click moves the cursor and does nothing
        // else; a click on the row already under it means what `↵` means, so it
        // *becomes* that key rather than restating what opening a row does. A
        // click that both selects and opens is how you end up somewhere you did
        // not ask to be, on a row you had not read — and here `↵` can focus
        // another pane, so that is a terminal you were not looking at.
        Key::Click { x, y } => match click(ui, view, w, h, x, y, had) {
            Hit::Activate => apply_key(Key::Enter, ui, view),
            // A mark in the strip is an agent, and going there is the whole of
            // what it means. The keyboard goes with it.
            Hit::Focus(a) => {
                *keyboard = false;
                Effect::Focus(a)
            }
            // The `⋯`: the agents the strip could not draw. It stands for the
            // same thing `w` does, so it presses it.
            Hit::Rest => apply_key(Key::Char('w'), ui, view),
            // The word in the footer opens the menu, and only that. It is
            // deliberately not the `q` key pressed for you, the way the `⋯` is
            // `w` pressed for you: `q` means "give back the nearest thing" and
            // only falls through to the menu when there is nothing left to give
            // back, so in the tab `Z` opened it would close the tab. A word
            // that says `menu` has to open the menu wherever it is drawn.
            Hit::Opens => open_menu(view),
            Hit::Menu(i) => match &view.mode {
                Mode::Menu(menu) => match menu.items.get(i).copied() {
                    Some(item) => choose(item, ui, view),
                    None => Effect::None,
                },
                _ => Effect::None,
            },
            // Off the box, so the menu goes away — including the word that
            // opened it, which is why `menu` toggles rather than doing
            // nothing on the second click.
            Hit::Shuts => {
                view.mode = Mode::Browse;
                Effect::None
            }
            // Selected, or landed on furniture, or landed on a pane nobody was
            // working in — all of which the cursor and `keyboard` above have
            // already recorded, and none of which anybody has to be told about.
            Hit::Select | Hit::Nothing | Hit::Keyboard => Effect::None,
        },
        k => apply_key(k, ui, view),
    }
}
