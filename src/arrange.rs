//! The arrange-panes port: what wsp asks of the thing it is living *inside*.
//!
//! Nothing calls this yet. It is the contract t-260816-084 was opened to write,
//! and t-260816-061 is the task that moves the call sites onto it. Read it as
//! the answer to one question: **when wsp wants a screen to look a certain way,
//! what does it say, and to whom?**
//!
//! Ed's shape, 2026-08-16, and the port is a transcription of it:
//!
//! > here are our panes; here is the content for the panes that need content;
//! > here is what should fill the rest, via an agent or a command.
//!
//! That is a declarative statement, and the word that carries it is *here are*.
//! The output of the view layer is a **desired state** — a [`Spec`] — and not a
//! sequence of calls. Something underneath reconciles the world to it, and that
//! something is [`plan`], which is a pure function of what we saw and what we
//! want. Nothing in this file talks to herdr.
//!
//! # The sibling contract, and the line this port is on the other side of
//!
//! `place.rs` is the other herdr port (decision on t-260816-083). It answers
//! *start an agent on this task* and its first rule is that **nothing in it
//! names a pane, a window, a tab or a terminal** — because a supervisor with no
//! TTY has to be able to implement all of it.
//!
//! This port names panes on purpose, and a reader arriving from `place.rs` will
//! otherwise think it is a mistake. It is not. Arranging panes is exactly what a
//! program does when it *lives inside* a multiplexer, and every one of its ten
//! herdr methods has its call site in `panel/` or `detail/` — wsp's own drawing
//! code, standing in a pane, moving its own furniture. A backend with no screen
//! does not implement a degraded version of this port; it implements none of it,
//! and the view layer that would have produced a [`Spec`] produces a page or a
//! canvas instead. That asymmetry is the point:
//!
//! - `place.rs` is what wsp needs from whatever **runs its agents**.
//! - This is what wsp needs from whatever **draws its screen**.
//!
//! The two touch in exactly one place, and it is named below.
//!
//! # The ten verbs, and what they become
//!
//! `place.rs` lists this port's inheritance explicitly: `pane.split`,
//! `pane.close`, `pane.focus`, `pane.layout`, `pane.swap`, `pane.send_text`,
//! `tab.create`, `tab.close`, `tab.focus`, `workspace.focus`. Plus half of
//! `pane.rename`: 081 demoted the claim-key use into the herdr adapter and left
//! the labelling done by `panel/install.rs:116`, `panel/verbs.rs:722`, `:777`,
//! `:841`, `:922` and `detail/editors.rs:372` here — so this port needs a label
//! verb of its own and does not inherit one.
//!
//! Eleven inbound, eight out, and the collapses are the design rather than
//! tidying:
//!
//! | herdr method | fate |
//! |---|---|
//! | `pane.list` + `pane.layout` | merged — [`Arrange::look`] |
//! | `pane.split` + `tab.create` | merged — [`Arrange::open`], which takes a [`Where`] |
//! | `pane.close` + `tab.close` | merged — [`Arrange::close`] |
//! | `pane.focus` + `tab.focus` + `workspace.focus` | merged — [`Arrange::focus`] |
//! | `pane.swap` | ported — [`Arrange::swap`] |
//! | `pane.send_text` | **split** — [`Arrange::run`] and [`Arrange::send`] |
//! | `pane.rename` (labelling half) | ported — [`Arrange::label`] |
//!
//! **The three focus verbs are one verb** because no caller has ever wanted one
//! of them. `panel/verbs.rs:698` calls `tab.focus` then `pane.focus`;
//! `panel/run.rs:333` calls `workspace.focus` then `pane.focus`. Both are one
//! sentence — *put this in front of me* — spelled twice because herdr's tree has
//! three levels. Which levels have to be walked to make a pane visible is the
//! backend's business and nobody else's.
//!
//! **`pane.list` and `pane.layout` are one read** because they are never wanted
//! apart. `panel/install.rs:80` lists panes and then asks for the layout to find
//! the widest, having learned that "first in the list" is an arbitrary answer;
//! `detail/run.rs:415` reads the rects to find the leftmost sibling, and says
//! why in a line that is really this port's charter: *the geometry comes from
//! herdr rather than from the layout this code happens to build, so it stays
//! true if that layout ever changes.* A reading with no rect cannot answer
//! either question.
//!
//! **`pane.send_text` splits** because its two uses are different verbs wearing
//! one coat. `exec wsp view\n` is *bring a pane up* — [`Arrange::run`] — and a
//! backend that starts processes properly has no keystrokes in it at all.
//! `\x03` then `:wq\r` is *type this at whatever is in there* —
//! [`Arrange::send`] — which is how `detail/editors.rs:196` gets `$EDITOR` to
//! leave, and which is irreducibly about a terminal. Keeping them apart is what
//! lets a non-terminal renderer implement seven of the eight and refuse one.
//!
//! # Focus: 081's most debatable line, confirmed and then settled
//!
//! `place.rs` pushed `focus` here, on the grounds that `workspace.focus` is one
//! of these ten, and called it the most debatable line in the file. It is right,
//! and the declarative model makes it more right than the argument it was given.
//!
//! Focus is a statement about *what a person is looking at*. It is meaningless
//! to a supervisor with no screen, so a port that must be implementable without
//! one cannot carry it. `workspace.create`'s `focus` flag and `wsp spawn
//! --focus` look like part of placing work only because herdr creates and
//! shows in one call.
//!
//! 081 priced the split at one extra call and a possible flicker. Under a spec
//! that price is not paid at all: focus is not an extra call bolted onto the end
//! of spawn, it is [`Spec::focus`] — one line of the desired state, stated with
//! everything else and applied once. Declaring the destination is strictly
//! better than sequencing two calls and hoping the gap is short. Which is also
//! why nothing should reach for focus to *drive* anything: a state applied once
//! is honest about being a destination, whereas a call sequenced mid-operation
//! moves a person's screen to get something done and reads to them as the screen
//! misbehaving (decision of 2026-08-17).
//!
//! This section once ended *"and `place.rs` needs no change"*, which stopped
//! being true when `spawn` migrated and `place.rs` grew `Order::show`. Settled on
//! t-260817-008: this port keeps the question and the argument above stands, and
//! `Order::show` is scaffolding held by the *other* port only for as long as this
//! one has no implementor to declare into. Its removal condition is written where
//! the field is defined, and it is this port's arrival that fires it. Do not read
//! that field as a second opinion about where focus lives; there is one.
//!
//! # The one place the two ports touch
//!
//! A pane can be [`Body::Filled`] by an agent, and this port does not start
//! agents — `Place::open` and `Place::start` do. The plan says [`Op::Open`] with
//! a body of [`Filler::Agent`], and the *caller* sends the pane half here and
//! the agent half there. That is only possible because a plan is data; see "who
//! executes what" below.
//!
//! It leaves one constraint on any backend implementing both ports: **a `Seat`
//! and a [`Surface`] naming the same physical pane must be the same string.**
//! herdr satisfies it for free — both are `w0:p3` — and a backend that does not
//! must supply its own mapping, because otherwise wsp can start an agent it
//! cannot then focus. Neither port parses the other's ids; they only have to
//! agree.
//!
//! # The spec: rendered and filled
//!
//! A view is a set of panes. Every pane is one of two kinds and the distinction
//! is the whole design:
//!
//! - **[`Body::Rendered`]** — wsp draws it from state. The panel, the detail
//!   pane, the board. Content is a pure function of a snapshot, so the pane can
//!   be redrawn at any time, thrown away, or sent somewhere else to be drawn
//!   (t-260816-082).
//! - **[`Body::Filled`]** — something else occupies it: an agent, or a command.
//!   wsp says what should be there and never draws it.
//!
//! They have opposite lifetimes, and every rule below is a consequence. A
//! rendered pane is cheap and disposable; a filled pane contains a live process
//! doing somebody's work. Reopening a rendered pane costs a redraw. Reopening a
//! filled one destroys what was in it.
//!
//! It also decides what a spec's *silence* means, which is the question every
//! declarative system has to answer and most answer by accident. **A spec is
//! complete over rendered panes and partial over filled ones.** wsp draws every
//! rendered pane, so it knows the whole set and leaving one out means take it
//! down. It draws none of the filled ones — an agent it never opened, a shell a
//! person made — so leaving one out means nothing whatsoever. One rule, read in
//! two directions, and both readings are the conservative one for the kind of
//! pane they apply to.
//!
//! There is a third [`Body`], and it exists so that rule 2 has a way to say yes:
//! [`Body::Done`] is the view layer stating that a slot's occupant is finished
//! and the pane may go. Without it there is no legitimate way to close a filled
//! pane, and with it there is exactly one.
//!
//! # Six rules the reconciler has to get right
//!
//! Each has a precedent in this tree or in strata, and each is a test at the
//! bottom of this file.
//!
//! **1. Identity is wsp's, not the runtime's, and never positional.** A
//! [`Slot`] is wsp's own name for a pane — `panel`, `view`, `board` — and the
//! binding from slot to [`Surface`] is wsp's record, which is what
//! `panels.json` already is (`panel/install.rs:25`). herdr reissues pane ids
//! across restarts, which is why claims are keyed on workspaces and carry a
//! label to re-find one. Strata answered the same problem with D-55: an id is
//! an index *plus a generation*, so storage is recycled and the id never is.
//! [`Slot`] carries a generation for exactly that reason — a slot closed and
//! remade is not the slot it replaces, so an op planned against the old one is
//! refused rather than applied to the new pane. And nothing is identified by
//! position: `panel/install.rs:80` had to stop taking "first in the list" as
//! "the pane to split off", which is D-53's positional-identity failure in
//! miniature.
//!
//! **2. Reconcile, never rebuild.** `panel/install.rs` opens with the scar:
//! *never `layout.apply`: herdr rebuilds the whole tree from that call and every
//! pane in it gets a fresh terminal, which takes down any agent running in the
//! workspace.* So the diff moves and swaps, and never recreates. Two
//! consequences that are easy to miss:
//!
//! - Changing what a rendered pane shows is [`Op::Repoint`], not close-and-open.
//!   `panel/verbs.rs:887` already does this — the target goes through a file the
//!   view polls, because *the alternative, killing and relaunching, would blink
//!   the pane on every press of a key whose whole job is to be cheap*.
//! - A filled pane is not closed, it is [`Op::Dismiss`]ed: you ask its occupant
//!   to leave and the pane goes when it does (`detail/editors.rs:196`). Forcing
//!   is a second, separate decision by a person who has now said it twice.
//!
//! **3. Absence of evidence must not create.** This is t-260816-058 inverted and
//! it is the frightening one. That bug reaped every binding when herdr answered
//! with an empty pane list. A reconciler handed the same empty list concludes
//! that *nothing exists* and opens everything — twenty-two agents, all at once,
//! all real. The guard is the one `reconcile` already has
//! (`cmd_agent::may_reap`): a world must have been **heard from** before
//! anything is done about it. So [`World::heard`] and [`World::silent`] are
//! different constructors and there is no `Default`, because the whole cost of
//! this bug is a `Vec::new()` that looks like an answer.
//!
//! **4. In flight is a state, not a boolean.** herdr's agent lifecycle has three
//! windows between "start it" and "talk to it"; `place.rs` names the first one
//! [`crate::place::State::Starting`] because reading it as idle sent a work
//! order into a pane still drawing its banner. A pane has the same window: the
//! split returned, the process has not drawn, and a reconciler that reads *not
//! visible yet* as *not started* opens a second one. [`World::pending`] is that
//! state and it has a deadline — [`FILL_PENDING_MS`] for a filled pane, taken
//! from the 30s ceiling `spawn` already waits, and [`DRAW_PENDING_MS`] for a
//! rendered one, which is a split and a command and has failed if it has not
//! appeared in two seconds.
//!
//! **5. Drift is a decision, not a bug.** People move, close and resize panes by
//! hand. [`Drift`] is the three policies, and the house answer is [`Drift::Adopt`]
//! — wsp has `adopt` as a verb and a README section, and `panel/install.rs:142`
//! already adopts by hand: *a pane labelled `wsp` is ours even when the state
//! file lost track of it — a crash between the split and the save would
//! otherwise install twice.*
//!
//! **Size gets one policy, chosen rather than inherited.** A ratio is a
//! creation-time argument, a pane a person dragged wider stays wider, and
//! [`plan`] emits nothing for it under any policy — because re-imposing a width
//! fights the person, which is this rule applied rather than a limit handed to
//! us. A declarative window manager that silently re-imposes geometry is
//! resented for it.
//!
//! The first draft of this file said something stronger and false: that herdr
//! has no resize outside `layout.apply`. It has `pane.resize` — direction,
//! amount, no rebuild — and the reason it looked absent is the trap worth more
//! than the fact. **`src/herdr.rs` is not a measurement of herdr.** wsp wraps
//! about a third of the socket's methods, and `pane.resize`, `pane.move`,
//! `pane.zoom`, `pane.neighbor`, `pane.edges`, `pane.focus_direction` and
//! `pane.send_keys` are all in `herdr api schema --json` and in none of wsp.
//! Reading capability off wsp's wrapper turns "we never needed it" into "it
//! cannot be done", and a port is exactly the wrong file to make that mistake
//! in: the whole document is a claim about what a backend must be able to do.
//! Anything below asserting that the runtime cannot do something was checked
//! against the schema, not against `herdr.rs`. (Found by the coordination seat
//! reviewing this file, 2026-08-17.)
//!
//! **6. Two writers.** herdr's own UI changes panes underneath us — the same
//! shape as the shared `.git/index` problem the store already had, and not a
//! race that can be locked away. The answer is stated rather than hoped for:
//! **the runtime wins, always.** wsp is authoritative over slots and never over
//! the world. The loser finds out at the next [`Arrange::look`], because a plan
//! is computed against a fresh reading and never against a remembered one, and
//! because every op that touches an existing pane carries the [`Surface`] it was
//! planned against ([`Op::expects`]) so the executor can refuse when the world
//! has moved on. A plan is a compare-and-set, not a script.
//!
//! # Who executes what
//!
//! [`plan`] returns data, and that data fans out to three executors. This is the
//! reason the plan is separate from its execution, and it is what makes the six
//! rules unit tests rather than intentions:
//!
//! - ops naming a pane go to [`Arrange`] — this port;
//! - an [`Op::Open`] whose body is [`Filler::Agent`] has its agent half done by
//!   the place-work port;
//! - [`Op::Repoint`] goes to wsp itself, because what a rendered pane shows is
//!   wsp's own state (`detail.json`) and no backend has ever heard of it.
//!
//! The reconciler needs none of the three, which is why it can be tested with no
//! herdr, no processes and no clock.
//!
//! # What is deliberately absent
//!
//! - **A layout tree.** [`Spec`] is a flat list, each pane naming what it hangs
//!   off ([`Anchor`]). The runtime's geometry really is a binary split tree, but
//!   wsp only ever attaches one pane to another — the widest, the last, its own
//!   — and a tree would make position primary when rule 1 says identity is.
//!   The shape is derivable from the anchors; the slots are not derivable from
//!   the shape.
//! - **An op that fills a pane that already exists.** It was written and taken
//!   out, because [`plan`] can never honestly emit one. A pane that has just
//!   been made is filled as part of [`Op::Open`]; a pane whose occupant has
//!   *died* looks identical from here to one that is busy, because whether an
//!   agent is alive is a `Place::census` question and this port cannot see it.
//!   The trigger to add it is therefore named rather than guessed: when the
//!   observe half (t-260816-059) can tell wsp that a seat has gone
//!   `place::State::Gone` while its pane remains, a spec can ask for it to be
//!   refilled without reopening — and until then it would be a verb with no
//!   caller, which is the tax this store keeps warning about.
//! - **An op that swaps two panes.** Same test, same answer. Swapping is how a
//!   new pane gets onto the correct side ([`Arrange::swap`] is in the trait for
//!   it), but the plan cannot name a surface that does not exist yet — and
//!   between two panes that *do* exist, a swap would be re-imposing position,
//!   which is the same choice rule 5 makes about width and is made the same way.
//!   The spec describes structure at creation and not geometry afterwards, which
//!   is why there is nothing in it for such an op to be planned from.
//! - **`zoom`.** It was tried and reverted: *a zoom is not a bigger pane, it is
//!   a display mode over the whole tab, set by one pane and outliving it*
//!   (`panel/verbs.rs:687`). A spec cannot describe it, which is the strongest
//!   argument yet that it was the wrong primitive.
//! - **A resize verb, and a move verb.** Re-decided in the open once
//!   `pane.resize` and `pane.move` turned out to exist, because the first
//!   answer rested on them not existing. Same answer, and now for reasons that
//!   can be argued with:
//!
//!   *No caller.* Nothing in `panel/` or `detail/` resizes or moves a pane
//!   today. 081's rule applies unchanged — a verb with no caller is the tax this
//!   store keeps warning about — and it applies harder to a capability that has
//!   sat in the socket unused for as long as wsp has been talking to it.
//!
//!   *The spec has nothing to plan one from.* Ratios are creation-time and there
//!   is no position in a [`Spec`] at all, so a resize op would have no desired
//!   state to be a diff against. Adding the verb honestly means adding geometry
//!   to the spec first, and that is the decision to weigh — not this one.
//!
//!   *And the API shape is a cost worth knowing before anyone signs up for it.*
//!   `pane.resize` takes a direction and an amount: it is a nudge, not an
//!   assignment. There is no "make this pane 22%". Converging on a declared
//!   ratio means read the layout, compute a delta, nudge, read again — against a
//!   runtime that can answer `changed: false, reason: "unchanged"` when it will
//!   not move, so the loop needs a stop condition or it spins. That is a real
//!   design, and it should arrive with the caller that wants it.
//!
//!   The trigger, named rather than guessed: **a spec that has to restore a
//!   width somebody temporarily changed** — a zoom being undone, an edit tab
//!   collapsing back — is the first thing that cannot be expressed without it.
//!   When that arrives, geometry enters the spec, this verb comes with it, and
//!   the convergence rule above is part of the same change.
//! - **Reading a pane's contents.** `pane.read` is the observe half's
//!   (t-260816-059), and no arrange call site uses it.
//! - **Migrating the call sites.** t-260816-061's, exactly as it was for 081.
//!   There is no herdr adapter here either, on purpose: an adapter written
//!   before its two callers are known is a third opinion about what they need.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use crate::place::Agent;

