//! A backend that answers the socket from a state we choose.
//!
//! Not a multiplexer, and the name is the scope risk this file is written
//! against: **no PTYs, no processes, no terminal**. It answers the protocol and
//! holds a state. The moment it needs to host a shell it is herdr and we have
//! lost.
//!
//! # The argument is states, not speed
//!
//! A sandbox herdr comes up in 0.1s and `wsp verify` is seven seconds warm, so
//! herdr is not what is slow. What a real herdr cannot do is *be in a state we
//! choose*, and every expensive bug in this store was a state: an empty pane
//! list reaping every binding (t-260816-058), one `pane.exited` cascade
//! clearing every binding on the machine, a machine that stops answering
//! mid-tick, pane ids reissued across a restart, twenty-two workspaces and four
//! agents. None of those can be manufactured on a live herdr, and all of them
//! are two lines here.
//!
//! # The division of labour, and the trap it exists to avoid
//!
//! A fake encodes our *belief* about herdr, and a fake that is wrong about a
//! behaviour makes tests green on a lie, silently and for ever. t-260816-056's
//! log carries a correction where two probes "verified" that a headless session
//! loads no plugins and it was false — the redirect that made the probe safe
//! suppressed the evidence. So the division is explicit:
//!
//! - **the fake is for wsp's reaction to a state**;
//! - **`wsp sandbox` stays the contract check against real behaviour**.
//!
//! And everything here asserting what herdr *does* was recorded from a live one
//! rather than hand-written. herdr 0.7.5, in a sandbox session, 2026-08-17: the
//! id shapes, the reply envelopes, the four error codes, the split arithmetic,
//! the subscription refusal and the fact that a pane with no agent reports
//! `agent_status: "unknown"` and no `agent` field at all are all transcripts of
//! that session, and each is marked *recorded* where it appears. Anything not
//! marked that way is a decision of the fake's own, and is marked as one.
//!
//! # It speaks the ports' vocabulary and generates herdr's
//!
//! The state is [`Seat`], [`State`], [`Agent`] and [`Event`] — `place.rs`'s
//! words — plus [`Rect`], which is `arrange.rs`'s. herdr's JSON is a *dialect*
//! computed from that state on the way out ([`of_state`]), never a canned reply
//! hand-written into a test. Two things follow, and they are the reason the
//! task asked for it this way round:
//!
//! - a wrong belief about herdr shows up as **one mapping function** that
//!   `place_herdr::state_of_agent` documents the inverse of, rather than as a
//!   string in a fixture nobody re-reads;
//! - a second backend that is not herdr is a second dialect over the same
//!   state, which is what the seam is for.
//!
//! `of_state` and `place_herdr::state_of_agent` are a round trip, and
//! [`tests::the_dialect_is_the_inverse_of_the_readings_wsp_makes`] holds them
//! to it — including the one place the trip is lossy, which is herdr's blind
//! spot and not ours.
//!
//! # What it found on the first run, which is the argument for it
//!
//! Writing the dialect as the inverse of the port's own reading and asserting
//! the round trip failed immediately, and the failure was in the port rather
//! than in the fake. Both were reported on t-260816-081 rather than fixed here,
//! and both were **repaired on 2026-08-17 by t-260816-061**, which moved the
//! reading into `place_herdr` as it migrated the first call site onto the port.
//! They are kept here because the fake is what caught them and what holds them
//! shut:
//!
//! 1. **The port read a value herdr never sends.** It decided an agent was
//!    starting from `ready == Some(false)`. Recorded against a live Claude Code
//!    on 2026-08-17: for 3.3 seconds after `agent.start`, `agent.get` answers
//!    `agent_status: "idle"`, `launch_pending: true` and **no
//!    `interactive_ready` field at all**; then the two swap. Absence is the
//!    signal and `false` never appears. So the port mapped the launch window to
//!    `Idle`, whose `will_take_a_prompt` says yes — the `agent_not_ready` bug
//!    the port's own documentation exists to prevent. `place_herdr` now reads
//!    `launch_pending`.
//! 2. **The listing asymmetry is between panes and agents, not one and many.**
//!    `pane.list` rows have no `interactive_ready` in the schema. `agent.list`
//!    carries the same `AgentInfo` as `agent.get` and was recorded carrying
//!    `interactive_ready: true`. wsp failed to see it because `herdr::Pane` did
//!    not parse the field; it does now, and a census reads its seats from one
//!    call and its states from the other.
//!
//! A third: `agent.start` returns *before* the agent exists — its reply names
//! no agent and carries `launch_pending: true` — so `Place::start`'s "returns
//! when the agent exists" is a promise its herdr adapter has to keep for it, and
//! does, by waiting.
//!
//! And a fourth, which is t-260817-010 and which this fake **missed** until
//! 2026-08-17 because it modelled that reply and not the reads after it. For
//! roughly the next six tenths of a second `agent.get` answers the same way: a
//! record that exists and names nothing. The adapter was waiting for a record
//! rather than for an agent, so it returned inside that window and the caller
//! read the very same record as a seat that had emptied — a live Claude Code,
//! declared dead, three spawns out of three. [`Stage::unnamed`] is that window,
//! and it is on by default: a fake that skips the hard part of a launch agrees
//! with whatever the code does.
//!
//! # Behind a socket, and what that costs
//!
//! It binds a unix socket and speaks newline-delimited JSON, so it exercises
//! `herdr.rs`'s real wire code and works against an unmodified binary: point
//! `HERDR_SOCKET_PATH` at it and the daemon, the panel and the storyboard all
//! run against it unchanged. It therefore needs no trait and can land before
//! t-260816-061 rather than after.
//!
//! The cost is worth naming, because it is a limit on what this can ever prove.
//! `arrange.rs` splits `pane.send_text` into two verbs — [`Arrange::run`] is
//! *bring a pane up* and `send` is *type at whatever is in there* — and **that
//! split is invisible at the wire**: both arrive as `pane.send_text`. A fake
//! behind a socket can record that something was typed and can never say which
//! verb the caller meant. Only an in-process double can check that, and that is
//! t-260816-061's to build if it is worth building. Recorded here rather than
//! guessed at later.
//!
//! That question was answered for the *place-work* port while it had six verbs:
//! all six reached the wire as six distinguishable methods, so migrating `spawn`
//! onto it needed no double at all — `place_herdr`'s tests drive wsp's own client
//! against this fake.
//!
//! `Place::stop` is the seventh and it ends that. It is `pane.close` on this
//! backend and so is the arrange port's close, so **the two collide at the wire
//! exactly as `run` and `send` do**: this file can record that a pane was taken
//! away and cannot say whether the caller meant *end that agent's work* or *tidy
//! that viewport*. Both collisions are the same shape — one herdr method, two
//! wsp verbs — and a test that has to tell them apart wants an in-process double
//! rather than a socket.
//!
//! [`Arrange::run`]: crate::arrange::Arrange::run
//!
//! # What a seat is, and the join neither port makes
//!
//! One row of the state is a [`Spot`]: the place-work half (seat, agent, state,
//! session) and the arrange half (label, rect, and which screen it is on) in
//! one record, because on herdr they are one pane. `arrange.rs` requires it —
//! *a `Seat` and a `Surface` naming the same physical pane must be the same
//! string* — so the fake holds one id per spot and hands it out under both
//! names.
//!
//! **Neither port names the join, and this is the finding to report upward.**
//! `place::Seated` has no idea *where* a seat is; `arrange::Live` has a tab and
//! no idea *who* is in it. A backend implementing both has to keep a mapping
//! that neither contract mentions, and the fake is the first thing that has had
//! to. It is not obviously a defect — the ports are deliberately separate — but
//! it means "the same string" is doing more work than one line of prose in
//! `arrange.rs`, and a second backend will hit it before it writes a line of
//! its own code.
//!
//! # Silence has two clocks
//!
//! [`Quiet`] is the state a real herdr cannot be put in on demand and the one
//! that has cost the most: **an absence is not a fact**. It has two forms
//! deliberately, because they arrive at different times and the difference is
//! where the bugs live — a hang-up arrives at once and looks like an answer to
//! code that is not careful, and a machine that never answers costs the caller
//! its whole timeout. `sync`'s "`Err` is not an empty list" and `reconcile`'s
//! unreachable-is-not-empty are both about exactly this, and until now neither
//! could be tested against a backend that did it.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::arrange::Rect;
use crate::place::{Agent, Event, Seat, Seated, State};

/// What the fake was asked, in wsp's words rather than herdr's.
///
/// The recording is in port vocabulary on purpose. A test that asserts a JSON
/// method name is a test that has to be rewritten when the backend changes,
/// which is the coupling the ports exist to remove — so `stand_in`'s "here is
/// what arrived on the socket" becomes "here is what was asked of the backend",
/// and a test reads as a sentence about behaviour.
///
/// The mapping is [`Verb::of_method`], and it is many-to-one in both
/// directions: herdr answers a census with three different methods, and it
/// answers two different port verbs with one `pane.send_text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verb {
    /// Reserve somewhere an agent can run. `workspace.create`.
    Open,
    /// Start an agent in a seat. `agent.start`.
    Start,
    /// Give the agent a sentence to act on. `agent.prompt`.
    Tell,
    /// Press submit on a sentence that arrived and was not taken.
    /// `agent.send_keys`.
    ///
    /// Its own verb rather than [`Verb::Send`], which is typing at whatever is
    /// in a pane: this one is addressed to the agent, and a recording that
    /// could not tell the two apart could not show that a handover needed
    /// rescuing.
    Nudge,
    /// What is happening in one seat. `agent.get`.
    Ask,
    /// Every seat there is. `pane.list`, `workspace.list`, `agent.list`.
    Census,
    /// Where the panes are. `pane.layout`.
    Look,
    /// Hold the connection open and push what happens. `events.subscribe`.
    Watch,
    /// Make a pane beside another, or a tab of its own. `pane.split`,
    /// `tab.create`.
    Split,
    /// Put words on something a person reads. `pane.rename`,
    /// `workspace.rename`.
    Label,
    /// Type at whatever is in a pane. `pane.send_text`.
    ///
    /// Both halves of the arrange port's split arrive here; see the module
    /// docs.
    Send,
    /// Exchange two panes' positions. `pane.swap`.
    Swap,
    /// Put something in front of the person. `pane.focus`, `tab.focus`,
    /// `workspace.focus`.
    Focus,
    /// Take a pane down. `pane.close`, `tab.close`, `workspace.close`.
    ///
    /// Both the arrange port's close and the place-work port's
    /// [`crate::place::Place::stop`] arrive here, because on herdr they are the
    /// same method; see the module docs on what a socket cannot see.
    Close,
    /// Read what is on a screen. `pane.read`.
    Peek,
    /// Metadata wsp pushes and never reads back. `pane.report_metadata`,
    /// `workspace.report_metadata`.
    ///
    /// Not a port verb in either port — `place.rs` deletes both — and it is
    /// here so that a test can assert wsp *stopped* pushing them.
    Meta,
}

impl Verb {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verb::Open => "open",
            Verb::Start => "start",
            Verb::Tell => "tell",
            Verb::Nudge => "nudge",
            Verb::Ask => "ask",
            Verb::Census => "census",
            Verb::Look => "look",
            Verb::Watch => "watch",
            Verb::Split => "split",
            Verb::Label => "label",
            Verb::Send => "send",
            Verb::Swap => "swap",
            Verb::Focus => "focus",
            Verb::Close => "close",
            Verb::Peek => "peek",
            Verb::Meta => "meta",
        }
    }

    /// Which port verb a herdr method is, or `None` for one wsp has never
    /// called.
    ///
    /// The table is the fake's whole surface, and it is short because wsp wraps
    /// about a third of herdr's eighty-nine methods. A method absent from here
    /// is answered with `unsupported_method` rather than something plausible —
    /// see [`Fake::answer`].
    pub fn of_method(method: &str) -> Option<Verb> {
        Some(match method {
            "workspace.create" => Verb::Open,
            "agent.start" => Verb::Start,
            "agent.prompt" => Verb::Tell,
            "agent.send_keys" => Verb::Nudge,
            "agent.get" => Verb::Ask,
            "pane.list" | "workspace.list" | "agent.list" | "pane.get" => Verb::Census,
            "pane.layout" => Verb::Look,
            "events.subscribe" => Verb::Watch,
            "pane.split" | "tab.create" => Verb::Split,
            "pane.rename" | "workspace.rename" => Verb::Label,
            "pane.send_text" => Verb::Send,
            "pane.swap" => Verb::Swap,
            "pane.focus" | "tab.focus" | "workspace.focus" => Verb::Focus,
            "pane.close" | "tab.close" | "workspace.close" => Verb::Close,
            "pane.read" => Verb::Peek,
            "pane.report_metadata" | "workspace.report_metadata" => Verb::Meta,
            _ => return None,
        })
    }
}