/// How long a filled pane may be in flight before the plan will try again.
///
/// Rule 4's deadline for the expensive kind. Not guessed: `spawn` already polls
/// a single seat for up to 30 seconds before it gives up on a cold start
/// (`cmd_spawn.rs`), so this is the one number in the tree that has already been
/// measured against a person's patience.
pub const FILL_PENDING_MS: u64 = 30_000;

/// How long a rendered pane may be in flight before the plan will try again.
///
/// A split and a command. If it has not appeared in two seconds the call failed
/// rather than being slow, and the cost of trying again is a redraw.
pub const DRAW_PENDING_MS: u64 = 2_000;

/// The runtime's handle for a pane, carried and never parsed.
///
/// The same discipline as `place::Seat` and for the same reason: herdr's are
/// `w0:p3` and `w0:p3@mb2`, a browser's would be a node id, and a seam that
/// passes ids around as strings works where one that assumes herdr's *shape* of
/// id does not.
///
/// Unlike a `Seat`, a surface is **not** durable and this port does not ask it
/// to be. herdr reissues pane ids across restarts; that is precisely why
/// identity is a [`Slot`] and a surface is only ever the answer to "where is
/// this slot right now".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Surface(String);

impl Surface {
    pub fn new(id: impl Into<String>) -> Surface {
        Surface(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// wsp's own name for a pane, plus a generation.
///
/// Rule 1. The name is what the view layer and a person both use — `panel`,
/// `view`, `board`, `agent:t-260816-084`. The generation is strata's D-55
/// applied one level down: a slot that is closed and opened again is a
/// *different* slot, so an op planned against the old one cannot be applied to
/// the new pane by a reconciler that ran a beat late.
///
/// It matters here more than it looks. herdr reissues pane ids on restart, so
/// the same string can name somebody else's shell tomorrow; the binding
/// slot → surface is only trustworthy if a stale slot cannot silently match a
/// fresh one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Slot {
    pub name: String,
    pub gen: u32,
}

impl Slot {
    pub fn new(name: impl Into<String>) -> Slot {
        Slot { name: name.into(), gen: 0 }
    }
    /// The same name, one life later. What the reconciler binds after it has
    /// opened a replacement pane.
    pub fn next(&self) -> Slot {
        Slot { name: self.name.clone(), gen: self.gen.saturating_add(1) }
    }
    /// Whether two slots are the same *thing*, ignoring which life it is on.
    /// The view layer writes specs in these terms; the reconciler does not.
    pub fn same_name(&self, other: &Slot) -> bool {
        self.name == other.name
    }
}

impl std::fmt::Display for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.name, self.gen)
    }
}

/// Where a pane is on screen, in cells.
///
/// Read, never written; see rule 5. It is here because two questions need it and
/// neither can be answered from a list: which pane is widest, and which of these
/// is on the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// One pane, as the backend currently sees it.
///
/// `tab` is not decoration. `pane.list` is per *workspace*, and every ownership
/// bug in this tree came from forgetting it: `panel/verbs.rs:658` had the
/// sidebar and the fullscreen panel closing each other's detail pane, and `↵`
/// retargeting a pane in a tab nobody was looking at. A pane in another tab is
/// somebody else's pane.
#[derive(Debug, Clone, Default)]
pub struct Live {
    pub surface: Surface,
    pub tab: String,
    pub label: String,
    pub rect: Rect,
}

/// What should be drawn in a rendered pane.
///
/// Deliberately not a command. `view` names which of wsp's views this is —
/// `panel`, `detail`, `board` — and `target` is what it is currently pointed at.
/// A terminal renderer maps `view` to an argv; a canvas maps it to a component.
/// That mapping is part 4's, and keeping it out of here is what makes the
/// renderer replaceable (t-260816-082).
///
/// The split between the two fields is load-bearing rather than tidy: `view`
/// changes mean a new pane, `target` changes mean [`Op::Repoint`], and today's
/// code already lives on that line — `panel/verbs.rs:887` writes the target to a
/// file the view polls precisely so that retargeting costs no process churn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Content {
    pub view: String,
    pub target: Option<String>,
}

impl Content {
    pub fn new(view: impl Into<String>) -> Content {
        Content { view: view.into(), target: None }
    }
    pub fn on(view: impl Into<String>, target: impl Into<String>) -> Content {
        Content { view: view.into(), target: Some(target.into()) }
    }
}

/// What occupies a pane wsp does not draw.
///
/// Ed's sentence names both: *what should fill the rest, via an agent or a
/// command*. They are one enum because the reconciler treats them identically —
/// both are live processes with somebody's work in them — and two variants
/// because they are executed by different things: an agent goes to the
/// place-work port, a command goes to [`Arrange::run`].
#[derive(Debug, Clone)]
pub enum Filler {
    /// Started through `place::Place`, never by this port.
    Agent(Agent),
    /// A command line. The editors, a board, a shell.
    Command(Vec<String>),
}