/// One thing the fake was asked to do.
///
/// `said` is whatever the verb carries that a person would want to read back —
/// the sentence for [`Verb::Tell`], the label for [`Verb::Label`], the agent
/// kind for [`Verb::Start`]. Empty where the verb carries nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub verb: Verb,
    pub seat: Option<Seat>,
    pub said: String,
}

impl std::fmt::Display for Asked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.verb.as_str())?;
        if let Some(s) = &self.seat {
            write!(f, " {s}")?;
        }
        if !self.said.is_empty() {
            write!(f, " {:?}", self.said)?;
        }
        Ok(())
    }
}

/// Whether the backend is answering at all, and how it fails when it is not.
///
/// Two forms, because they arrive at different times. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quiet {
    /// Answering normally.
    #[default]
    No,
    /// Accepts the connection and hangs up without a word. Arrives at once, and
    /// is what a server being restarted under a caller looks like.
    HangsUp,
    /// Accepts the connection and never says anything. Costs the caller its
    /// whole timeout, and is the machine that stops answering mid-tick.
    Never,
}

/// One seat, and where it is on a screen.
///
/// Both ports' halves in one record, because on herdr they are one pane; see
/// the module docs on the join neither port makes.
#[derive(Debug, Clone, Default)]
pub struct Spot {
    /// The durable handle, and also the surface. Opaque: nothing here parses
    /// it, and [`tests::a_seat_the_fake_did_not_invent_is_still_a_seat`] holds
    /// that to ids no herdr would ever issue.
    pub seat: Seat,
    /// What a person reading a list of seats has to go on.
    pub label: String,
    pub cwd: String,
    pub agent: Agent,
    pub state: State,
    /// The agent's own session id, where the backend knows one — Claude Code's,
    /// which is what ties a seat to the transcript a TTY-less agent leaves.
    pub session: String,
    /// Which screen this seat is on. herdr's workspace; `arrange::Live` has no
    /// word for it and `place::Seated` has no field for it.
    pub space: String,
    /// Which tab within that screen. `arrange::Live::tab`.
    pub tab: String,
    pub rect: Rect,
    /// Metadata pushed at this seat. Kept because a real herdr echoes it back
    /// in `pane.list` (recorded), and because a test asserting wsp has *stopped*
    /// pushing tokens needs somewhere for them to fail to arrive.
    pub tokens: BTreeMap<String, String>,
}

/// Written out rather than derived, for the reason `arrange::Filler` gives:
/// `place::Agent` has no equality and should not grow one for a comparison only
/// a test asks for. Two spots are the same when they would read the same over
/// the wire, so `args` counts — this is not `Filler`'s question about whether
/// two fillers would start the same thing.
impl PartialEq for Spot {
    fn eq(&self, other: &Spot) -> bool {
        self.seat == other.seat
            && self.label == other.label
            && self.cwd == other.cwd
            && self.agent.kind == other.agent.kind
            && self.agent.name == other.agent.name
            && self.agent.args == other.agent.args
            && self.state == other.state
            && self.session == other.session
            && self.space == other.space
            && self.tab == other.tab
            && self.rect == other.rect
            && self.tokens == other.tokens
    }
}

impl Spot {
    /// A seat with nothing in it: a terminal somebody opened.
    pub fn empty(seat: &str) -> Spot {
        Spot { seat: Seat::new(seat), state: State::Empty, ..Spot::default() }
    }

    /// A seat with an agent in it, in whatever state you name.
    pub fn agent(seat: &str, kind: &str, name: &str, state: State) -> Spot {
        Spot {
            seat: Seat::new(seat),
            agent: Agent { kind: kind.into(), name: name.into(), args: Vec::new() },
            state,
            ..Spot::default()
        }
    }

    pub fn labelled(mut self, label: impl Into<String>) -> Spot {
        self.label = label.into();
        self
    }
    pub fn at(mut self, cwd: impl Into<String>) -> Spot {
        self.cwd = cwd.into();
        self
    }
    pub fn on(mut self, space: impl Into<String>, tab: impl Into<String>) -> Spot {
        self.space = space.into();
        self.tab = tab.into();
        self
    }
    pub fn sized(mut self, rect: Rect) -> Spot {
        self.rect = rect;
        self
    }
    pub fn session(mut self, id: impl Into<String>) -> Spot {
        self.session = id.into();
        self
    }

    /// The census row for this spot.
    pub fn seated(&self) -> Seated {
        Seated {
            seat: self.seat.clone(),
            label: self.label.clone(),
            cwd: self.cwd.clone(),
            agent: self.agent.clone(),
            state: self.state,
            session: self.session.clone(),
        }
    }
}

/// The whole world the fake is answering out of.
///
/// Plain data with no socket in it, so a permutation can be built, diffed and
/// asserted on without anything being bound — which is what makes the state
/// machine testable rather than the transport.
#[derive(Debug, Clone)]
pub struct Stage {
    pub spots: Vec<Spot>,
    pub quiet: Quiet,
    /// Whether an agent that has just been started reaches [`State::Idle`] by
    /// itself.
    ///
    /// `true` is herdr's happy path and the default, so `wsp spawn` against a
    /// fake completes. `false` leaves every started agent in
    /// [`State::Starting`] until something says otherwise, which is the window
    /// `agent.prompt` refuses in and the one a real herdr passes through in
    /// half a second whether you wanted it to or not.
    pub settle: bool,
    /// Whether a prompt the agent accepts actually starts a turn.
    ///
    /// `true` is herdr's happy path and the default. `false` is
    /// robustness-035: `agent.prompt` answers `ok`, the sentence sits in the
    /// composer unsent and the agent stays [`State::Idle`] — which is what
    /// `wsp spawn` reported as a success for a night, and what nothing could
    /// reproduce without a socket that could lie in this particular way. A
    /// `send_keys` still rescues it, exactly as pressing return by hand did.
    pub takes: bool,
    /// How many `agent.get` reads answer with a record that **names no agent**
    /// after `agent.start`, before detection catches up.
    ///
    /// The window this fake used to skip, and skipping it is what let
    /// t-260817-010 live in `place_herdr::start` for as long as it did. Recorded
    /// against herdr 0.7.5 in a sandbox on 2026-08-17, polling `agent.get` every
    /// 150ms from the moment `agent.start` was sent: at 70ms and at 210ms the
    /// reply is a record carrying `launch_pending: true`, `agent_status:
    /// "unknown"` and the name wsp asked for, with **no `agent` field at all**;
    /// `"agent": "claude"` first appears at 620ms.
    ///
    /// So there are three readings across a launch and not two — nothing, then
    /// something with no name, then a named agent still coming up — and the
    /// middle one is indistinguishable from an empty seat to everything in wsp
    /// that reads it. Two is the default because two is what was recorded, and
    /// because a default of nought is the fake agreeing with the bug.
    pub unnamed: u32,
    /// What is left of [`Stage::unnamed`] for the agent most recently started.
    ///
    /// One counter for the whole stage rather than one per seat: a launch window
    /// is three tenths of a second long and nothing starts two agents inside
    /// one. If something ever does, this is the line to grow.
    launching: u32,
    /// The screen the panes are laid out in. Nothing reads it but the layout
    /// arithmetic.
    pub area: Rect,
    /// Which seat is in front of the person.
    pub focused: Option<Seat>,
    /// Verbs to refuse, and the wsp-side reason.
    ///
    /// Keyed on the port verb rather than the method, so a test says "this
    /// backend will not take a prompt" rather than naming `agent.prompt`.
    pub refuse: BTreeMap<Verb, Snub>,
    /// Where the next invented id comes from. herdr numbers workspaces from one
    /// and panes within a workspace from one (recorded), and the fake does the
    /// same so that ids in a sandbox read the way ids read everywhere else.
    next_space: u32,
}

/// Why the fake will not do something, in wsp's terms.
///
/// A narrower thing than `place::Refusal` and deliberately not it:
/// `Refusal::Unreachable` is [`Quiet`]'s business — a backend that is not
/// answering cannot tell you it is not answering — and `Refusal::NoSeat` is a
/// fact about the state rather than a script. What is left is the three a test
/// wants to *impose*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Snub {
    /// The agent is not in a state to be told anything. `agent_not_ready`.
    NotReady,
    /// The seat is not ready to have an agent started in it. `agent_pane_busy`
    /// — the shell race `place_herdr::launch` retries for five seconds.
    Busy,
    /// The backend said no, in its own words.
    Backend(String),
}

impl Default for Stage {
    fn default() -> Stage {
        Stage {
            spots: Vec::new(),
            quiet: Quiet::No,
            settle: true,
            takes: true,
            unnamed: 2,
            launching: 0,
            area: Rect { x: 0, y: 0, w: 120, h: 40 },
            focused: None,
            refuse: BTreeMap::new(),
            next_space: 1,
        }
    }
}

impl Stage {
    pub fn new() -> Stage {
        Stage::default()
    }

    /// A stage with these seats in it, ids and all.
    pub fn of(spots: Vec<Spot>) -> Stage {
        let mut stage = Stage::default();
        for spot in spots {
            stage.put(spot);
        }
        stage
    }

    /// Add a seat, filling in whatever it did not say.
    ///
    /// A spot with no id gets one; a spot with no screen gets one of its own,
    /// which is what a workspace per task looks like. A spot that named both is
    /// left exactly as it was, because the ids are the caller's to choose — see
    /// the note on opacity in [`Spot::seat`].
    pub fn put(&mut self, mut spot: Spot) -> Seat {
        if spot.space.is_empty() {
            spot.space = format!("w{}", self.next_space);
            self.next_space += 1;
        }
        if spot.tab.is_empty() {
            spot.tab = format!("{}:t1", spot.space);
        }
        if spot.seat.is_empty() {
            let n = self.spots.iter().filter(|s| s.space == spot.space).count() + 1;
            spot.seat = Seat::new(format!("{}:p{n}", spot.space));
        }
        if spot.rect == Rect::default() {
            spot.rect = self.area;
        }
        let seat = spot.seat.clone();
        match self.spots.iter_mut().find(|s| s.seat == seat) {
            Some(there) => *there = spot,
            None => self.spots.push(spot),
        }
        seat
    }

    pub fn find(&self, seat: &Seat) -> Option<&Spot> {
        self.spots.iter().find(|s| &s.seat == seat)
    }
    fn find_mut(&mut self, seat: &Seat) -> Option<&mut Spot> {
        self.spots.iter_mut().find(|s| &s.seat == seat)
    }

    /// Every seat, as the place-work port would see them.
    pub fn census(&self) -> Vec<Seated> {
        self.spots.iter().map(Spot::seated).collect()
    }