/// Written out rather than derived, because `place::Agent` has no equality and
/// should not grow one for this: two fillers are the same when they would start
/// the same thing, which is a question this port asks and the place-work port
/// does not.
///
/// `place::Agent::args` is deliberately not read here, and that is a decision
/// rather than an oversight. A running claude and a spec asking for a claude
/// with one more flag on it are the same filler, because the alternative is a
/// reconcile that kills somebody's session to restart it with a different
/// preamble — a change to `agent_commands::TRIM` would then close every agent on the
/// seat. Flags decide what a *new* agent starts with, and nothing else.
impl PartialEq for Filler {
    fn eq(&self, other: &Filler) -> bool {
        match (self, other) {
            (Filler::Agent(a), Filler::Agent(b)) => a.kind == b.kind && a.name == b.name,
            (Filler::Command(a), Filler::Command(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for Filler {}

/// What is in a pane, and therefore what may be done to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// wsp draws it from state. Cheap, disposable, redrawable anywhere.
    Rendered(Content),
    /// Somebody else's process is in it. Closing it destroys work.
    Filled(Filler),
    /// The occupant is finished and the pane may go.
    ///
    /// The only way a filled pane is ever taken down, and the reason rule 2 has
    /// an "unless": a spec that cannot say this can only close panes by
    /// accident.
    Done,
}

impl Body {
    pub fn is_filled(&self) -> bool {
        matches!(self, Body::Filled(_))
    }
    /// How long this kind of pane may be in flight. Rule 4.
    pub fn pending_ms(&self) -> u64 {
        match self {
            Body::Rendered(_) => DRAW_PENDING_MS,
            _ => FILL_PENDING_MS,
        }
    }
}

/// Which way a new pane goes, and what herdr's `ratio` means.
///
/// The trap is in the ratio and it has bitten twice. **The target keeps
/// `ratio`; the new pane gets the remainder.** `panel/install.rs:155` learned it
/// by putting the sidebar on the wrong side at the wrong width and then swapping
/// the two panes to fix it, and `panel/verbs.rs:862` encodes the same fact as
/// `1.0 / (n - i + 1)` when peeling even columns off the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dir {
    #[default]
    Right,
    Down,
}

/// What a new pane hangs off.
///
/// Rule 1's other half: a pane is placed *relative to something named*, never at
/// a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// The root pane of a tab this spec is making. Boards and pop-outs.
    Root,
    /// The widest pane that is not one of ours.
    ///
    /// The panel's rule, named rather than reimplemented. It is measured, not
    /// guessed: splitting off a leftover view pane gave the panel 22% of an
    /// already narrow column — seven usable characters (`panel/install.rs:149`).
    Widest,
    /// One of our own slots.
    Slot(Slot),
}

/// One pane wsp wants.
#[derive(Debug, Clone)]
pub struct Want {
    pub slot: Slot,
    pub body: Body,
    /// What a person reading a list of panes has to go on. Not a key — rule 1 —
    /// except in the one place a key is all that survived: see [`Drift::Adopt`].
    pub label: String,
    pub at: Anchor,
    pub dir: Dir,
    /// Creation-time only, by choice: see rule 5. herdr can resize a pane and
    /// wsp declines to, because a width a person changed is a width they meant.
    pub ratio: f64,
}

impl Want {
    pub fn new(slot: Slot, body: Body) -> Want {
        Want { slot, body, label: String::new(), at: Anchor::Widest, dir: Dir::Right, ratio: 0.5 }
    }
    pub fn labelled(mut self, label: impl Into<String>) -> Want {
        self.label = label.into();
        self
    }
    pub fn at(mut self, at: Anchor, dir: Dir, ratio: f64) -> Want {
        self.at = at;
        self.dir = dir;
        self.ratio = ratio;
        self
    }
}

/// What to do about a world that has stopped matching the spec.
///
/// Rule 5. Applies to *identity* — a pane of ours we lost track of, a pane
/// somebody closed. Size is not one of the three, and that is a decision rather
/// than a limit: `pane.resize` exists and is not destructive, and wsp still
/// leaves a width alone, because the only policy re-imposing it could implement
/// is one that fights the person holding the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Drift {
    /// Take it back. A live pane carrying our label is our pane, whatever the
    /// binding says. The house answer: `panel/install.rs:142` already does this
    /// by hand, because a crash between the split and the save would otherwise
    /// install a second panel.
    #[default]
    Adopt,
    /// Say so and do nothing. The honest policy for a caller that is not sure
    /// the label is unique — and the one to reach for when a spec starts being
    /// written by something other than wsp.
    Report,
    /// Close it and open ours. Safe for a rendered pane and never for a filled
    /// one, so [`plan`] downgrades it to [`Drift::Report`] rather than taking a
    /// live process down to satisfy a layout.
    Replace,
}

/// The desired state: one tab's worth of panes.
///
/// The tab is the unit because the runtime's read is per workspace and every
/// ownership bug in this tree came from mistaking a pane in another tab for one
/// of ours (`panel/verbs.rs:658`). A workspace with a sidebar, a board and an
/// edit tab is three specs, not one.
#[derive(Debug, Clone)]
pub struct Spec {
    /// Which tab this is about, as the runtime names it. `None` means *the tab
    /// this pane is in*, which is what the panel and the detail view mean.
    pub tab: Option<String>,
    pub panes: Vec<Want>,
    /// What should be in front of the person once this is true. One line, stated
    /// with everything else — see the focus argument in the module docs.
    pub focus: Option<Slot>,
    pub drift: Drift,
}

impl Spec {
    pub fn new(panes: Vec<Want>) -> Spec {
        Spec { tab: None, panes, focus: None, drift: Drift::default() }
    }
    fn want(&self, slot: &Slot) -> Option<&Want> {
        self.panes.iter().find(|w| w.slot.same_name(slot))
    }
}

/// What wsp remembers about one slot: where it is, and whether there is work in
/// it.
///
/// `filled` is not derivable from the world. A pane with an agent in it and a
/// pane with a view in it look identical from outside, and the difference
/// decides whether the pane may be closed — so it is wsp's own record, written
/// when the slot was opened, exactly as `panels.json` records what wsp installed
/// rather than asking herdr what looks like a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub surface: Surface,
    pub filled: bool,
}

/// What we have seen, and what we remember calling it.
///
/// Both halves, because rule 1 makes identity wsp's: the world this port cares
/// about is what the runtime says *plus* the binding from slot to surface, and
/// neither is a fact on its own.
///
/// There is no `Default` and the constructors are the whole point. [`World::heard`]
/// is a reading; [`World::silent`] is not having one. t-260816-058 cost every
/// binding in the store to a `unwrap_or_default()` that turned the second into
/// the first.
#[derive(Debug, Clone)]
pub struct World {
    heard: bool,
    pub live: Vec<Live>,
    /// wsp's record. `panels.json` is today's version of it.
    pub bound: BTreeMap<Slot, Held>,
    /// Slots whose pane has been asked for and not yet seen, and when we asked.
    /// Rule 4.
    pub pending: BTreeMap<Slot, u64>,
    /// Monotonic milliseconds. Passed in so that [`plan`] has no clock.
    pub now: u64,
}

impl World {
    /// A reading. An empty one is a fact: this tab has no panes.
    pub fn heard(live: Vec<Live>) -> World {
        World { heard: true, live, bound: BTreeMap::new(), pending: BTreeMap::new(), now: 0 }
    }

    /// No reading. Nobody answered, or nobody was asked.
    ///
    /// Rule 3. Everything [`plan`] would have created is withheld, and it says
    /// so in a note rather than silently doing nothing.
    pub fn silent() -> World {
        World { heard: false, live: Vec::new(), bound: BTreeMap::new(), pending: BTreeMap::new(), now: 0 }
    }

    pub fn was_heard(&self) -> bool {
        self.heard
    }

    /// Remember a pane wsp draws in.
    pub fn binding(mut self, slot: Slot, surface: Surface) -> World {
        self.bound.insert(slot, Held { surface, filled: false });
        self
    }
    /// Remember a pane with somebody's work in it.
    pub fn filling(mut self, slot: Slot, surface: Surface) -> World {
        self.bound.insert(slot, Held { surface, filled: true });
        self
    }
    pub fn in_flight(mut self, slot: Slot, since: u64) -> World {
        self.pending.insert(slot, since);
        self
    }
    pub fn at(mut self, now: u64) -> World {
        self.now = now;
        self
    }

    fn find(&self, surface: &Surface) -> Option<&Live> {
        self.live.iter().find(|l| &l.surface == surface)
    }

    /// The widest pane that is not one of ours, in this tab.
    ///
    /// `panel/install.rs:80`, as a pure function. Ours means bound to a slot —
    /// which is stronger than the label test the code uses today, and is what
    /// rule 1 buys.
    fn widest_foreign(&self, tab: Option<&str>) -> Option<&Live> {
        let ours: BTreeSet<&Surface> = self.bound.values().map(|h| &h.surface).collect();
        self.live
            .iter()
            .filter(|l| tab.is_none_or(|t| l.tab == t))
            .filter(|l| !ours.contains(&l.surface))
            .max_by_key(|l| l.rect.w)
    }
}

/// One thing to do. Every variant that touches an existing pane carries the
/// surface it was planned against — rule 6.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Make a pane and put its content in.
    ///
    /// One op rather than three, because a split, a label and a command are
    /// never wanted apart: `panel/install.rs:185`, `panel/verbs.rs:722` and
    /// `panel/verbs.rs:922` all do exactly this sequence, and a pane that
    /// exists with nothing in it is a bare shell somebody has to notice. It
    /// also means the plan never has to name a surface that does not exist yet,
    /// which is the constraint that keeps [`plan`] pure.
    ///
    /// It is the one op with two executors. The pane half is this port's; the
    /// `body` half is the renderer's for [`Body::Rendered`] and the place-work
    /// port's for [`Filler::Agent`] — wsp does not start agents through a
    /// screen. Getting the pane onto the correct side at the correct width may
    /// take an [`Arrange::swap`] as well as an [`Arrange::open`], and that is
    /// the executor's business rather than the plan's.
    ///
    /// `off` is the pane it is split from, already resolved from the spec's
    /// [`Anchor`] against the world — so an [`Anchor::Widest`] that has changed
    /// its mind between planning and execution is caught by rule 6 like
    /// everything else. `None` means a tab of its own.
    Open { slot: Slot, off: Option<Surface>, dir: Dir, ratio: f64, label: String, body: Body },
    /// This existing pane is the slot's pane. Costs nothing and destroys
    /// nothing; see [`Drift::Adopt`].
    Adopt { slot: Slot, surface: Surface },
    /// Put the right words on a pane.
    Label { slot: Slot, surface: Surface, label: String },
    /// Point a rendered pane at something else, without touching the process in
    /// it. Executed by wsp, not by a backend.
    Repoint { slot: Slot, surface: Surface, target: Option<String> },
    /// Put this in front of the person.
    Focus { slot: Slot, surface: Surface },
    /// Take a rendered pane down.
    Close { slot: Slot, surface: Surface },
    /// Ask a filled pane's occupant to leave. The pane goes when it does, and
    /// forcing it is a separate decision by a person who has said it twice
    /// (`detail/editors.rs:196`).
    Dismiss { slot: Slot, surface: Surface },
}

impl Op {
    /// The surface this op was planned against, if any.
    ///
    /// Rule 6's compare-and-set. An executor that finds a different world
    /// refuses the op and re-plans; only [`Op::Open`] has nothing to compare,
    /// because it is making something that does not exist — and even then its
    /// `off` is an expectation.
    pub fn expects(&self) -> Option<&Surface> {
        match self {
            Op::Open { off, .. } => off.as_ref(),
            Op::Adopt { surface, .. }
            | Op::Label { surface, .. }
            | Op::Repoint { surface, .. }
            | Op::Focus { surface, .. }
            | Op::Close { surface, .. }
            | Op::Dismiss { surface, .. } => Some(surface),
        }
    }

    /// Whether this op can destroy something a person cares about.
    pub fn is_destructive(&self) -> bool {
        matches!(self, Op::Close { .. } | Op::Dismiss { .. })
    }
}

/// The diff, plus what the reconciler decided not to do.
///
/// Notes are not logging. They are the third drift policy's output and rule 3's
/// only visible effect, and a reconciler that withholds work silently is
/// indistinguishable from one that has nothing to do.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub ops: Vec<Op>,
    pub notes: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
    fn note(&mut self, s: impl Into<String>) {
        self.notes.push(s.into());
    }
}

/// The reconciler: what has to happen for the world to match the spec.
///
/// Pure. No clock, no backend, no processes — the world comes in as data and the
/// plan goes out as data, which is the only way the six rules are testable
/// rather than aspirational. Rule 3 is the one that spawns twenty-two agents if
/// it is wrong, and it is the first line of the function.
///
/// **Order matters and is not alphabetical.** Grow, re-point, then shrink —
/// `detail/run.rs:292` found this the hard way: *a section being moved out of a
/// column that is about to close must reach its new home before its old one
/// goes.* Focus lands after the panes exist, and closes land last so that
/// nothing is destroyed before its replacement is up.
pub fn plan(world: &World, spec: &Spec) -> Plan {
    let mut out = Plan::default();

    // Rule 3. A spec is only actionable against a world we have actually heard
    // from; an unheard world is not an empty one. Nothing below this line runs.
    if !world.was_heard() {
        out.note("nothing heard from the runtime — no panes were opened or closed");
        return out;
    }

    let tab = spec.tab.as_deref();
    let mut opened: Vec<Op> = Vec::new();
    let mut changed: Vec<Op> = Vec::new();
    let mut closed: Vec<Op> = Vec::new();
    // Slots that end this plan with a pane, so the focus line can be resolved.
    let mut placed: BTreeMap<String, Surface> = BTreeMap::new();

    for want in &spec.panes {
        let here = world
            .bound
            .iter()
            .find(|(s, _)| s.same_name(&want.slot))
            .and_then(|(s, held)| world.find(&held.surface).map(|l| (s.clone(), l)));

        match here {
            // The pane exists and is ours. Rule 2: change it in place.
            Some((slot, live)) => {
                placed.insert(slot.name.clone(), live.surface.clone());

                if let Body::Done = want.body {
                    closed.push(Op::Dismiss { slot, surface: live.surface.clone() });
                    continue;
                }
                if !want.label.is_empty() && live.label != want.label {
                    changed.push(Op::Label {
                        slot: slot.clone(),
                        surface: live.surface.clone(),
                        label: want.label.clone(),
                    });
                }
                if let Body::Rendered(content) = &want.body {
                    // A rendered pane is retargeted, never relaunched. There is
                    // deliberately no comparison against what it is *currently*
                    // showing: that lives in wsp's own state, this port cannot
                    // see it, and writing the same target twice costs nothing
                    // while a stale one costs a person looking at the wrong
                    // task.
                    if content.target.is_some() {
                        changed.push(Op::Repoint {
                            slot,
                            surface: live.surface.clone(),
                            target: content.target.clone(),
                        });
                    }
                }
                // Nothing for a live filled pane. Whether the agent in it is
                // still alive is a `Place::census` question and not this port's;
                // see the module docs.
            }

            // No pane. Either it never existed, it is on its way, or somebody
            // closed it.
            None => {
                if let Body::Done = want.body {
                    continue;
                }

                // Rule 4. In flight is a state: a pane asked for and not yet
                // drawn is not a pane that was never asked for.
                if let Some((slot, since)) = world.pending.iter().find(|(s, _)| s.same_name(&want.slot)) {
                    let deadline = since.saturating_add(want.body.pending_ms());
                    if world.now < deadline {
                        out.note(format!("{slot} is still coming up — not opening a second one"));
                        continue;
                    }
                    out.note(format!("{slot} never appeared — opening again"));
                }

                // Rule 5. A pane of ours that the binding lost — a crash between
                // the split and the save, or a restart that reissued the ids.
                // The label is what is left to recognise it by, which is the one
                // place a label is a key and is why the policy is stated rather
                // than assumed.
                let orphan = (!want.label.is_empty())
                    .then(|| {
                        world.live.iter().find(|l| {
                            l.label == want.label
                                && tab.is_none_or(|t| l.tab == t)
                                && !world.bound.values().any(|h| h.surface == l.surface)
                        })
                    })
                    .flatten();

                if let Some(orphan) = orphan {
                    match spec.drift {
                        Drift::Adopt => {
                            placed.insert(want.slot.name.clone(), orphan.surface.clone());
                            changed.push(Op::Adopt {
                                slot: want.slot.clone(),
                                surface: orphan.surface.clone(),
                            });
                            continue;
                        }
                        Drift::Report => {
                            out.note(format!(
                                "a pane labelled {} is already open and is not bound to {} — left alone",
                                want.label, want.slot
                            ));
                            continue;
                        }
                        Drift::Replace => {
                            // Never for a filled pane: a layout is not a reason
                            // to take somebody's process down. Rule 2's floor,
                            // and the policy degrades rather than refusing the
                            // whole plan.
                            if want.body.is_filled() {
                                out.note(format!(
                                    "{} is filled — reporting the stray pane labelled {} rather than replacing it",
                                    want.slot, want.label
                                ));
                                continue;
                            }
                            closed.push(Op::Close {
                                slot: want.slot.clone(),
                                surface: orphan.surface.clone(),
                            });
                        }
                    }
                }

                let off = match &want.at {
                    Anchor::Root => None,
                    Anchor::Slot(s) => {
                        let found = placed
                            .get(&s.name)
                            .cloned()
                            .or_else(|| {
                                world
                                    .bound
                                    .iter()
                                    .find(|(b, _)| b.same_name(s))
                                    .map(|(_, held)| held.surface.clone())
                            });
                        match found {
                            Some(surface) => Some(surface),
                            None => {
                                out.note(format!("{} hangs off {s}, which is not open", want.slot));
                                continue;
                            }
                        }
                    }
                    Anchor::Widest => match world.widest_foreign(tab) {
                        Some(l) => Some(l.surface.clone()),
                        None => {
                            out.note(format!("nothing to split {} off", want.slot));
                            continue;
                        }
                    },
                };

                let slot = world
                    .bound
                    .keys()
                    .find(|s| s.same_name(&want.slot))
                    .map(|s| s.next())
                    .unwrap_or_else(|| want.slot.clone());

                opened.push(Op::Open {
                    slot,
                    off,
                    dir: want.dir,
                    ratio: want.ratio,
                    label: want.label.clone(),
                    body: want.body.clone(),
                });
            }
        }
    }

    // Slots we hold that the spec does not name.
    //
    // The completeness asymmetry, and it follows from the lifetimes rather than
    // from taste: a spec is *complete* over rendered panes and *partial* over
    // filled ones. wsp draws every rendered pane, so it knows the whole set and
    // omitting one means take it down. It draws none of the filled ones, so
    // omitting one means nothing at all — and the only way to end one is for the
    // spec to say so with `Body::Done`.
    for (slot, held) in &world.bound {
        if spec.want(slot).is_some() || world.find(&held.surface).is_none() {
            continue;
        }
        if held.filled {
            out.note(format!("{slot} is not in this spec and has work in it — left alone"));
            continue;
        }
        closed.push(Op::Close { slot: slot.clone(), surface: held.surface.clone() });
    }

    if let Some(f) = &spec.focus {
        match placed.get(&f.name) {
            Some(surface) => {
                changed.push(Op::Focus { slot: f.clone(), surface: surface.clone() });
            }
            None => {
                // Not a failure: the pane is being opened in this same plan, and
                // an [`Op::Open`] carries its own focus by arriving. Saying so
                // costs a line and stops the next reader adding a second focus
                // op that races the split.
                if !opened.iter().any(|op| matches!(op, Op::Open { slot, .. } if slot.same_name(f))) {
                    out.note(format!("{f} is not open — nothing to focus"));
                }
            }
        }
    }

    // Grow, re-point, shrink. `detail/run.rs:292`.
    out.ops.extend(opened);
    out.ops.extend(changed);
    out.ops.extend(closed);
    out
}