    /// What changed between one state of the world and another, as events.
    ///
    /// The whole permutation driver: hand the fake a new stage and the
    /// subscribers are told what moved. It is a pure function so the hard part
    /// — deciding what an absence means — is a unit test rather than something
    /// that only shows up over a socket.
    ///
    /// Order matters and is the order a backend would raise them in: things
    /// that appeared, then things that changed, then things that went. A
    /// consumer that reaped on `Closed` before reading `Opened` would see a
    /// replaced seat as a gone one.
    pub fn diff(before: &Stage, after: &Stage) -> Vec<Event> {
        let mut out = Vec::new();
        for spot in &after.spots {
            match before.find(&spot.seat) {
                None => {
                    out.push(Event::Opened(spot.seat.clone()));
                    if spot.state.is_running() {
                        out.push(Event::Started(spot.seat.clone()));
                    }
                }
                Some(was) if was.state != spot.state => {
                    let seat = spot.seat.clone();
                    match (was.state.is_running(), spot.state) {
                        (false, s) if s.is_running() => out.push(Event::Started(seat)),
                        // An agent that has stopped is the one transition with
                        // a verb of its own, because it is the clause of "tell
                        // me when it stops" that no poll can carry.
                        (true, State::Gone | State::Empty) => out.push(Event::Stopped(seat)),
                        (_, s) => out.push(Event::Moved(seat, s)),
                    }
                }
                Some(_) => {}
            }
        }
        for spot in &before.spots {
            if after.find(&spot.seat).is_none() {
                out.push(Event::Closed(spot.seat.clone()));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The dialect: herdr's words, computed from the state
// ---------------------------------------------------------------------------

/// The reading a herdr would give of a seat: what it calls the agent, its
/// status, and whether it will take a prompt.
///
/// The inverse of [`crate::place_herdr::state_of_agent`], and the one function in this file that
/// is a *claim about herdr* rather than a translation. Every line of it was
/// recorded from herdr 0.7.5 on 2026-08-17 except where marked:
///
/// - a pane with no agent reports `agent_status: "unknown"` and carries **no
///   `agent` field at all** — not `"idle"`, which is what a reader of the
///   adapter's reading might assume from the order of its arms;
/// - a plugin-reported agent comes back from `agent.get` with no
///   `interactive_ready` field, so its absence is not evidence of anything;
/// - `Starting` is `agent_status: "idle"` with `launch_pending` true and
///   `interactive_ready` **absent** — never `false`. Recorded across the whole
///   launch window of a real Claude Code, which took 3.3 seconds of looking
///   exactly like an idle agent;
/// - `Gone` has no reading of its own. herdr cannot distinguish an exited
///   agent's pane from a shell, so this answers exactly what `Empty` answers,
///   and the loss is pinned in a test rather than smoothed over;
/// - `Blocked` is `agent_status: "blocked"`, read off herdr 0.8.0's schema
///   (`api/schema/common.rs:151`) rather than off a running one, and marked as
///   such because that is a weaker claim than the lines above it.
///
/// **`done` has no `Spot` and deliberately never will.** herdr sends it, and it
/// is `idle` with a flag saying no human has looked at the pane since
/// (`app/api_helpers.rs:104`) — a fact about a viewer's window, not about an
/// agent. The adapter reads it as `Idle` (`place_herdr::of_status`), so a state
/// here that produced it would be a state wsp cannot round-trip, and this
/// function's whole value is that it is an exact inverse.
///
/// The `Starting` line is the one that cost something to get right, and it is
/// why this function returns `Option<bool>` rather than `bool`: absence and
/// `false` are different readings, herdr only ever sends the first, and the
/// port was written against the second until this caught it. See
/// [`tests::the_dialect_is_the_inverse_of_the_readings_wsp_makes`].
fn of_state(spot: &Spot) -> (Option<&str>, &'static str, Option<bool>) {
    let name = match spot.agent.kind.trim() {
        "" => None,
        kind => Some(kind),
    };
    match spot.state {
        State::Empty | State::Gone => (None, "unknown", None),
        State::Starting => (name, "idle", None),
        State::Idle => (name, "idle", Some(true)),
        State::Working => (name, "working", Some(true)),
        State::Blocked => (name, "blocked", Some(true)),
        State::Unknown => (name, "unknown", None),
    }
}

/// One pane, as `pane.list` and `pane.get` render it.
///
/// The shape is a transcript: `terminal_id`, `revision` and `scroll` are here
/// because a real reply carries them, and leaving them out would let something
/// start depending on their absence.
fn pane_json(spot: &Spot) -> Value {
    let (agent, status, _) = of_state(spot);
    let mut v = json!({
        "pane_id": spot.seat.as_str(),
        "terminal_id": format!("term_{}", spot.seat),
        "workspace_id": spot.space,
        "tab_id": spot.tab,
        "focused": false,
        "cwd": spot.cwd,
        "foreground_cwd": spot.cwd,
        "agent_status": status,
        "revision": 0,
        "scroll": { "offset_from_bottom": 0, "max_offset_from_bottom": 0, "viewport_rows": spot.rect.h },
    });
    if let Some(a) = agent {
        v["agent"] = json!(a);
    }
    if !spot.label.is_empty() {
        v["label"] = json!(spot.label);
    }
    // The shape a session arrives in, which is what `herdr::parse_pane` digs
    // `session_id` out of — and **only while there is an agent to have one**.
    //
    // Recorded 2026-08-17 in a sandbox session, herdr 0.7.5: a Claude Code was
    // spawned into `w1:p1`, herdr reported `agent_session.value`, the process
    // was killed, and the very next `pane.list` gave `agent_status: "unknown"`
    // with no `agent` and no `agent_session` at all. herdr's own
    // `session.json` had dropped the key too, so it does not come back after a
    // restart either — the persistence is of a *live* agent's session and not
    // of the fact that a session once ran there.
    //
    // That is the whole reason `cmd_agent::sessions_learned` refuses to treat
    // an absent session as a correction: the moment wsp most needs the id is
    // the moment the backend stops offering it.
    if let (Some(a), false) = (agent, spot.session.is_empty()) {
        v["agent_session"] =
            json!({ "source": "wsp", "agent": a, "kind": "id", "value": spot.session });
    }
    if !spot.tokens.is_empty() {
        v["tokens"] = Value::Object(spot.tokens.iter().map(|(k, x)| (k.clone(), json!(x))).collect());
    }
    v
}

/// One agent, as `agent.get` and `agent.list` render it.
///
/// The difference from a pane is `interactive_ready`, which lives on herdr's
/// `AgentInfo` and on nothing else — a `pane.list` row has no such field in the
/// schema and never carries one. So the reading that can tell starting from
/// idle is available from *either* agent call and from neither pane call, which
/// is not quite what `place.rs` says; see the module docs.
///
/// **Both fields are present-or-absent rather than true-or-false**, recorded
/// across a live launch: `launch_pending: true` with no `interactive_ready`
/// while the agent comes up, then `interactive_ready: true` with no
/// `launch_pending` once it will take a prompt. Neither is ever sent false, and
/// that asymmetry is load-bearing, and reading it wrong is what
/// `place_herdr::state_of_agent` was repaired to stop doing.
/// The same agent, in the first reading after `agent.start` — before herdr can
/// say what is in the pane.
///
/// A transcript, and the shape is the whole point: the record *exists*, so a
/// caller waiting for one is satisfied by it, and it names nothing, so the same
/// caller reading its state is told the seat is empty. See [`Stage::unnamed`]
/// for the recording and `place_herdr::start` for what that cost.
fn launching_json(spot: &Spot) -> Value {
    let mut v = pane_json(spot);
    if let Some(o) = v.as_object_mut() {
        // Absent rather than empty, because that is what herdr sends and
        // because `herdr::parse_pane` reading a missing field as `""` is
        // precisely the step that turns this into an empty seat.
        o.remove("agent");
        o.remove("agent_session");
    }
    v["agent_status"] = json!("unknown");
    v["launch_pending"] = json!(true);
    if !spot.agent.name.is_empty() {
        v["name"] = json!(spot.agent.name);
    }
    v
}

fn agent_json(spot: &Spot) -> Value {
    let (agent, status, ready) = of_state(spot);
    let mut v = pane_json(spot);
    v["agent"] = json!(agent.unwrap_or(""));
    v["agent_status"] = json!(status);
    v["state_change_seq"] = json!(1);
    if !spot.agent.name.is_empty() {
        v["name"] = json!(spot.agent.name);
    }
    match ready {
        Some(r) => {
            v["interactive_ready"] = json!(r);
        }
        None if spot.state == State::Starting => {
            v["launch_pending"] = json!(true);
        }
        None => {}
    }
    v
}

fn space_json(stage: &Stage, space: &str) -> Value {
    let spots: Vec<&Spot> = stage.spots.iter().filter(|s| s.space == space).collect();
    let tabs: Vec<&String> = {
        let mut t: Vec<&String> = spots.iter().map(|s| &s.tab).collect();
        t.dedup();
        t
    };
    // A workspace's own label is whatever `workspace.rename` last said, and the
    // fake keeps it on the seat that is its root pane — herdr keeps it on the
    // workspace, and nothing in wsp reads one back except through
    // `workspace.list`, so this is the cheapest place it can live.
    let label = spots.first().map(|s| s.label.clone()).unwrap_or_default();
    let status = spots
        .iter()
        .find_map(|s| match of_state(s) {
            (Some(_), st, _) => Some(st),
            _ => None,
        })
        .unwrap_or("unknown");
    json!({
        "workspace_id": space,
        "number": space.trim_start_matches('w').parse::<u32>().unwrap_or(0),
        "label": label,
        "focused": false,
        "pane_count": spots.len(),
        "tab_count": tabs.len().max(1),
        "active_tab_id": spots.first().map(|s| s.tab.clone()).unwrap_or_default(),
        "agent_status": status,
    })
}

fn tab_json(stage: &Stage, tab: &str) -> Value {
    let spots: Vec<&Spot> = stage.spots.iter().filter(|s| s.tab == tab).collect();
    json!({
        "tab_id": tab,
        "workspace_id": spots.first().map(|s| s.space.clone()).unwrap_or_default(),
        "number": 1,
        "label": "",
        "focused": false,
        "pane_count": spots.len(),
        "agent_status": "unknown",
    })
}

fn rect_json(r: &Rect) -> Value {
    json!({ "x": r.x, "y": r.y, "width": r.w, "height": r.h })
}

/// The layout of the tab a seat is in.
///
/// `splits` is empty and that is a decision rather than an oversight: a real
/// reply carries the split tree, wsp reads `panes[].rect` and nothing else
/// (`panel/install.rs:103`, `detail/run.rs:433`, `detail/editors.rs:342`), and
/// inventing a tree would be the fake asserting something about herdr that
/// nobody has checked. Empty is visibly empty; a plausible tree is not.
fn layout_json(stage: &Stage, tab: &str) -> Value {
    let spots: Vec<&Spot> = stage.spots.iter().filter(|s| s.tab == tab).collect();
    json!({
        "workspace_id": spots.first().map(|s| s.space.clone()).unwrap_or_default(),
        "tab_id": tab,
        "zoomed": false,
        "area": rect_json(&stage.area),
        "focused_pane_id": stage.focused.as_ref().map(|s| s.to_string()).unwrap_or_default(),
        "panes": spots
            .iter()
            .map(|s| json!({
                "pane_id": s.seat.as_str(),
                "focused": stage.focused.as_ref() == Some(&s.seat),
                "rect": rect_json(&s.rect),
            }))
            .collect::<Vec<_>>(),
        "splits": [],
    })
}

/// A port event, in the words herdr's stream uses.
///
/// The stream names events with underscores where a subscription uses dots, and
/// the mapping is not one-to-one in either direction:
///
/// - an agent *starting* is `pane_agent_detected`, which herdr also raises when
///   the agent goes away (`released`), so [`Event::Stopped`] is the same event
///   with the agent taken out of it;
/// - a state *change* is `pane_updated`, the noisiest thing herdr sends and the
///   only global carrier of a status change — the panel's `CHATTY` list exists
///   because of it;
/// - `pane_exited` is the pane's process ending rather than the agent's, so
///   nothing here raises it. A test that wants the `pane.exited` cascade drives
///   it by closing the seat, which is what a person watching would see.
fn event_json(stage: &Stage, event: &Event) -> Vec<(String, Value)> {
    let seat = match event {
        Event::Opened(s) | Event::Started(s) | Event::Moved(s, _) | Event::Stopped(s) | Event::Closed(s) => s,
    };
    let spot = stage.find(seat);
    match event {
        Event::Opened(_) => match spot {
            Some(spot) => vec![(
                "pane_created".into(),
                json!({ "type": "pane_created", "pane": pane_json(spot) }),
            )],
            None => Vec::new(),
        },
        Event::Started(_) => match spot {
            Some(spot) => vec![
                (
                    "pane_agent_detected".into(),
                    json!({
                        "type": "pane_agent_detected",
                        "pane_id": spot.seat.as_str(),
                        "workspace_id": spot.space,
                        "agent": spot.agent.kind,
                    }),
                ),
                ("pane_updated".into(), json!({ "type": "pane_updated", "pane": pane_json(spot) })),
            ],
            None => Vec::new(),
        },
        Event::Moved(_, _) => match spot {
            Some(spot) => {
                vec![("pane_updated".into(), json!({ "type": "pane_updated", "pane": pane_json(spot) }))]
            }
            None => Vec::new(),
        },
        Event::Stopped(_) => match spot {
            Some(spot) => vec![
                (
                    "pane_agent_detected".into(),
                    json!({
                        "type": "pane_agent_detected",
                        "pane_id": spot.seat.as_str(),
                        "workspace_id": spot.space,
                        "agent": Value::Null,
                        "released": true,
                    }),
                ),
                ("pane_updated".into(), json!({ "type": "pane_updated", "pane": pane_json(spot) })),
            ],
            None => Vec::new(),
        },
        // The seat is gone, so there is no spot to describe it with — which is
        // why the event carries the two ids and nothing else, exactly as a
        // recorded `pane_closed` does.
        Event::Closed(seat) => vec![(
            "pane_closed".into(),
            json!({
                "type": "pane_closed",
                "pane_id": seat.as_str(),
                "workspace_id": seat.as_str().split_once(':').map(|(w, _)| w).unwrap_or(""),
            }),
        )],
    }
}

/// The subscription types herdr scopes to a single pane.
///
/// Recorded, and it is the fact that cost the panel its whole event feed once:
/// naming one of these without a `pane_id` refuses **the entire list** with
/// `invalid_request`, and the server then hangs up. The fake reproduces the
/// refusal rather than being helpful, because being helpful here would make
/// `daemon::EVENTS` look arbitrary to whoever reads it next.
const PER_PANE: &[&str] = &["pane.agent_status_changed", "pane.output_matched", "pane.scroll_changed"];

/// Every subscription type a real herdr accepts. Recorded from its own refusal
/// message, which lists them.
const EVENT_TYPES: &[&str] = &[
    "workspace.created", "workspace.updated", "workspace.metadata_updated", "workspace.renamed",
    "workspace.moved", "workspace.closed", "workspace.focused", "worktree.created",
    "worktree.opened", "worktree.removed", "tab.created", "tab.closed", "tab.focused",
    "tab.renamed", "tab.moved", "pane.created", "pane.closed", "pane.updated", "pane.focused",
    "pane.moved", "pane.exited", "pane.agent_detected", "pane.output_matched",
    "pane.agent_status_changed", "pane.scroll_changed", "layout.updated",
];

/// Whether a subscription list would be accepted, and why not.
///
/// Whole-list validation, which is the part that matters: one bad entry refuses
/// every other entry with it.
fn subscription_error(subs: &[Value]) -> Option<String> {
    for s in subs {
        let t = s.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if !EVENT_TYPES.contains(&t) {
            return Some(format!("invalid request: unknown variant `{t}`"));
        }
        if PER_PANE.contains(&t) && s.get("pane_id").and_then(|p| p.as_str()).unwrap_or("").is_empty() {
            return Some("invalid request: missing field `pane_id`".into());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

/// Everything the connection threads share.
struct Inner {
    stage: Mutex<Stage>,
    asked: Mutex<Vec<Asked>>,
    watchers: Mutex<Vec<Watcher>>,
    stop: AtomicBool,
}

struct Watcher {
    types: Vec<String>,
    tx: Sender<Value>,
}

/// A herdr that is not herdr, on a socket of its own.
///
/// Dropping it stops the server and removes the socket file, because a unix
/// socket outlives the process that bound it and the second fake on a path
/// would otherwise fail as `AddrInUse` — which is `stand_in`'s scar, kept.
pub struct Fake {
    path: PathBuf,
    inner: Arc<Inner>,
}

impl Fake {
    /// Bind a socket and start answering.
    pub fn bind(path: impl AsRef<Path>, stage: Stage) -> std::io::Result<Fake> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;

        let inner = Arc::new(Inner {
            stage: Mutex::new(stage),
            asked: Mutex::new(Vec::new()),
            watchers: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });

        let accept = Arc::clone(&inner);
        std::thread::spawn(move || {
            while !accept.stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // The listener is non-blocking so that `stop` is
                        // ever looked at. The connection must not be, and BSD
                        // hands it the listener's `O_NONBLOCK` anyway —
                        // measured here; POSIX says otherwise and Linux agrees
                        // with POSIX. Left set, `serve`'s first `read_line`
                        // answered `WouldBlock` whenever the request had not
                        // landed yet, was matched by the arm that means *the
                        // client went away*, and hung up on somebody
                        // mid-sentence. What the caller saw was an empty reply
                        // or a broken pipe from a backend that was up the whole
                        // time — including a real one, since `wsp sandbox
                        // --fake` serves through this bind.
                        //
                        // A busy suite hid it, as it hid `robustness-072` and
                        // for the same reason: contention handed the writer its
                        // turn before the reader got one. Alone on this machine
                        // `a_seat_the_fake_did_not_invent_is_still_a_seat`
                        // failed 14 times in 60, and 0 in 200 after
                        // (`robustness-074`).
                        let _ = stream.set_nonblocking(false);
                        let inner = Arc::clone(&accept);
                        std::thread::spawn(move || serve(inner, stream));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Fake { path, inner })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Point this process at the fake, for a test that drives wsp's own code.
    ///
    /// Says out loud what it is doing to a process-wide variable, because the
    /// caller has to hold whatever lock the rest of the suite uses for it.
    pub fn socket_env(&self) -> (&'static str, String) {
        ("HERDR_SOCKET_PATH", self.path.display().to_string())
    }

    fn with_stage<T>(&self, f: impl FnOnce(&mut Stage) -> T) -> T {
        let mut stage = self.inner.stage.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut stage)
    }

    /// The world as it stands.
    pub fn stage(&self) -> Stage {
        self.with_stage(|s| s.clone())
    }

    /// Every seat, in the port's words.
    pub fn seats(&self) -> Vec<Seated> {
        self.with_stage(|s| s.census())
    }

    /// What the fake has been asked to do, in the port's words.
    pub fn asked(&self) -> Vec<Asked> {
        self.inner.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The verbs only, for a test whose whole assertion is the sequence.
    pub fn verbs(&self) -> Vec<Verb> {
        self.asked().iter().map(|a| a.verb).collect()
    }

    pub fn forget(&self) {
        self.inner.asked.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Put the world in this state, and tell every watcher what moved.
    ///
    /// The permutation driver. Everything else that changes the state goes
    /// through here, including the fake's own answers to wsp's writes, so there
    /// is exactly one place events are raised from.
    pub fn restage(&self, next: Stage) {
        let (events, after) = {
            let mut stage = self.inner.stage.lock().unwrap_or_else(|e| e.into_inner());
            let events = Stage::diff(&stage, &next);
            *stage = next;
            (events, stage.clone())
        };
        self.raise(&after, &events);
    }

    /// A seat appears — a person opened a terminal, or something else did.
    pub fn opens(&self, spot: Spot) -> Seat {
        let mut next = self.stage();
        let seat = next.put(spot);
        self.restage(next);
        seat
    }

    /// A seat changes state.
    pub fn moves(&self, seat: &Seat, state: State) {
        let mut next = self.stage();
        if let Some(spot) = next.find_mut(seat) {
            spot.state = state;
        }
        self.restage(next);
    }

    /// The agent in a seat has stopped. The seat stays.
    pub fn stops(&self, seat: &Seat) {
        let mut next = self.stage();
        if let Some(spot) = next.find_mut(seat) {
            spot.state = State::Gone;
            spot.agent = Agent::default();
        }
        self.restage(next);
    }

    /// The seat itself is gone.
    pub fn closes(&self, seat: &Seat) {
        let mut next = self.stage();
        next.spots.retain(|s| &s.seat != seat);
        self.restage(next);
    }

    /// Stop answering, in one of the two ways a backend stops answering.
    pub fn goes(&self, quiet: Quiet) {
        self.with_stage(|s| s.quiet = quiet);
    }

    /// Refuse a verb until told otherwise.
    pub fn refuses(&self, verb: Verb, snub: Snub) {
        self.with_stage(|s| {
            s.refuse.insert(verb, snub);
        });
    }
    pub fn relents(&self, verb: Verb) {
        self.with_stage(|s| {
            s.refuse.remove(&verb);
        });
    }

    /// Wait until somebody is listening to the stream, and say whether anybody
    /// turned up.
    ///
    /// **The only honest form of the sentence a subscriber test has to say
    /// first.** A watcher that subscribes after the change learns nothing at
    /// all, so a test that changes the state has to know its own subscriber is
    /// registered — and what it used to do was `sleep(150ms)` and hope, which is
    /// right on a quiet machine and a flake on a busy one (robustness-054, the
    /// same fault as the shell race and one file over).
    ///
    /// So it asks the fake's own register, which is exactly the thing
    /// [`Fake::raise`] delivers to: the answer is a fact rather than an
    /// allowance. The bound is a hang-guard and not a measurement — it is there
    /// so a broken subscribe fails the test instead of wedging the suite, and a
    /// machine slow enough to reach it has already failed the assertion that
    /// follows.
    pub fn watched(&self) -> bool {
        for _ in 0..2_000 {
            if !self.inner.watchers.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }

    /// Push what happened to whoever is listening.
    fn raise(&self, stage: &Stage, events: &[Event]) {
        let mut lines: Vec<(String, Value)> = Vec::new();
        for e in events {
            lines.extend(event_json(stage, e));
        }
        if lines.is_empty() {
            return;
        }
        let mut watchers = self.inner.watchers.lock().unwrap_or_else(|e| e.into_inner());
        watchers.retain(|w| {
            lines.iter().all(|(name, data)| {
                if !w.types.iter().any(|t| t.replace('.', "_") == *name) {
                    return true;
                }
                w.tx.send(json!({ "event": name, "data": data })).is_ok()
            })
        });
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One connection: newline-delimited JSON in, one reply per request, until the
/// client goes away.
fn serve(inner: Arc<Inner>, stream: UnixStream) {
    let Ok(write) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    let mut out = write;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(line.trim()) else { continue };
        let id = req.get("id").cloned().unwrap_or(Value::String(String::new()));
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));

        // Silence, before anything else — including before the recording. A
        // backend that is not answering does not know it was asked.
        match quiet_of(&inner) {
            Quiet::HangsUp => return,
            Quiet::Never => {
                // Hold the connection open and say nothing. The caller finds
                // out on its own read timeout, which is the point.
                while !inner.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                return;
            }
            Quiet::No => {}
        }

        if method == "events.subscribe" {
            record(&inner, Asked { verb: Verb::Watch, seat: None, said: String::new() });
            watch(&inner, &mut out, &id, &params);
            return;
        }

        let reply = match answer(&inner, &method, &params) {
            Ok(result) => json!({ "id": id, "result": result }),
            Err((code, message)) => json!({ "id": id, "error": { "code": code, "message": message } }),
        };
        if out.write_all(format!("{reply}\n").as_bytes()).is_err() || out.flush().is_err() {
            return;
        }
    }
}

fn quiet_of(inner: &Arc<Inner>) -> Quiet {
    inner.stage.lock().unwrap_or_else(|e| e.into_inner()).quiet
}

fn record(inner: &Arc<Inner>, asked: Asked) {
    inner.asked.lock().unwrap_or_else(|e| e.into_inner()).push(asked);
}

/// Hold the connection open and push events down it.
///
/// The one genuine addition beyond `stand_in`, and what the daemon's whole
/// event path has never been tested against.
fn watch(inner: &Arc<Inner>, out: &mut UnixStream, id: &Value, params: &Value) {
    let subs: Vec<Value> =
        params.get("subscriptions").and_then(|s| s.as_array()).cloned().unwrap_or_default();

    if let Some(message) = subscription_error(&subs) {
        // One error reply and then the server hangs up — recorded, and the
        // behaviour that made a refused subscription indistinguishable from a
        // clean close until `herdr::subscribe_on` learned to read it.
        let reply = json!({ "id": "", "error": { "code": "invalid_request", "message": message } });
        let _ = out.write_all(format!("{reply}\n").as_bytes());
        let _ = out.flush();
        return;
    }

    let (tx, rx): (Sender<Value>, Receiver<Value>) = mpsc::channel();
    {
        let types = subs
            .iter()
            .filter_map(|s| s.get("type").and_then(|t| t.as_str()).map(String::from))
            .collect();
        inner.watchers.lock().unwrap_or_else(|e| e.into_inner()).push(Watcher { types, tx });
    }

    let started = json!({ "id": id, "result": { "type": "subscription_started" } });
    if out.write_all(format!("{started}\n").as_bytes()).is_err() || out.flush().is_err() {
        return;
    }

    while !inner.stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if out.write_all(format!("{line}\n").as_bytes()).is_err() || out.flush().is_err() {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The error codes a real herdr answers with, recorded rather than invented.
///
/// Anything the fake refuses for a reason herdr has never had is
/// [`OUR_OWN`], and says so in the message — because a fake that invents a
/// herdr error code teaches a caller to match on a string no server will ever
/// send.
const NO_PANE: &str = "pane_not_found";
const NO_AGENT: &str = "agent_not_found";
const NOT_READY: &str = "agent_not_ready";
const PANE_BUSY: &str = "agent_pane_busy";
const NOT_TAKEN: &str = "agent_prompt_stalled";
const OUR_OWN: &str = "fake_cannot";

type Answer = Result<Value, (String, String)>;

fn no_pane(id: &str) -> (String, String) {
    (NO_PANE.into(), format!("pane {id} not found"))
}

/// Answer one request out of the state.
///
/// Every arm ends in the state, and no arm holds a canned reply: that is the
/// whole design. The verb table in [`Verb::of_method`] is what decides whether
/// a method is answered at all — an unknown one is refused rather than given
/// something plausible, because a fake that quietly answers a method wsp has
/// never called is a fake that will go on answering it after herdr stops.
fn answer(inner: &Arc<Inner>, method: &str, params: &Value) -> Answer {
    let sget = |key: &str| params.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();

    let Some(verb) = Verb::of_method(method) else {
        return Err((
            OUR_OWN.into(),
            format!("the fake answers what wsp calls, and nothing calls `{method}`"),
        ));
    };

    let mut stage = inner.stage.lock().unwrap_or_else(|e| e.into_inner());

    // A scripted refusal comes before the state, so that a test can refuse a
    // verb on a seat that exists — which is the only interesting case.
    if let Some(snub) = stage.refuse.get(&verb).cloned() {
        let seat = pick_seat(params);
        record(inner, Asked { verb, seat, said: String::new() });
        return Err(match snub {
            Snub::NotReady => (NOT_READY.into(), format!("agent {} is not ready", sget("target"))),
            Snub::Busy => (PANE_BUSY.into(), "not an available shell".into()),
            Snub::Backend(msg) => (OUR_OWN.into(), msg),
        });
    }

    let mut events: Vec<Event> = Vec::new();
    let before = stage.clone();

    let result = match method {
        // ---- the place-work port ------------------------------------------
        "workspace.create" => {
            let label = sget("label");
            let mut spot = Spot::default();
            spot.label = label.clone();
            spot.cwd = sget("cwd");
            spot.state = State::Empty;
            let seat = stage.put(spot);
            record(inner, Asked { verb, seat: Some(seat.clone()), said: label });
            let spot = stage.find(&seat).cloned().unwrap_or_default();
            events.push(Event::Opened(seat.clone()));
            if params.get("focus").and_then(|f| f.as_bool()).unwrap_or(false) {
                stage.focused = Some(seat);
            }
            json!({
                "type": "workspace_created",
                "workspace": space_json(&stage, &spot.space),
                "tab": tab_json(&stage, &spot.tab),
                "root_pane": pane_json(&spot),
            })
        }

        "agent.start" => {
            let seat = Seat::new(sget("pane_id"));
            let kind = sget("kind");
            record(inner, Asked { verb, seat: Some(seat.clone()), said: kind.clone() });
            let settle = stage.settle;
            let Some(before) = stage.find(&seat).cloned() else {
                return Err(no_pane(seat.as_str()));
            };
            // The detection lag starts here and is counted down by `agent.get`.
            stage.launching = stage.unnamed;
            let Some(spot) = stage.find_mut(&seat) else {
                return Err(no_pane(seat.as_str()));
            };
            spot.agent = Agent {
                kind,
                name: sget("name"),
                args: params
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
            };
            // Starting, then idle if this stage settles. Two events, in that
            // order, because that is what a caller polling `agent.get` sees and
            // the gap between them is where `agent_not_ready` lives.
            spot.state = State::Starting;
            events.push(Event::Started(seat.clone()));
            if settle {
                if let Some(spot) = stage.find_mut(&seat) {
                    spot.state = State::Idle;
                }
                events.push(Event::Moved(seat.clone(), State::Idle));
            }
            // The reply describes the seat as it was a moment *before*, which
            // is not tidiness: recorded, `agent.start` comes back with
            // `launch_pending: true`, `agent_status: "unknown"` and **no agent
            // named** — the name appears about three tenths of a second later.
            // So `Place::start`'s "returns when the agent exists" is not what
            // this backend does, and an adapter owes that wait to its caller.
            // Nothing in wsp reads this reply (`place_herdr::launch` matches on
            // `Ok(_)`); the adapter waits for the agent instead.
            let mut agent = agent_json(&before);
            agent["launch_pending"] = json!(true);
            agent["name"] = json!(sget("name"));
            let argv = stage.find(&seat).map(|s| s.agent.clone()).unwrap_or_default();
            json!({
                "type": "agent_started",
                "agent": agent,
                "argv": std::iter::once(argv.kind).chain(argv.args).collect::<Vec<_>>(),
            })
        }

        "agent.prompt" => {
            let seat = Seat::new(sget("target"));
            let text = sget("text");
            record(inner, Asked { verb, seat: Some(seat.clone()), said: text });
            let Some(spot) = stage.find(&seat) else {
                return Err((NO_AGENT.into(), format!("agent target {seat} not found")));
            };
            if spot.agent.kind.is_empty() {
                return Err((NO_AGENT.into(), format!("agent target {seat} not found")));
            }
            // The one guard that is the port's rather than herdr's wording:
            // `will_take_a_prompt` is `place::State`'s single answer to the
            // question every `state == "idle"` caller is actually asking.
            //
            // `Working` is let through, and that is herdr's behaviour rather
            // than a convenience. A real `agent.prompt` has no idle guard at all
            // — it refuses a launch window and an unknown agent and nothing else
            // — so a sentence addressed to an agent in the middle of a turn is
            // accepted and queued behind it. That is not a corner: it is the
            // whole of `wsp govern --tell`, which the fake could not reach while
            // it modelled a guard herdr does not have.
            let busy = spot.state == State::Working;
            if !busy && !spot.state.will_take_a_prompt() {
                return Err((
                    NOT_READY.into(),
                    format!("agent {seat} is {} and will not take a prompt", spot.state.as_str()),
                ));
            }
            // A prompt that is taken starts a turn, and a stage that says
            // otherwise is robustness-035 on a socket: the call is accepted, the
            // sentence sits in the composer and the agent stays idle. The fake
            // could not express that failure before, which is why nothing here
            // ever noticed `spawn` reporting a handover it had not confirmed.
            let takes = stage.takes;
            if takes && !busy {
                if let Some(spot) = stage.find_mut(&seat) {
                    spot.state = State::Working;
                }
                events.push(Event::Moved(seat.clone(), State::Working));
            }
            // `wait` is a caller asking herdr to hold the reply until the status
            // moves, and modelling it is modelling its two failures rather than
            // its duration — the fake answers in outcomes, and a test that slept
            // for eight seconds to prove a point would be measuring the machine.
            //
            // Delivered and unmoved is `agent_prompt_stalled`, which is
            // robustness-035 arriving as an answer instead of as a silence.
            // Delivered to an agent that was *already* working is the trap: the
            // status cannot change because it is already what it would change
            // to, so herdr waits out the whole timeout and calls it a timeout —
            // a plain failure reported for a sentence that arrived perfectly.
            // A caller that sends the field to a busy agent gets this, and the
            // fake's job is to make that a test failure rather than a night.
            if params.get("wait").is_some() {
                if busy {
                    return Err((
                        "timeout".into(),
                        format!("timed out waiting for agent status on {seat}"),
                    ));
                }
                if !takes {
                    return Err((
                        NOT_TAKEN.into(),
                        format!("agent prompt to {seat} produced no observed state change"),
                    ));
                }
            }
            let spot = stage.find(&seat).expect("the seat answered a moment ago");
            json!({ "type": "agent_prompted", "agent": agent_json(spot) })
        }

        // The recovery for the above, and the reason [`Place::nudge`] exists:
        // press submit on a work order that arrived and was not taken. Whatever
        // keys are asked for, what the fake models is the one that was ever
        // sent — a return into an agent waiting on one starts the turn the
        // prompt should have.
        "agent.send_keys" => {
            let seat = Seat::new(sget("target"));
            let keys: Vec<String> = params
                .get("keys")
                .and_then(|k| k.as_array())
                .map(|k| k.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            record(inner, Asked { verb, seat: Some(seat.clone()), said: keys.join(" ") });
            let Some(spot) = stage.find(&seat) else {
                return Err((NO_AGENT.into(), format!("agent target {seat} not found")));
            };
            if spot.agent.kind.is_empty() {
                return Err((NO_AGENT.into(), format!("agent target {seat} not found")));
            }
            if spot.state == State::Idle {
                if let Some(spot) = stage.find_mut(&seat) {
                    spot.state = State::Working;
                }
                events.push(Event::Moved(seat.clone(), State::Working));
            }
            json!({ "type": "ok" })
        }

        "agent.get" => {
            let seat = Seat::new(sget("target"));
            record(inner, Asked { verb, seat: Some(seat.clone()), said: String::new() });
            let launching = stage.launching > 0;
            stage.launching = stage.launching.saturating_sub(1);
            match stage.find(&seat) {
                // Recorded: a pane with no agent answers `agent_not_found`,
                // exactly as a pane that does not exist does. A caller cannot
                // tell an empty seat from a missing one this way, which is why
                // `place::Place::state` is a different question from a census.
                Some(spot) if !spot.agent.kind.is_empty() && launching => {
                    json!({ "type": "agent_info", "agent": launching_json(spot) })
                }
                Some(spot) if !spot.agent.kind.is_empty() => {
                    json!({ "type": "agent_info", "agent": agent_json(spot) })
                }
                _ => return Err((NO_AGENT.into(), format!("agent target {seat} not found"))),
            }
        }

        // ---- reading ------------------------------------------------------
        "pane.list" => {
            record(inner, Asked { verb, seat: None, said: String::new() });
            json!({ "type": "pane_list", "panes": stage.spots.iter().map(pane_json).collect::<Vec<_>>() })
        }
        "pane.get" => {
            let seat = Seat::new(sget("pane_id"));
            record(inner, Asked { verb, seat: Some(seat.clone()), said: String::new() });
            match stage.find(&seat) {
                Some(spot) => json!({ "type": "pane_info", "pane": pane_json(spot) }),
                None => return Err(no_pane(seat.as_str())),
            }
        }
        "agent.list" => {
            record(inner, Asked { verb, seat: None, said: String::new() });
            let agents: Vec<Value> = stage
                .spots
                .iter()
                .filter(|s| !s.agent.kind.is_empty() && s.state != State::Gone)
                .map(agent_json)
                .collect();
            json!({ "type": "agent_list", "agents": agents })
        }
        "workspace.list" => {
            record(inner, Asked { verb, seat: None, said: String::new() });
            let mut spaces: Vec<String> = stage.spots.iter().map(|s| s.space.clone()).collect();
            spaces.dedup();
            let list: Vec<Value> = spaces.iter().map(|w| space_json(&stage, w)).collect();
            json!({ "type": "workspace_list", "workspaces": list })
        }
        "pane.layout" => {
            let seat = Seat::new(sget("pane_id"));
            record(inner, Asked { verb, seat: Some(seat.clone()), said: String::new() });
            let Some(tab) = stage.find(&seat).map(|s| s.tab.clone()) else {
                return Err(no_pane(seat.as_str()));
            };
            json!({ "type": "pane_layout", "layout": layout_json(&stage, &tab) })
        }
        "pane.read" => {
            let seat = Seat::new(sget("pane_id"));
            record(inner, Asked { verb, seat: Some(seat.clone()), said: String::new() });
            // Deliberately refused, with the fake's own code. A fake with no
            // terminal has no screen to read, and answering with text would be
            // inventing what somebody's agent said.
            return Err((
                OUR_OWN.into(),
                "read a screen: there is no terminal here, only a state".into(),
            ));
        }

        // ---- the arrange-panes port ---------------------------------------
        "pane.split" => {
            let off = Seat::new(sget("target_pane_id"));
            record(inner, Asked { verb, seat: Some(off.clone()), said: sget("direction") });
            let Some(parent) = stage.find(&off).cloned() else {
                return Err(no_pane(off.as_str()));
            };
            let ratio = params.get("ratio").and_then(|r| r.as_f64()).unwrap_or(0.5);
            let down = sget("direction") == "down";
            let (kept, made) = split_rect(parent.rect, ratio, down);
            if let Some(spot) = stage.find_mut(&off) {
                spot.rect = kept;
            }
            let n = stage.spots.iter().filter(|s| s.space == parent.space).count() + 1;
            let mut spot = Spot::empty(&format!("{}:p{n}", parent.space));
            spot.space = parent.space.clone();
            spot.tab = parent.tab.clone();
            spot.cwd = match sget("cwd") {
                c if c.is_empty() => parent.cwd.clone(),
                c => c,
            };
            spot.rect = made;
            let seat = stage.put(spot);
            events.push(Event::Opened(seat.clone()));
            let spot = stage.find(&seat).cloned().unwrap_or_default();
            json!({ "type": "pane_info", "pane": pane_json(&spot) })
        }
        "tab.create" => {
            let space = sget("workspace_id");
            record(inner, Asked { verb, seat: None, said: sget("label") });
            let tabs = stage.spots.iter().filter(|s| s.space == space).count() + 1;
            let mut spot = Spot::empty("");
            spot.space = space.clone();
            spot.tab = format!("{space}:t{tabs}");
            spot.label = sget("label");
            spot.cwd = sget("cwd");
            let seat = stage.put(spot);
            events.push(Event::Opened(seat.clone()));
            let spot = stage.find(&seat).cloned().unwrap_or_default();
            json!({
                "type": "tab_created",
                "tab": tab_json(&stage, &spot.tab),
                "root_pane": pane_json(&spot),
            })
        }
        "pane.rename" => {
            let seat = Seat::new(sget("pane_id"));
            let label = sget("label");
            record(inner, Asked { verb, seat: Some(seat.clone()), said: label.clone() });
            let Some(spot) = stage.find_mut(&seat) else {
                return Err(no_pane(seat.as_str()));
            };
            spot.label = label;
            let spot = spot.clone();
            json!({ "type": "pane_info", "pane": pane_json(&spot) })
        }
        "workspace.rename" => {
            let space = sget("workspace_id");
            let label = sget("label");
            record(inner, Asked { verb, seat: None, said: label.clone() });
            let Some(spot) = stage.spots.iter_mut().find(|s| s.space == space) else {
                return Err((NO_PANE.into(), format!("workspace {space} not found")));
            };
            spot.label = label;
            json!({ "type": "workspace_info", "workspace": space_json(&stage, &space) })
        }
        "pane.send_text" => {
            let seat = Seat::new(sget("pane_id"));
            record(inner, Asked { verb, seat: Some(seat.clone()), said: sget("text") });
            if stage.find(&seat).is_none() {
                return Err(no_pane(seat.as_str()));
            }
            json!({ "type": "ok" })
        }
        "pane.swap" => {
            let a = Seat::new(sget("source_pane_id"));
            let b = Seat::new(sget("target_pane_id"));
            record(inner, Asked { verb, seat: Some(a.clone()), said: b.to_string() });
            let (Some(ra), Some(rb)) = (stage.find(&a).map(|s| s.rect), stage.find(&b).map(|s| s.rect))
            else {
                return Err(no_pane(if stage.find(&a).is_none() { a.as_str() } else { b.as_str() }));
            };
            if let Some(s) = stage.find_mut(&a) {
                s.rect = rb;
            }
            if let Some(s) = stage.find_mut(&b) {
                s.rect = ra;
            }
            let tab = stage.find(&a).map(|s| s.tab.clone()).unwrap_or_default();
            json!({
                "type": "pane_swap",
                "swap": {
                    "changed": true,
                    "source_pane_id": a.as_str(),
                    "target_pane_id": b.as_str(),
                    "focused_pane_id": stage.focused.as_ref().map(|s| s.to_string()).unwrap_or_default(),
                    "layout": layout_json(&stage, &tab),
                }
            })
        }
        "pane.focus" | "tab.focus" | "workspace.focus" => {
            let seat = pick_seat(params);
            record(inner, Asked { verb, seat: seat.clone(), said: String::new() });
            match method {
                "pane.focus" => {
                    let seat = Seat::new(sget("pane_id"));
                    let Some(spot) = stage.find(&seat).cloned() else {
                        return Err(no_pane(seat.as_str()));
                    };
                    stage.focused = Some(seat);
                    json!({ "type": "pane_info", "pane": pane_json(&spot) })
                }
                "tab.focus" => {
                    let tab = sget("tab_id");
                    json!({ "type": "tab_info", "tab": tab_json(&stage, &tab) })
                }
                _ => {
                    let space = sget("workspace_id");
                    json!({ "type": "workspace_info", "workspace": space_json(&stage, &space) })
                }
            }
        }
        "pane.close" | "tab.close" | "workspace.close" => {
            record(inner, Asked { verb, seat: pick_seat(params), said: String::new() });
            let gone: Vec<Seat> = match method {
                "pane.close" => {
                    let seat = Seat::new(sget("pane_id"));
                    if stage.find(&seat).is_none() {
                        return Err(no_pane(seat.as_str()));
                    }
                    vec![seat]
                }
                "tab.close" => {
                    let tab = sget("tab_id");
                    stage.spots.iter().filter(|s| s.tab == tab).map(|s| s.seat.clone()).collect()
                }
                _ => {
                    let space = sget("workspace_id");
                    stage.spots.iter().filter(|s| s.space == space).map(|s| s.seat.clone()).collect()
                }
            };
            stage.spots.retain(|s| !gone.contains(&s.seat));
            events.extend(gone.into_iter().map(Event::Closed));
            json!({ "type": "ok" })
        }

        // ---- what the port deletes ----------------------------------------
        "pane.report_metadata" | "workspace.report_metadata" => {
            record(inner, Asked { verb, seat: pick_seat(params), said: String::new() });
            let tokens: BTreeMap<String, String> = params
                .get("tokens")
                .and_then(|t| t.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let seat = Seat::new(sget("pane_id"));
            let space = sget("workspace_id");
            // Hung on the seat it names, or on the first seat of the workspace
            // it names — because a workspace is not a thing the fake's state
            // has, and the tokens are only ever read back to prove they were
            // pushed.
            let on = match stage.find(&seat) {
                Some(_) => Some(seat),
                None => stage.spots.iter().find(|s| s.space == space).map(|s| s.seat.clone()),
            };
            if let Some(spot) = on.and_then(|seat| stage.find_mut(&seat)) {
                spot.tokens = tokens;
            }
            json!({ "type": "ok" })
        }

        other => {
            return Err((
                OUR_OWN.into(),
                format!("the fake answers what wsp calls, and nothing calls `{other}`"),
            ))
        }
    };

    // Events are raised after the reply is computed and while the lock is still
    // held, so a watcher can never see a world the caller has not been given.
    if !events.is_empty() {
        let after = stage.clone();
        let events = match events.len() {
            // A diff of the whole stage would raise the same things twice for
            // the write verbs above, which name what they changed. It is here
            // only as the guard for anything that forgot to.
            0 => Stage::diff(&before, &after),
            _ => events,
        };
        let mut lines: Vec<(String, Value)> = Vec::new();
        for e in &events {
            lines.extend(event_json(&after, e));
        }
        drop(stage);
        let mut watchers = inner.watchers.lock().unwrap_or_else(|e| e.into_inner());
        watchers.retain(|w| {
            lines.iter().all(|(name, data)| {
                if !w.types.iter().any(|t| t.replace('.', "_") == *name) {
                    return true;
                }
                w.tx.send(json!({ "event": name, "data": data })).is_ok()
            })
        });
    }

    Ok(result)
}

/// The seat a call is about, from whichever id it is addressed by.
///
/// The same keys `herdr::route` reads, and for a related reason: the id is what
/// a call is *about*, so a recording keyed on anything else would lose which
/// seat a test was watching.
fn pick_seat(params: &Value) -> Option<Seat> {
    for key in ["pane_id", "target", "source_pane_id", "target_pane_id", "workspace_id", "tab_id"] {
        if let Some(id) = params.get(key).and_then(|v| v.as_str()) {
            if !id.is_empty() {
                return Some(Seat::new(id));
            }
        }
    }
    None
}

/// How a split divides a pane: **the target keeps `ratio`, and the new pane
/// gets the remainder.**
///
/// Recorded, because it has bitten twice in this tree and once in the writing
/// of this file: a 54-column pane split at 0.22 keeps 12 columns and gives 42
/// away, so a sidebar asked for at 22% arrives *wide* unless it is swapped
/// afterwards. `panel/install.rs:155` learned it by putting the panel on the
/// wrong side at the wrong width.
fn split_rect(r: Rect, ratio: f64, down: bool) -> (Rect, Rect) {
    let ratio = ratio.clamp(0.0, 1.0);
    if down {
        let keep = (r.h as f64 * ratio).round() as u32;
        (Rect { h: keep, ..r }, Rect { y: r.y + keep, h: r.h.saturating_sub(keep), ..r })
    } else {
        let keep = (r.w as f64 * ratio).round() as u32;
        (Rect { w: keep, ..r }, Rect { x: r.x + keep, w: r.w.saturating_sub(keep), ..r })
    }
}

// ---------------------------------------------------------------------------
// A stage on disk
// ---------------------------------------------------------------------------

/// Read a stage from JSON, in the ports' vocabulary.
///
/// The file is what makes `wsp sandbox --fake` more than a decoration: a person
/// with a text editor can put the machine into a state and watch the panel
/// react to it. Everything is optional and the defaults are the boring answers,
/// because a stage nobody can write in one line is a stage nobody writes.
///
/// ```json
/// { "settle": false,
///   "seats": [ { "state": "working", "agent": "claude", "name": "t-260816-080",
///                "label": "robustness/080", "cwd": "/Users/e/claude/wsp" } ] }
/// ```
pub fn stage_from_json(v: &Value) -> Stage {
    let mut stage = Stage::default();
    if let Some(b) = v.get("settle").and_then(|b| b.as_bool()) {
        stage.settle = b;
    }
    if let Some(b) = v.get("takes").and_then(|b| b.as_bool()) {
        stage.takes = b;
    }
    stage.quiet = match v.get("quiet").and_then(|q| q.as_str()).unwrap_or("") {
        "hangs-up" | "hangs_up" => Quiet::HangsUp,
        "never" => Quiet::Never,
        _ => Quiet::No,
    };
    if let Some(a) = v.get("area") {
        stage.area = Rect {
            x: num(a, "x", 0),
            y: num(a, "y", 0),
            w: num(a, "w", 120),
            h: num(a, "h", 40),
        };
    }
    for s in v.get("seats").and_then(|s| s.as_array()).cloned().unwrap_or_default() {
        let text = |k: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let kind = text("agent");
        let state = match s.get("state").and_then(|v| v.as_str()).unwrap_or("") {
            "empty" => State::Empty,
            "starting" => State::Starting,
            "idle" => State::Idle,
            "working" => State::Working,
            "blocked" => State::Blocked,
            "gone" => State::Gone,
            "unknown" => State::Unknown,
            // A seat that named an agent and no state is one that is sitting
            // there waiting, which is what anybody writing a fixture means.
            _ if !kind.is_empty() => State::Idle,
            _ => State::Empty,
        };
        let mut spot = Spot {
            seat: Seat::new(text("seat")),
            label: text("label"),
            cwd: text("cwd"),
            agent: Agent { kind, name: text("name"), args: Vec::new() },
            state,
            session: text("session"),
            space: text("space"),
            tab: text("tab"),
            ..Spot::default()
        };
        if let Some(r) = s.get("rect") {
            spot.rect = Rect { x: num(r, "x", 0), y: num(r, "y", 0), w: num(r, "w", 0), h: num(r, "h", 0) };
        }
        stage.put(spot);
    }
    stage
}

fn num(v: &Value, key: &str, or: u32) -> u32 {
    v.get(key).and_then(|x| x.as_u64()).map(|n| n as u32).unwrap_or(or)
}

/// The same stage, written back out — so `--json` can say what the fake is
/// holding, in the same words the file is written in.
pub fn stage_to_json(stage: &Stage) -> Value {
    json!({
        "settle": stage.settle,
        "takes": stage.takes,
        "quiet": match stage.quiet {
            Quiet::No => "no",
            Quiet::HangsUp => "hangs-up",
            Quiet::Never => "never",
        },
        "seats": stage.spots.iter().map(|s| json!({
            "seat": s.seat.as_str(),
            "label": s.label,
            "cwd": s.cwd,
            "agent": s.agent.kind,
            "name": s.agent.name,
            "state": s.state.as_str(),
            "session": s.session,
            "space": s.space,
            "tab": s.tab,
            "rect": { "x": s.rect.x, "y": s.rect.y, "w": s.rect.w, "h": s.rect.h },
        })).collect::<Vec<_>>(),
    })
}

/// Serve a fake until something kills the process, re-reading the stage file
/// whenever it changes.
///
/// What `wsp sandbox --fake` runs. The file is the only way in from outside, and
/// a change to it goes through [`Fake::restage`] like everything else — so
/// editing a seat's state pushes exactly the events that state change would
/// have raised, which is the whole point of it being a diff rather than a
/// reload.
pub fn serve_forever(socket: &Path, stage_file: Option<&Path>) -> std::io::Result<()> {
    let first = stage_file.and_then(read_stage).unwrap_or_default();
    let fake = Fake::bind(socket, first)?;
    let mut stamp = stage_file.and_then(mtime);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let Some(path) = stage_file else { continue };
        let now = mtime(path);
        if now != stamp {
            stamp = now;
            if let Some(next) = read_stage(path) {
                fake.restage(next);
            }
        }
    }
}

fn read_stage(path: &Path) -> Option<Stage> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(stage_from_json(&v))
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr;
    use crate::util;


    /// How long to wait for an event already pushed down a socket on this
    /// machine. A hang-guard rather than a measurement, and generous on purpose:
    /// it is only ever paid by a test that is already failing.
    const DELIVERY: Duration = Duration::from_secs(10);

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-fake-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A caller that takes its time asking is still a caller.
    ///
    /// The pause is the whole test, and it is why nothing here caught the
    /// non-blocking accept for as long as it stood: every other case connects
    /// and writes in the same breath, and this one puts the fake inside its
    /// first read before there is anything to read. The argument is at the
    /// accept.
    #[test]
    fn a_caller_that_takes_its_time_asking_is_still_answered() {
        let dir = scratch("slow");
        let fake = Fake::bind(dir.join("herdr.sock"), Stage::new()).unwrap();
        let mut s = UnixStream::connect(fake.path()).unwrap();
        s.set_read_timeout(Some(DELIVERY)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        s.write_all(b"{\"id\":\"t1\",\"method\":\"pane.list\",\"params\":{}}\n")
            .expect("the fake hung up before the request arrived");
        s.flush().unwrap();

        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("a reply, not a hang-up: {e} — {line:?}"));
        assert_eq!(reply["id"], "t1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn call(fake: &Fake, method: &str, params: Value) -> Result<Value, String> {
        let mut s = UnixStream::connect(fake.path()).map_err(|e| e.to_string())?;
        // A hang-guard rather than a measurement, on the same argument as
        // [`DELIVERY`]: nothing here is asserting that the fake is fast, and a
        // tight bound on a loaded machine is a flake for nothing.
        s.set_read_timeout(Some(DELIVERY)).unwrap();
        let req = json!({ "id": "t1", "method": method, "params": params });
        s.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
        s.flush().unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
        match v.get("error") {
            Some(e) => Err(e.to_string()),
            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    /// The dialect and the readings wsp makes of herdr are one mapping seen
    /// from two sides, so a wrong belief about herdr can only be wrong in one
    /// place. This is that assertion — and it is where the fake earned its
    /// keep, because it failed the first time it was run.
    ///
    /// **Two states do not survive the trip, and only one of them is herdr's
    /// fault.**
    ///
    /// `Gone` is the known one. herdr cannot tell an exited agent's pane from a
    /// shell somebody opened, so it comes back as `Empty`; `place.rs` says the
    /// adapter answers `Empty` from a census and `Gone` from the event stream,
    /// and this is what forces that to stay true.
    ///
    /// `Starting` is the finding, and it is worth reading twice. The port
    /// decided an agent was starting from `ready == Some(false)` — and **herdr
    /// never sends `false`**. Recorded against a live Claude Code on 2026-08-17:
    /// for 3.3 seconds after `agent.start`, `agent.get` answers
    /// `agent_status: "idle"` with `launch_pending: true` and *no*
    /// `interactive_ready` field at all, and then the fields swap —
    /// `interactive_ready: true`, no `launch_pending`. Absence is the signal, in
    /// both directions.
    ///
    /// So the port's own translation mapped the launch window to
    /// [`State::Idle`], whose `will_take_a_prompt` says yes, which is exactly
    /// the bug `place.rs` names three paragraphs above the function that had it:
    /// the work order goes into a pane still drawing its banner.
    ///
    /// The fake keeps herdr's answer rather than the convenient one. That is
    /// the whole discipline: a fake that sent `Some(false)` would make this
    /// test pass, the port look right, and the bug arrive in production.
    ///
    /// **Repaired 2026-08-17 by t-260816-061**, which is what this assertion
    /// asked for in as many words: the reading moved out of `place.rs` and into
    /// the herdr adapter, where it takes a parsed reply instead of three loose
    /// arguments — so `Starting` now survives the trip, and there is no longer a
    /// way to hand it a shape herdr does not send.
    #[test]
    fn the_dialect_is_the_inverse_of_the_readings_wsp_makes() {
        let read = |spot: &Spot| crate::place_herdr::state_of_agent(&herdr::parse_pane(&agent_json(spot)));

        for state in
            [State::Empty, State::Starting, State::Idle, State::Working, State::Blocked, State::Unknown]
        {
            let spot = Spot::agent("s1", "claude", "t-1", state);
            assert_eq!(
                read(&spot),
                state,
                "{state:?} does not survive the round trip through herdr's words"
            );
        }

        let gone = Spot::agent("s1", "claude", "t-1", State::Gone);
        assert_eq!(
            read(&gone),
            State::Empty,
            "herdr's blind spot has been quietly fixed by the fake, which makes tests green on a lie"
        );

        // The launch window is the one that mattered, and the absence is still
        // the only thing marking it: nothing here was made to send a readiness
        // herdr does not send.
        let starting = Spot::agent("s1", "claude", "t-1", State::Starting);
        assert_eq!(of_state(&starting).2, None, "herdr was made to send a readiness it does not send");
        assert!(!read(&starting).will_take_a_prompt(), "the port would tell a caller to prompt a banner");
    }

    /// `done` is the one status in herdr's vocabulary with no `Spot`, and the
    /// asymmetry is deliberate rather than missing — so it is asserted here
    /// instead of being left to be discovered as a hole.
    ///
    /// herdr sends it for an agent that went idle while nobody was looking
    /// (`app/api_helpers.rs:104`), which makes it a fact about a viewer's
    /// window rather than about an agent. The adapter reads it as `Idle`, so it
    /// cannot round-trip, and a fake that gave it its own state would be
    /// offering wsp a distinction wsp has decided not to have.
    #[test]
    fn done_is_a_word_herdr_uses_for_idle_and_the_fake_never_sends_it() {
        let done = herdr::parse_pane(&json!({
            "pane_id": "s1", "agent": "claude", "agent_status": "done", "interactive_ready": true,
        }));
        assert_eq!(crate::place_herdr::state_of_agent(&done), State::Idle);
        assert!(
            crate::place_herdr::state_of_agent(&done).will_take_a_prompt(),
            "read as Unknown this agent was unreachable, and four of twelve on this machine were it"
        );

        for state in [State::Idle, State::Working, State::Blocked, State::Starting] {
            let spot = Spot::agent("s1", "claude", "t-1", state);
            assert_ne!(of_state(&spot).1, "done", "{state:?} was given herdr's viewer flag");
        }
    }

    /// The launch window is something [`Stage::unnamed`] *does*, not something
    /// it *is* — so a stage of agents that are already up is unaffected by it
    /// being on by default.
    ///
    /// Worth pinning because the default is the risky half of that change: every
    /// existing fixture builds its seats with agents already in them and never
    /// calls `agent.start`, so if the window were a property of a seat rather
    /// than of a start, turning it on would have made the fake disagree with a
    /// herdr that finished launching an hour ago. The countdown is armed by
    /// `agent.start` and by nothing else, which is what these two readings say.
    #[test]
    fn a_stage_of_agents_that_are_already_up_never_enters_the_launch_window() {
        let env = util::isolated("fake-already-up");
        let stage = Stage::of(vec![Spot::agent("w1:p1", "claude", "t-1", State::Idle)]);
        assert!(stage.unnamed > 0, "the window is off, and this asserts nothing");
        let fake = Fake::bind(env.path("herdr.sock"), stage).unwrap();
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let read = || {
            let v = herdr::call("agent.get", json!({ "target": "w1:p1" })).expect("the fake answered");
            herdr::parse_pane(v.get("agent").expect("a record"))
        };
        assert_eq!(read().agent, "claude", "a seat nobody started went blind");
        assert_eq!(read().agent, "claude", "…and again, so this is not a countdown running out");

        // Armed by a start and by nothing else, so the same seat read *after*
        // one is in the window. The pair is the field: same stage, same seat,
        // and the only thing between the two readings is `agent.start`.
        herdr::call("agent.start", json!({ "pane_id": "w1:p1", "kind": "claude", "name": "t-1" }))
            .expect("started");
        let coming_up = read();
        assert_eq!(coming_up.agent, "", "the launch window named an agent");
        assert_eq!(coming_up.launch_pending, Some(true));
        assert_eq!(
            crate::place_herdr::state_of_agent(&coming_up),
            State::Empty,
            "which wsp reads as an empty seat, and that is the whole of t-260817-010"
        );
    }

    /// Which readings can tell a starting agent from an idle one, recorded
    /// rather than reasoned about — because `place.rs` reasons about it and
    /// gets it half right.
    ///
    /// The port says a listing cannot carry `interactive_ready` and so answers
    /// with the state that can lie. True of `pane.list`, whose rows have no
    /// such field in herdr's schema and never carry one. **False of
    /// `agent.list`**, which carries the same `AgentInfo` as `agent.get` and
    /// was recorded carrying `interactive_ready: true` on a ready agent. So the
    /// asymmetry is between *panes and agents*, not between one and many, and
    /// wsp only fails to see it because `herdr::Pane` does not parse the field.
    #[test]
    fn a_pane_reading_cannot_tell_starting_from_idle_and_an_agent_reading_can() {
        let starting = Spot::agent("w1:p1", "claude", "t-1", State::Starting);
        let idle = Spot::agent("w1:p1", "claude", "t-1", State::Idle);

        for spot in [&starting, &idle] {
            let pane = pane_json(spot);
            assert!(pane.get("interactive_ready").is_none(), "a pane row invented a field herdr has not got");
            assert_eq!(pane["agent_status"], "idle", "both look idle to a pane listing, and must");
        }

        let coming_up = agent_json(&starting);
        assert!(coming_up.get("interactive_ready").is_none(), "starting is the absence of the field");
        assert_eq!(coming_up["launch_pending"], true);

        let ready = agent_json(&idle);
        assert_eq!(ready["interactive_ready"], true, "the reading that can say yes did not");
        assert!(ready.get("launch_pending").is_none(), "the two fields were sent together, which herdr never does");
    }

    /// A pane with nobody in it is `agent_status: "unknown"` with no `agent`
    /// field — recorded from herdr 0.7.5, and not what a reader would guess.
    /// The panel draws `·` from exactly this, and `sync` decides what to reap
    /// from it.
    #[test]
    fn an_empty_seat_says_unknown_and_names_no_agent() {
        let v = pane_json(&Spot::empty("w1:p1").at("/tmp"));
        assert_eq!(v["agent_status"], "unknown");
        assert!(v.get("agent").is_none(), "an empty pane carried an agent field");
        assert_eq!(crate::place_herdr::state_of_agent(&herdr::parse_pane(&v)), State::Empty);
    }

    /// The whole point of the thing: wsp's own client, over a real socket, and
    /// a state nobody could put a herdr into — a seat that is starting, one
    /// that is working, one that has gone and a shell with nobody in it.
    #[test]
    fn wsps_own_client_reads_a_state_no_herdr_could_be_put_in() {
        let env = util::isolated("fake-client");
        let stage = Stage::of(vec![
            Spot::agent("w1:p1", "claude", "t-1", State::Starting).labelled("one").on("w1", "w1:t1"),
            Spot::agent("w1:p2", "claude", "t-2", State::Working).labelled("two").on("w1", "w1:t1"),
            Spot::agent("w2:p1", "claude", "t-3", State::Gone)
                .session("s-3")
                .labelled("three")
                .on("w2", "w2:t1"),
            Spot::empty("w2:p2").labelled("a shell").on("w2", "w2:t1"),
        ]);
        let fake = Fake::bind(env.path("herdr.sock"), stage).unwrap();
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let panes = herdr::panes().expect("the fake answered");
        assert_eq!(panes.len(), 4, "every seat is a pane, whatever is in it");
        assert_eq!(panes[0].agent, "claude");
        assert_eq!(panes[0].agent_status, "idle", "starting reads as idle from a listing");
        assert_eq!(panes[1].agent_status, "working");
        assert_eq!(panes[2].agent, "", "an agent that has gone leaves a pane that looks like a shell");
        // And it takes its session with it, recorded against a live herdr on
        // 2026-08-17 by killing a spawned Claude Code and reading the next
        // `pane.list`. This is the reading `cmd_agent::sessions_learned` must
        // not act on: it is silence, not a correction, and treating it as one
        // would erase the id `claude --resume` needs at the one moment it is
        // wanted.
        assert_eq!(panes[2].session_id, "", "a dead agent's pane offers no session id");
        assert_eq!(panes[3].agent_status, "unknown");

        // …and `agent.list` is the narrower question, which a gone agent is not
        // an answer to.
        let agents = herdr::agents().expect("the fake answered");
        assert_eq!(agents.len(), 2, "a gone agent was listed as a live one");
    }

    /// Silence, in both its forms, is not an empty list. This is t-260816-058's
    /// class of bug, at the seam it came in through — and it is the state the
    /// task exists for, since a live herdr cannot be asked to stop answering.
    #[test]
    fn a_backend_that_is_not_answering_is_an_error_and_never_an_empty_list() {
        let env = util::isolated("fake-quiet");
        let fake = Fake::bind(
            env.path("herdr.sock"),
            Stage::of(vec![Spot::agent("w1:p1", "claude", "t-1", State::Idle)]),
        )
        .unwrap();
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        assert_eq!(herdr::panes().expect("answering").len(), 1);

        // Hangs up: arrives at once, and is what a server restarting under a
        // caller looks like.
        fake.goes(Quiet::HangsUp);
        assert!(herdr::panes().is_err(), "a hang-up was read as a machine with no panes");

        fake.goes(Quiet::No);
        assert_eq!(herdr::panes().expect("answering again").len(), 1);
    }

    /// The order `spawn` depends on, driven end to end: open a seat, start an
    /// agent in it, find it will not take a prompt yet, and only then tell it
    /// something. The recording is the assertion, and it is in the port's
    /// words rather than herdr's.
    #[test]
    fn a_spawn_shaped_conversation_is_recorded_as_the_verbs_it_was() {
        let dir = scratch("spawn");
        let mut stage = Stage::new();
        // The window a live herdr passes through in half a second whether you
        // want it to or not.
        stage.settle = false;
        let fake = Fake::bind(dir.join("herdr.sock"), stage).unwrap();

        let made = call(&fake, "workspace.create", json!({ "label": "r/1", "cwd": "/tmp", "focus": true }))
            .expect("a seat");
        let seat = made["root_pane"]["pane_id"].as_str().unwrap().to_string();

        call(&fake, "agent.start", json!({ "pane_id": seat, "kind": "claude", "name": "t-1" }))
            .expect("an agent");

        // Starting, and it says so in the one reading that can.
        let got = call(&fake, "agent.get", json!({ "target": seat })).expect("a reading");
        assert!(got["agent"].get("interactive_ready").is_none(), "the fake was helpful and lied");

        let refused = call(&fake, "agent.prompt", json!({ "target": seat, "text": "go" }))
            .expect_err("a prompt into a starting agent");
        assert!(refused.contains("agent_not_ready"), "{refused}");

        fake.moves(&Seat::new(&seat), State::Idle);
        call(&fake, "agent.prompt", json!({ "target": seat, "text": "go" })).expect("now it takes one");

        assert_eq!(
            fake.verbs(),
            vec![Verb::Open, Verb::Start, Verb::Ask, Verb::Tell, Verb::Tell],
            "the recording is the conversation, in wsp's words"
        );
        let told = fake.asked().into_iter().find(|a| a.verb == Verb::Tell).unwrap();
        assert_eq!(told.said, "go");
        assert_eq!(told.seat, Some(Seat::new(&seat)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **robustness-035, on a socket.** A prompt herdr accepts and Claude Code
    /// never takes: `ok` on the wire, the sentence in the composer, the agent
    /// idle in front of it. Every reading `wsp spawn` had said the handover went
    /// well, and for one night it printed so.
    ///
    /// The rescue is the second half and is what was run by hand four times in
    /// one burst: a return, addressed to the agent, and the turn starts. Both
    /// halves are here because a fake that could only model the happy path is
    /// what let the defect live — nothing in the tree could express the failure,
    /// so nothing in the tree could fail on it.
    #[test]
    fn a_prompt_the_agent_never_takes_leaves_it_idle_until_something_presses_return() {
        let env = util::isolated("fake-unsent");
        let mut stage = Stage::of(vec![Spot::agent("w3M:p1", "claude", "robustness-035", State::Idle)]);
        stage.takes = false;
        let fake = Fake::bind(env.path("herdr.sock"), stage).unwrap();
        let seat = "w3M:p1";

        call(&fake, "agent.prompt", json!({ "target": seat, "text": "you have been claimed" }))
            .expect("the call is accepted, which is the whole trouble");
        let got = call(&fake, "agent.get", json!({ "target": seat })).expect("a reading");
        assert_eq!(got["agent"]["agent_status"], json!("idle"), "the failure cannot be modelled");

        call(&fake, "agent.send_keys", json!({ "target": seat, "keys": ["enter"] }))
            .expect("the recovery Ed ran by hand");
        let got = call(&fake, "agent.get", json!({ "target": seat })).expect("a reading");
        assert_eq!(got["agent"]["agent_status"], json!("working"), "the return did nothing");

        assert!(
            fake.verbs().contains(&Verb::Nudge),
            "a submit was recorded as though it were the sentence: {:?}",
            fake.verbs()
        );
    }

    /// The subscribe stream, which is the half neither port could test before:
    /// hold a connection open, change the state, and the event arrives on it.
    /// wsp's own subscriber reads it, so the qualification and the
    /// dots-to-underscores rename are exercised rather than assumed.
    #[test]
    fn a_state_change_reaches_a_watcher_that_was_already_listening() {
        let env = util::isolated("fake-watch");
        let fake = Fake::bind(
            env.path("herdr.sock"),
            Stage::of(vec![Spot::agent("w1:p1", "claude", "t-1", State::Idle)]),
        )
        .unwrap();
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let (tx, rx) = mpsc::channel::<(String, Value)>();
        std::thread::spawn(move || {
            let _ = herdr::subscribe(&["pane.updated", "pane.closed", "pane.agent_detected"], |name, data| {
                tx.send((name.to_string(), data.clone())).is_ok()
            });
        });
        // The stream has to be up before the change, or the test is asserting
        // nothing: a watcher that subscribes afterwards learns nothing at all,
        // which is the failure mode the daemon lives with. Asked of the register
        // the event will be delivered to rather than slept for — see
        // [`Fake::watched`].
        assert!(fake.watched(), "the subscriber never reached the fake");

        let seat = Seat::new("w1:p1");
        fake.moves(&seat, State::Working);
        let (name, data) = rx.recv_timeout(DELIVERY).expect("no event arrived");
        assert_eq!(name, "pane_updated", "the stream renames dots to underscores");
        assert_eq!(data["pane"]["agent_status"], "working");

        fake.closes(&seat);
        let (name, data) = rx.recv_timeout(DELIVERY).expect("no close arrived");
        assert_eq!(name, "pane_closed");
        assert_eq!(data["pane_id"], "w1:p1");
    }

    /// One entry missing its `pane_id` refuses **every other entry with it**,
    /// and the server then hangs up. Recorded from herdr 0.7.5, and reproduced
    /// rather than smoothed over: `daemon::EVENTS` and `panel::SHAPE` are both
    /// shaped by this refusal, and a fake that accepted the list would make
    /// both of them look arbitrary.
    #[test]
    fn one_per_pane_subscription_refuses_the_whole_list() {
        let env = util::isolated("fake-subs");
        let fake = Fake::bind(env.path("herdr.sock"), Stage::new()).unwrap();
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let err = herdr::subscribe(&["pane.created", "pane.agent_status_changed"], |_, _| true)
            .expect_err("a list herdr would have refused");
        assert!(err.to_string().contains("pane_id"), "{err}");

        // Naming a pane is what makes it legal, which is why the panel
        // subscribes per pane for those three and globally for the rest.
        assert!(subscription_error(&[json!({ "type": "pane.agent_status_changed", "pane_id": "w1:p1" })]).is_none());
        assert!(subscription_error(&[json!({ "type": "nonesuch.event" })]).is_some());
    }

    /// The split arithmetic, recorded: **the target keeps the ratio and the new
    /// pane gets the remainder**. A sidebar asked for at 22% of 54 columns
    /// arrives as the 42-column half, which is why installing one ends in a
    /// swap.
    #[test]
    fn a_split_leaves_the_ratio_with_the_pane_that_was_already_there() {
        let whole = Rect { x: 26, y: 1, w: 54, h: 23 };
        let (kept, made) = split_rect(whole, 0.22, false);
        assert_eq!((kept.x, kept.w), (26, 12));
        assert_eq!((made.x, made.w), (38, 42), "the new pane got the narrow half");
        assert_eq!(kept.w + made.w, whole.w, "columns went missing in the split");

        let (top, bottom) = split_rect(whole, 0.5, true);
        assert_eq!((top.y, top.h), (1, 12));
        assert_eq!((bottom.y, bottom.h), (13, 11));
    }

    /// Nothing here parses an id. herdr's are `w0:p3`, strata's D-55 is an
    /// index and a generation, and a seat the fake did not invent has to work
    /// exactly as well as one it did — otherwise the fake is quietly asserting
    /// herdr's shape of id, which is the assumption the ports exist to remove.
    #[test]
    fn a_seat_the_fake_did_not_invent_is_still_a_seat() {
        let dir = scratch("ids");
        let stage = Stage::of(vec![
            Spot::agent("sess_01HQ", "claude", "t-1", State::Idle).on("host-a", "host-a/main"),
            Spot::empty("7").on("host-a", "host-a/main"),
        ]);
        let fake = Fake::bind(dir.join("herdr.sock"), stage).unwrap();

        let panes = call(&fake, "pane.list", json!({})).expect("a listing");
        assert_eq!(panes["panes"][0]["pane_id"], "sess_01HQ");
        assert_eq!(panes["panes"][1]["pane_id"], "7");

        let got = call(&fake, "agent.get", json!({ "target": "sess_01HQ" })).expect("an agent");
        assert_eq!(got["agent"]["pane_id"], "sess_01HQ");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A diff is what a change *means* to a watcher, and the two that matter
    /// are the ones an absence could be mistaken for: a seat appearing with an
    /// agent already in it is two events, and an agent leaving a seat that
    /// stays is a stop rather than a close.
    #[test]
    fn what_changed_between_two_states_is_a_decision_about_absences() {
        let empty = Stage::new();
        let one = Stage::of(vec![Spot::agent("w1:p1", "claude", "t-1", State::Working)]);
        assert_eq!(
            Stage::diff(&empty, &one),
            vec![Event::Opened(Seat::new("w1:p1")), Event::Started(Seat::new("w1:p1"))],
            "a seat that arrives with somebody in it is both things happening"
        );

        let gone = Stage::of(vec![Spot::agent("w1:p1", "", "", State::Gone)]);
        assert_eq!(
            Stage::diff(&one, &gone),
            vec![Event::Stopped(Seat::new("w1:p1"))],
            "an agent leaving a seat that is still there is not the seat closing"
        );
        assert_eq!(
            Stage::diff(&one, &empty),
            vec![Event::Closed(Seat::new("w1:p1"))],
            "the seat itself going is the other event, and they are not the same news"
        );
        assert!(Stage::diff(&one, &one).is_empty(), "a world that did not change raised events");
    }

    /// A stage is written by a person in a text file, so the defaults have to
    /// be the boring answers and a seat has to be one line.
    #[test]
    fn a_stage_written_by_hand_fills_in_what_it_did_not_say() {
        let stage = stage_from_json(&json!({
            "seats": [ { "agent": "claude", "name": "t-1" }, { }, { "state": "working", "agent": "codex" } ]
        }));
        assert_eq!(stage.spots.len(), 3);
        assert_eq!(stage.spots[0].state, State::Idle, "an agent with no state named is waiting");
        assert_eq!(stage.spots[1].state, State::Empty, "a seat with nobody in it is a shell");
        assert_eq!(stage.spots[2].state, State::Working);

        // Ids are invented herdr-style so that a sandbox reads like a machine,
        // and each seat gets a screen of its own — a workspace per task, which
        // is what wsp opens.
        assert_eq!(stage.spots[0].seat.as_str(), "w1:p1");
        assert_eq!(stage.spots[1].seat.as_str(), "w2:p1");
        assert_ne!(stage.spots[0].space, stage.spots[1].space);

        // And it round-trips, so `--json` says the same thing the file does.
        let again = stage_from_json(&stage_to_json(&stage));
        assert_eq!(again.spots, stage.spots);
        assert!(again.settle);
    }

    /// A method wsp has never called is refused rather than answered. A fake
    /// that improvises is a fake that goes on answering after herdr has stopped
    /// — and the refusal carries the fake's own error code, because inventing a
    /// herdr one teaches a caller to match on a string no server sends.
    #[test]
    fn a_method_nothing_calls_is_refused_in_the_fakes_own_words() {
        let dir = scratch("unknown");
        let fake = Fake::bind(dir.join("herdr.sock"), Stage::new()).unwrap();
        let err = call(&fake, "layout.apply", json!({})).expect_err("answered a method nothing calls");
        assert!(err.contains(OUR_OWN), "{err}");
        assert!(!err.contains("pane_not_found"), "the fake borrowed a herdr code for its own refusal");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