/// Why a call did not happen, in wsp's terms.
///
/// Four of these are `place::Refusal`'s and they are deliberately not shared.
/// That enum's [`crate::place::Refusal::NoSeat`] names the *other* port's
/// handle, and a port whose whole justification is that it may name panes must
/// not report a missing pane as a missing seat. Merge them when there is a third
/// port to merge them for; two is a coincidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Nothing answered. `herdr::available()`'s replacement, arriving from the
    /// call that wanted it rather than a guard in front of it.
    Unreachable(String),
    /// No such pane. Rule 6's ordinary outcome: somebody closed it under us.
    Gone(Surface),
    /// The world moved between the plan and the op. The expectation is what the
    /// op carried; re-read and re-plan.
    Moved(Surface),
    /// This renderer cannot do that at all — [`Arrange::send`] on a surface with
    /// no terminal behind it. Distinct from a failure, and not a bug.
    Unsupported(&'static str),
    /// The backend said no, in its own words.
    Backend(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unreachable(w) => write!(f, "nothing answered: {w}"),
            Refusal::Gone(s) => write!(f, "no pane {s}"),
            Refusal::Moved(s) => write!(f, "{s} is not what it was — read again"),
            Refusal::Unsupported(w) => write!(f, "this renderer cannot {w}"),
            Refusal::Backend(w) => write!(f, "{w}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Refusal>;

/// Where a new pane goes, resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Where {
    /// Beside an existing pane. `pane.split`, with the trap in [`Dir`].
    Beside { of: Surface, dir: Dir, ratio: f64 },
    /// A tab of its own, with its root pane. `tab.create`.
    Tab { label: String, env: BTreeMap<String, String> },
}

/// The screen wsp is standing in.
///
/// Eight methods, from eleven herdr ones; the table in the module docs says
/// which and why. Object-safe for the same reason `place::Place` is: the
/// `Remote` decorator holds a `Box<dyn Arrange>` and qualifies ids on the way
/// in, and a renderer is chosen at runtime rather than compiled in.
///
/// Every method takes a [`Surface`] the caller has seen in a [`look`]. There is
/// no method that finds a pane by name, by label or by position — that is rule
/// 1, and putting it in the trait is what stops it being re-litigated at each
/// call site.
///
/// [`look`]: Arrange::look
pub trait Arrange {
    /// Every pane this renderer has, where it is, and what it says.
    ///
    /// One read, because the two halves are never wanted apart. An error is not
    /// an empty list — [`World::heard`] versus [`World::silent`] is the whole of
    /// rule 3, and a caller that flattens the two has reintroduced
    /// t-260816-058.
    fn look(&self) -> Result<Vec<Live>>;

    /// Make a pane, beside another or in a tab of its own.
    ///
    /// Returns when the pane exists. Whatever is going to draw in it has not
    /// drawn yet — those are different moments, and a caller that conflates them
    /// opens the pane twice.
    fn open(&self, at: &Where) -> Result<Surface>;

    /// Put words on a pane, for a person to read.
    ///
    /// The half of `pane.rename` the place-work port did not take. Cosmetic to
    /// the runtime and load-bearing to us: it is the only thing left to
    /// recognise a pane by when the binding is lost, which is exactly what
    /// [`Drift::Adopt`] relies on.
    fn label(&self, surface: &Surface, label: &str) -> Result<()>;

    /// Bring something up in a pane.
    ///
    /// Today this is `exec`ing a command line over the pane's shell, which is
    /// also why quitting takes the pane with it rather than leaving a bare
    /// prompt (`panel/install.rs:111`). A renderer that starts processes
    /// properly does so; one that draws in-process ignores the argv and draws.
    fn run(&self, surface: &Surface, argv: &[String]) -> Result<()>;

    /// Type at whatever is in a pane.
    ///
    /// The irreducibly terminal verb, and the only one a non-terminal renderer
    /// is expected to refuse with [`Refusal::Unsupported`]. It exists because
    /// getting `$EDITOR` to save and leave is done by sending it the keys — with
    /// a pause between the interrupt and the command, because vim throws away
    /// type-ahead it takes an interrupt on (`detail/editors.rs:233`).
    fn send(&self, surface: &Surface, text: &str) -> Result<()>;

    /// Exchange two panes' positions, touching neither process.
    ///
    /// The only move the port takes, out of the three the socket offers —
    /// `pane.move` and `pane.resize` are real and are left out on the grounds in
    /// the module docs, so this one is here for a reason rather than by default.
    /// It is here because `pane.split` puts the new pane in the remainder — so a sidebar arrives on the wrong side at the
    /// wrong width, and swapping is what lands it in the narrow slot without
    /// disturbing either process (`panel/install.rs:155`).
    fn swap(&self, a: &Surface, b: &Surface) -> Result<()>;

    /// Put a pane in front of the person, walking whatever tree it takes.
    ///
    /// Three herdr calls behind one verb; see the module docs. A caller has
    /// never wanted "focus the tab but not the pane".
    fn focus(&self, surface: &Surface) -> Result<()>;

    /// Take a pane down.
    ///
    /// Panes only. There is no tab verb because a tab whose last pane closes
    /// goes with it, which is the behaviour `detail/run.rs:97` already depends
    /// on, and because a spec is a statement about panes.
    ///
    /// This is for panes wsp draws. A pane with somebody's process in it is
    /// asked to leave instead — [`Op::Dismiss`] — and only forced when a person
    /// has said so twice.
    fn close(&self, surface: &Surface) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(id: &str, label: &str, w: u32) -> Live {
        Live {
            surface: Surface::new(id),
            tab: "t0".into(),
            label: label.into(),
            rect: Rect { x: 0, y: 0, w, h: 40 },
        }
    }

    fn rendered(name: &str, view: &str) -> Want {
        Want::new(Slot::new(name), Body::Rendered(Content::new(view))).labelled(name)
    }

    fn filled(name: &str) -> Want {
        Want::new(Slot::new(name), Body::Filled(Filler::Command(vec!["claude".into()])))
            .labelled(name)
    }

    fn opens(plan: &Plan) -> usize {
        plan.ops.iter().filter(|o| matches!(o, Op::Open { .. })).count()
    }

    /// Rule 3, and the reason this file exists in this order. An unheard world
    /// is not an empty one: handed silence and a spec of twenty-two panes, a
    /// reconciler that treats the two alike starts twenty-two agents.
    #[test]
    fn a_world_nobody_answered_for_creates_nothing() {
        let spec = Spec::new((0..22).map(|i| filled(&format!("agent{i}"))).collect());
        let p = plan(&World::silent(), &spec);
        assert!(p.is_empty(), "silence opened {} panes", opens(&p));
        assert_eq!(p.notes.len(), 1, "and it must say so rather than look idle");
    }

    /// The other side of it, or rule 3 would just be "never do anything". A
    /// reading of no panes is a fact, and a fact is actionable.
    #[test]
    fn a_reading_of_nothing_is_a_fact_and_does_create() {
        let spec = Spec::new(vec![rendered("panel", "panel").at(Anchor::Root, Dir::Right, 0.22)]);
        let p = plan(&World::heard(Vec::new()), &spec);
        assert_eq!(opens(&p), 1);
    }

    /// Rule 1. The panel is bound to a surface, and the world reorders itself
    /// underneath: the pane that was first is now last and a stranger is widest.
    /// Nothing re-points, because nothing was ever identified by position.
    #[test]
    fn a_pane_is_found_by_its_slot_and_never_by_where_it_sits() {
        let spec = Spec::new(vec![rendered("panel", "panel")]);
        let before = World::heard(vec![live("p1", "panel", 20), live("p2", "shell", 100)])
            .binding(Slot::new("panel"), Surface::new("p1"));
        let after = World::heard(vec![live("p2", "shell", 100), live("p1", "panel", 20)])
            .binding(Slot::new("panel"), Surface::new("p1"));

        assert!(plan(&before, &spec).is_empty());
        assert!(plan(&after, &spec).is_empty(), "reordering the world is not a change to it");
    }

    /// Rule 1's generation, which is strata's D-55 one level down. herdr
    /// reissues pane ids across restarts, so a slot that has been remade must
    /// not be matched by an op planned against the one it replaced.
    #[test]
    fn a_slot_remade_is_not_the_slot_it_replaces() {
        let first = Slot::new("view");
        let second = first.next();
        assert_ne!(first, second);
        assert!(first.same_name(&second), "but it is still the view pane");
        assert_eq!(second.to_string(), "view#1");

        // The pane was closed under us, so the plan opens a replacement — and
        // binds it to a later generation.
        let spec = Spec::new(vec![rendered("view", "detail")]);
        let world = World::heard(vec![live("p9", "shell", 80)])
            .binding(first.clone(), Surface::new("p1"));
        match plan(&world, &spec).ops.first() {
            Some(Op::Open { slot, .. }) => assert_eq!(slot, &second),
            other => panic!("expected an open, got {other:?}"),
        }
    }

    /// Rule 2. A rendered pane pointed somewhere else is re-pointed, never
    /// closed and reopened — the difference is a blink on every keypress, and
    /// for the pane below it, a process that did not need restarting.
    #[test]
    fn showing_something_else_repoints_and_never_reopens() {
        let spec = Spec::new(vec![Want::new(
            Slot::new("view"),
            Body::Rendered(Content::on("detail", "t-260816-084")),
        )
        .labelled("view")]);
        let world = World::heard(vec![live("p2", "view", 60)])
            .binding(Slot::new("view"), Surface::new("p2"));
        let p = plan(&world, &spec);

        assert_eq!(opens(&p), 0);
        assert!(!p.ops.iter().any(|o| o.is_destructive()));
        match p.ops.as_slice() {
            [Op::Repoint { surface, target, .. }] => {
                assert_eq!(surface, &Surface::new("p2"));
                assert_eq!(target.as_deref(), Some("t-260816-084"));
            }
            other => panic!("expected one repoint, got {other:?}"),
        }
    }

    /// Rule 2's hard half, and the one that costs somebody their afternoon. A
    /// spec is a statement about the slots it names: a rendered pane it stops
    /// naming is disposable, and a filled one is not.
    #[test]
    fn a_pane_with_work_in_it_survives_a_spec_that_forgot_it() {
        let world = World::heard(vec![live("p1", "view", 60), live("p2", "agent", 60)])
            .binding(Slot::new("view"), Surface::new("p1"))
            .filling(Slot::new("agent"), Surface::new("p2"));

        // Neither slot is named. The rendered pane goes, because wsp drew it and
        // can draw it again. The filled one stays: forgetting to mention
        // somebody's agent is not a decision to end it.
        let p = plan(&world, &Spec::new(Vec::new()));
        assert_eq!(
            p.ops,
            vec![Op::Close { slot: Slot::new("view"), surface: Surface::new("p1") }]
        );
        assert_eq!(p.notes.len(), 1, "and the one it left alone is said out loud");
    }

    /// And the one legitimate way a filled pane ends: the spec says so. Without
    /// [`Body::Done`] there is no way to close one on purpose, which is how
    /// designs end up closing them by accident.
    #[test]
    fn a_filled_pane_ends_when_the_spec_says_its_occupant_is_finished() {
        let world = World::heard(vec![live("p2", "agent", 60)])
            .binding(Slot::new("agent"), Surface::new("p2"));
        let spec = Spec::new(vec![Want::new(Slot::new("agent"), Body::Done).labelled("agent")]);
        assert_eq!(
            plan(&world, &spec).ops,
            vec![Op::Dismiss { slot: Slot::new("agent"), surface: Surface::new("p2") }],
            "asked to leave, not closed — the pane goes when its occupant does"
        );
    }

    /// Rule 4. The split returned and the process has not drawn yet. Reading
    /// "not visible" as "not started" is how one agent becomes two.
    #[test]
    fn a_pane_on_its_way_is_not_a_pane_to_open_again() {
        let spec = Spec::new(vec![filled("agent").at(Anchor::Widest, Dir::Right, 0.5)]);
        let world = World::heard(vec![live("p1", "shell", 100)])
            .in_flight(Slot::new("agent"), 1_000)
            .at(1_000 + FILL_PENDING_MS - 1);
        let p = plan(&world, &spec);
        assert_eq!(opens(&p), 0, "a second agent, on a cold start");
        assert_eq!(p.notes.len(), 1);

        // Past the deadline it is tried again — once. A deadline shorter than a
        // person's patience and longer than a cold start.
        let world = world.at(1_000 + FILL_PENDING_MS);
        assert_eq!(opens(&plan(&world, &spec)), 1);
    }

    /// The two deadlines are different because the two kinds are. A rendered
    /// pane that has not appeared in two seconds failed; an agent may still be
    /// starting at twenty.
    #[test]
    fn a_drawn_pane_is_given_less_time_than_a_started_one() {
        let world = World::heard(vec![live("p1", "shell", 100)])
            .in_flight(Slot::new("x"), 0)
            .at(DRAW_PENDING_MS);
        assert_eq!(opens(&plan(&world, &Spec::new(vec![rendered("x", "detail")]))), 1);
        assert_eq!(opens(&plan(&world, &Spec::new(vec![filled("x")]))), 0);
    }

    /// Rule 5. A pane of ours the binding lost — a crash between the split and
    /// the save, or a restart that reissued the ids. Three policies, three
    /// different plans, and the default is the one the panel already implements
    /// by hand.
    #[test]
    fn a_pane_of_ours_we_lost_track_of_is_a_decision() {
        let world = World::heard(vec![live("p7", "panel", 30), live("p1", "shell", 100)]);
        let mut spec = Spec::new(vec![rendered("panel", "panel")]);

        spec.drift = Drift::Adopt;
        assert_eq!(
            plan(&world, &spec).ops,
            vec![Op::Adopt { slot: Slot::new("panel"), surface: Surface::new("p7") }],
            "installing a second panel beside the first is the bug this prevents"
        );

        spec.drift = Drift::Report;
        let p = plan(&world, &spec);
        assert!(p.is_empty());
        assert_eq!(p.notes.len(), 1);

        spec.drift = Drift::Replace;
        assert!(plan(&world, &spec).ops.iter().any(|o| matches!(o, Op::Close { .. })));
    }

    /// Replace is safe for something wsp drew and never for something with a
    /// process in it. The policy degrades rather than refusing the plan, because
    /// a layout is not a reason to take somebody's work down.
    #[test]
    fn replace_will_not_take_down_a_pane_that_is_filled() {
        let world = World::heard(vec![live("p7", "agent", 30), live("p1", "shell", 100)]);
        let mut spec = Spec::new(vec![filled("agent")]);
        spec.drift = Drift::Replace;

        let p = plan(&world, &spec);
        assert!(!p.ops.iter().any(|o| o.is_destructive()));
        assert_eq!(p.notes.len(), 1, "and it says why, or it looks like it did nothing");
    }

    /// Rule 5's other half, and a **policy** rather than a limit — the
    /// distinction is the whole value of this test, because the reason is what a
    /// future reader will check before changing the behaviour.
    ///
    /// herdr *can* resize: `pane.resize` takes a direction and an amount and
    /// rebuilds nothing. wsp declines to use it, because a width somebody
    /// dragged is a width they meant, and a reconciler that puts it back is one
    /// they will turn off. So a ratio applies once, at creation, and a resized
    /// pane produces no ops under any of the three policies.
    ///
    /// If that is ever revisited, revisit it here: the assertion is the
    /// decision, and the module docs carry what adding the verb would cost.
    #[test]
    fn a_pane_a_person_resized_is_left_alone() {
        let spec = Spec::new(vec![rendered("panel", "panel").at(Anchor::Widest, Dir::Right, 0.22)]);
        for drift in [Drift::Adopt, Drift::Report, Drift::Replace] {
            let mut spec = spec.clone();
            spec.drift = drift;
            // 0.22 was asked for; the person has made it half the screen.
            let world = World::heard(vec![live("p1", "panel", 60), live("p2", "shell", 60)])
                .binding(Slot::new("panel"), Surface::new("p1"));
            assert!(plan(&world, &spec).is_empty(), "{drift:?} re-imposed a width");
        }
    }

    /// Rule 6. herdr's own UI moves panes underneath us and that is not a race
    /// wsp can lock away, so every op that touches something that already exists
    /// carries the surface it was planned against. An executor that finds
    /// anything else refuses and re-reads.
    #[test]
    fn every_op_says_what_it_expected_to_find() {
        let world = World::heard(vec![live("p1", "shell", 100), live("p2", "view", 40)])
            .binding(Slot::new("view"), Surface::new("p2"));
        let mut spec = Spec::new(vec![
            Want::new(Slot::new("view"), Body::Rendered(Content::on("detail", "t-1"))).labelled("view"),
            rendered("panel", "panel").at(Anchor::Widest, Dir::Right, 0.22),
        ]);
        spec.focus = Some(Slot::new("view"));

        let p = plan(&world, &spec);
        assert!(!p.is_empty());
        for op in &p.ops {
            assert!(op.expects().is_some(), "{op:?} would act on whatever it found");
        }
    }

    /// Grow, re-point, shrink — `detail/run.rs` found the order by moving a
    /// section into a column that had already closed. Opens first, destruction
    /// last, always.
    #[test]
    fn nothing_is_taken_down_before_its_replacement_is_up() {
        let world = World::heard(vec![live("p1", "shell", 100), live("p2", "overview", 40)])
            .binding(Slot::new("col2"), Surface::new("p2"));
        let spec = Spec::new(vec![
            rendered("col1", "editor").at(Anchor::Widest, Dir::Right, 0.5),
            Want::new(Slot::new("col2"), Body::Done).labelled("overview"),
        ]);

        let p = plan(&world, &spec);
        let first_close = p.ops.iter().position(|o| o.is_destructive()).unwrap();
        let last_open = p.ops.iter().rposition(|o| matches!(o, Op::Open { .. })).unwrap();
        assert!(last_open < first_close, "{:?}", p.ops);
    }

    /// A pane hangs off something named, and the name may belong to a pane this
    /// same plan is opening. The order in the spec is the order they appear, so
    /// a chain resolves in one pass rather than needing a second reconcile.
    #[test]
    fn a_pane_can_hang_off_one_this_plan_has_not_opened_yet() {
        let world = World::heard(vec![live("p1", "panel", 100)])
            .binding(Slot::new("panel"), Surface::new("p1"));
        let spec = Spec::new(vec![
            rendered("panel", "panel"),
            rendered("view", "detail").at(Anchor::Slot(Slot::new("panel")), Dir::Down, 0.45),
        ]);
        match plan(&world, &spec).ops.as_slice() {
            [Op::Open { off, dir, .. }] => {
                assert_eq!(off.as_ref(), Some(&Surface::new("p1")));
                assert_eq!(*dir, Dir::Down);
            }
            other => panic!("expected one open beside the panel, got {other:?}"),
        }

        // And an anchor that is not open says so instead of guessing. Splitting
        // off whatever happened to be there is how the panel once ended up
        // seven characters wide.
        let spec = Spec::new(vec![
            rendered("panel", "panel"),
            rendered("view", "detail").at(Anchor::Slot(Slot::new("nothing")), Dir::Down, 0.45),
        ]);
        let p = plan(&world, &spec);
        assert!(p.is_empty());
        assert_eq!(p.notes.len(), 1);
    }

    /// The widest pane that is not ours — the panel's rule, and the reason it is
    /// not "the first one": splitting the leftover view pane gave the panel 22%
    /// of an already narrow column. Ours means bound, which is stronger than the
    /// label test the code uses today.
    #[test]
    fn a_panel_splits_the_widest_pane_that_is_not_ours() {
        let world = World::heard(vec![
            live("p1", "shell", 80),
            live("p2", "view", 120),
            live("p3", "shell", 100),
        ])
        .binding(Slot::new("view"), Surface::new("p2"));
        let spec = Spec::new(vec![
            rendered("view", "detail"),
            rendered("panel", "panel").at(Anchor::Widest, Dir::Right, 0.22),
        ]);
        match plan(&world, &spec).ops.as_slice() {
            [Op::Open { off, .. }] => assert_eq!(
                off.as_ref(),
                Some(&Surface::new("p3")),
                "p2 is wider, and is ours — splitting it is the seven-character panel"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// A pane in another tab is somebody else's pane. `pane.list` is per
    /// workspace, and reading the workspace's first view pane had the sidebar
    /// and the fullscreen panel closing each other's.
    #[test]
    fn a_pane_in_another_tab_is_not_ours_to_adopt() {
        let mut elsewhere = live("p9", "view", 60);
        elsewhere.tab = "t1".into();
        let world = World::heard(vec![live("p1", "shell", 100), elsewhere]);
        let mut spec = Spec::new(vec![rendered("view", "detail").at(Anchor::Widest, Dir::Right, 0.45)]);
        spec.tab = Some("t0".into());

        let p = plan(&world, &spec);
        assert_eq!(opens(&p), 1, "the pane two tabs over is not this spec's");
        assert!(!p.ops.iter().any(|o| matches!(o, Op::Adopt { .. })));
    }

    /// Focus is one line of the desired state rather than a call bolted onto the
    /// end of a sequence — which is what makes 081's flicker argument moot.
    #[test]
    fn focus_is_stated_with_everything_else() {
        let world = World::heard(vec![live("p2", "view", 60)])
            .binding(Slot::new("view"), Surface::new("p2"));
        let mut spec = Spec::new(vec![rendered("view", "detail")]);
        spec.focus = Some(Slot::new("view"));
        assert_eq!(
            plan(&world, &spec).ops,
            vec![Op::Focus { slot: Slot::new("view"), surface: Surface::new("p2") }]
        );
    }

    /// Nothing in the port reads an id. The closest a test can get: a surface is
    /// whatever the renderer said, unaltered, including the `@mb2` suffix the
    /// `Remote` decorator puts on it.
    #[test]
    fn a_surface_is_carried_rather_than_parsed() {
        for id in ["w0:p3", "w0:p3@mb2", "7", "node-14", ""] {
            assert_eq!(Surface::new(id).as_str(), id);
            assert_eq!(Surface::new(id).to_string(), id);
        }
        assert!(Surface::default().is_empty());
        assert_ne!(Surface::new("w0:p3"), Surface::new("w0:p3@mb2"));
    }
}
