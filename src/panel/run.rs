//! The pane: the terminal it runs in, and the loop that keeps it honest.
//!
//! Everything impure lives here. Raw mode, the input thread, herdr's event
//! stream, the tick, and the loop that turns an [`Effect`] into something that
//! actually happens.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::herdr;
use crate::input::Key;
use crate::store::Store;
use crate::util::exe_stamp;

use super::keys::{apply_key, say, Effect, Mode, View};
use super::render::{frame, to_ansi};
use super::rows::{collect, refetch_into, AgentRef, Cursor, Snapshot, Target, Ui};
use super::shared;
use super::verbs::{close_view, inspect, pop_out, run_wsp, send_tell, Tell};

pub(super) enum Msg {
    Key(Key),
    Herdr(HerdrEvent),
    Tick,
    /// A line for the footer from something that took too long to do on the
    /// loop. `S` and `O` produce these — starting an agent means waiting on
    /// `claude` to answer, and the sidebar must go on drawing while it does —
    /// and so does every sentence typed at an agent, which waits on the clear
    /// in front of it.
    Note(String),
}

/// A herdr event, reduced to what the loop decides with.
pub(super) struct HerdrEvent {
    /// The workspace it was about, when it named one.
    pub(super) workspace: Option<String>,
    /// A pane or workspace appeared, went, or gained an agent. See [`SHAPE`].
    pub(super) shape: bool,
    /// Focus moved to some workspace. Only interesting when that workspace is
    /// ours, and then it is very interesting — see the loop.
    pub(super) focus: bool,
}

pub(crate) fn stty(args: &[&str]) {
    if let Ok(tty) = File::open("/dev/tty") {
        let _ = Command::new("stty")
            .args(args)
            .stdin(Stdio::from(tty))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub(crate) fn term_size() -> (usize, usize) {
    if let Ok(tty) = File::open("/dev/tty") {
        if let Ok(out) = Command::new("stty").arg("size").stdin(Stdio::from(tty)).output() {
            let s = String::from_utf8_lossy(&out.stdout);
            let mut it = s.split_whitespace();
            if let (Some(r), Some(c)) = (it.next(), it.next()) {
                if let (Ok(r), Ok(c)) = (r.parse::<usize>(), c.parse::<usize>()) {
                    return (c.max(16), r.max(6));
                }
            }
        }
    }
    (26, 40)
}

pub(super) fn spawn_input(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let Ok(mut tty) = File::open("/dev/tty") else { return };
        let mut buf = [0u8; 1];
        let mut keys = crate::input::Keys::new();
        let mut out: Vec<Key> = Vec::new();
        loop {
            match tty.read(&mut buf) {
                Ok(1) => keys.feed(buf[0], &mut out),
                // `stty min 0 time 1` makes a read with nothing waiting come
                // back empty after ~100ms, and that silence is the only thing
                // that tells a bare Esc from the start of an arrow key.
                Ok(_) => keys.idle(&mut out),
                // A tty that has started erroring will keep erroring, so this
                // must not become a spin. Treat it as silence, slowly.
                Err(_) => {
                    keys.idle(&mut out);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            for k in out.drain(..) {
                if tx.send(Msg::Key(k)).is_err() {
                    return;
                }
            }
        }
    });
}

/// Rare, and every one of them changes which rows exist: a pane opened or
/// closed, a workspace came or went, a pane picked up an agent or lost one.
/// These refetch wherever the panel is standing, focused or not.
///
/// That is deliberately not free. Twenty-two background panels each take a
/// store read and two socket calls when any of these arrives — but they arrive
/// a handful of times a minute, and the dock is the one part of the frame that
/// cannot be allowed to go half a minute stale. It is the answer to "who is
/// free right now", and an answer that old is not an answer.
///
/// `pane.agent_detected` is the event a new agent raises, and herdr raises it
/// again when the agent goes away. `pane.agent_status_changed` is not here and
/// cannot be: it is per-pane and its request requires a `pane_id`, so asking
/// for it globally refuses the whole list — see the note in [`crate::herdr`].
///
/// An agent *finishing* belongs in this list too and cannot be in it, because
/// no event means it and none can. [`status_changed`] recovers what it can by
/// promoting the one `pane.updated` in a hundred that carries a status the pane
/// did not have before, and that is worth having — but it is not a guarantee,
/// and [`STATUS_POLL`] says why and covers the rest.
const SHAPE: &[&str] = &[
    "workspace.created",
    "workspace.closed",
    "pane.created",
    "pane.closed",
    "pane.exited",
    "pane.agent_detected",
];

/// Subscribed to, and deliberately not urgent. herdr emits `workspace.focused`
/// on every sidebar hover — about once a second — and `pane.updated` about
/// twice a second for each agent that is working. Refetching on those would
/// have every panel on the machine doing socket round-trips continuously to
/// keep a glyph honest. They mark the panel dirty and the tick gate coalesces
/// them to the cadence it already keeps: 250ms when the panel is being looked
/// at, 30s when it is not.
///
/// `pane.updated` is the only global carrier of an agent going idle or working,
/// which is what the "wants you" arrow is drawn from, so it has to be here even
/// though it is the noisiest thing herdr sends. Chatty is what it is on
/// average, not what it always is: the one in a hundred of them that carries a
/// status the pane did not have before is promoted to structural by
/// [`status_changed`], and the other ninety-nine go on being coalesced.
const CHATTY: &[&str] = &["workspace.focused", "workspace.renamed", "pane.updated"];

/// How often an unfocused panel asks herdr, itself, who has stopped.
///
/// Nothing pushes agent status to herdr. The hook it installs —
/// `~/.claude/hooks/herdr-agent-state.sh` — exits without a word for every
/// action but `session`, and the one message it does send is
/// `pane.report_agent_session`, carrying a session id and a transcript path. It
/// goes out of its way to drop `SubagentStop`. herdr classifies from that
/// transcript: `pane.list` reports `agent_session {kind: "id", value: <uuid>}`
/// per pane, and `blocked` is among the statuses, which nothing reading a PTY
/// could know.
///
/// That is the whole asymmetry. An agent *starting* is an edge — a write to a
/// file — so a filesystem notification can carry it at once, and does: measured
/// against the live server, 0.2s. An agent *finishing* is the **absence** of
/// writes for some quiet interval, and nothing fires on the passing of time.
/// Worse, the last write to the transcript is the agent's closing message,
/// which still looks like work. Only a timer can see a finish, and herdr's runs
/// on its own schedule: the event carrying one came 6.8s late once and 28.9s
/// late once, with 147s of unbroken silence on the stream across the second.
///
/// `pane.list` evaluates the same rule when it is asked, so it sees the absence
/// for free. That makes it not the fresher of two channels but the *only* one
/// that can report a finish on time — and a panel that waits for the event
/// instead is the half-minute of staleness that reads as broken from a chair.
///
/// Hence a poll, and one this side of human patience. It is far cheaper than
/// the refetch it guards: one socket call, where a refetch also reads the whole
/// store off disk and rebuilds every row. Only unfocused panels run it, because
/// a focused one already refetches four times a second.
const STATUS_POLL: Duration = Duration::from_secs(3);

/// Has any pane changed the status the frame was drawn with?
///
/// The poll-side twin of [`status_changed`], and it keeps the same rule for the
/// same reason: only panes in both are compared. A pane arriving or leaving is
/// structural, [`SHAPE`] already refetches on it at once, and counting it here
/// as well would refetch twice for one event.
fn status_moved(drawn: &HashMap<String, String>, now: &[herdr::Pane]) -> bool {
    now.iter().any(|p| drawn.get(&p.pane_id).is_some_and(|was| was != &p.agent_status))
}

/// Subscriptions herdr scopes to a single pane. Its request struct for these
/// requires a `pane_id`, and there is no wildcard — `*` and `""` both answer
/// `pane_not_found`. Naming one here without a pane refuses the whole list,
/// which is how the panel spent its life receiving no events at all.
#[cfg(test)]
const PER_PANE: &[&str] =
    &["pane.agent_status_changed", "pane.output_matched", "pane.scroll_changed"];

/// The stream names events with underscores where the subscription used dots —
/// `pane.agent_detected` is asked for that way and arrives as
/// `pane_agent_detected`.
fn is(types: &[&str], event: &str) -> bool {
    types.iter().any(|t| t.replace('.', "_") == event)
}

/// Did this event say an agent started or stopped, as against saying anything
/// else at all?
///
/// `pane.updated` is the only global carrier of an agent's status and also the
/// noisiest thing herdr sends: it fires on every spinner frame in a terminal
/// title and every token count, ten a second with four agents running, and
/// carries the whole pane each time. The status itself changes a few times a
/// minute. `seen` is the only thing that can tell those apart — the event does
/// not say what it was before, so a reducer that keeps no history has to treat
/// a stop and a spinner frame identically, which is why an agent finishing sat
/// on the 30s cadence while an agent appearing did not.
///
/// A pane met for the first time is recorded and is not a change. Every pane on
/// the machine arriving at once is what the first second of a subscription
/// looks like, and it is not news about any of them.
fn status_changed(seen: &mut HashMap<String, String>, d: &Value) -> bool {
    let Some(pane) = d.get("pane") else { return false };
    let Some(id) = pane.get("pane_id").and_then(|x| x.as_str()) else { return false };
    // A pane with no agent in it has no status to change. herdr sends the field
    // as an empty string rather than leaving it out.
    let status = pane.get("agent_status").and_then(|x| x.as_str()).unwrap_or_default();
    if status.is_empty() {
        return false;
    }
    match seen.insert(id.to_string(), status.to_string()) {
        Some(before) => before != status,
        None => false,
    }
}

pub(super) fn spawn_events(tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        let tx2 = tx.clone();
        let types: Vec<&str> = SHAPE.iter().chain(CHATTY).copied().collect();
        // Per subscription rather than per panel: a dropped stream is a gap we
        // cannot describe, so the map starts empty and the first event about
        // each pane teaches it rather than firing. The background tick covers
        // whatever changed while nobody was listening.
        let mut seen: HashMap<String, String> = HashMap::new();
        let res = herdr::subscribe(&types, move |e, d| {
            // `pane.created` and `pane.updated` carry the pane as an object and
            // the workspace id inside it; the rest put it at the top level.
            // Reading only the top level made every event about our own pane
            // look like an event about somebody else's.
            let ws = d
                .get("workspace_id")
                .or_else(|| d.get("pane").and_then(|p| p.get("workspace_id")))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            // First, and not behind the `||`: every event carrying a pane
            // teaches the map, including the structural ones. A `pane.created`
            // that did not would leave the pane's opening status unrecorded,
            // and the next `pane.updated` would read as a change.
            let changed = status_changed(&mut seen, d);
            let ev = HerdrEvent {
                workspace: ws,
                shape: changed || is(SHAPE, e),
                focus: e == "workspace_focused",
            };
            tx2.send(Msg::Herdr(ev)).is_ok()
        });
        if res.is_err() {
            std::thread::sleep(Duration::from_secs(3));
        } else {
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// Did this command make something that was not there before?
///
/// It decides two things at once: whether the cursor jumps to the id the
/// command answered with, and whether the footer says `… added · E to write it
/// up` — a sentence that names a key, and so also takes the keyboard.
///
/// Spelt out as verbs rather than read off the first word. `project` used to be
/// enough, back when `project add` was the only one of them a key could run.
/// `project rm` answers `{"removed": <id>}` and `project set` answers the whole
/// project, so both carry an id out of [`run_wsp`] — and under the old test
/// both landed here, moved the cursor, took the keyboard, and announced that
/// the thing you had just renamed or removed had been added.
fn creates(argv: &[String]) -> bool {
    let word = |i: usize| argv.get(i).map(|s| s.as_str());
    matches!((word(0), word(1)), (Some("add"), _) | (Some("project"), Some("add")))
}

/// Make this pane the focused one.
///
/// The mouse reaches an unfocused pane — that is what makes the panel worth
/// pointing at — but it means scrolling or clicking here leaves the keyboard
/// somewhere else, so the next `d` or `↵` goes to a pane you are not looking
/// at. Touching the panel is a statement about where you are working.
///
/// The workspace first, then the pane: focusing a pane in a workspace that is
/// not focused moves nothing you can see.
fn focus_self(pane: Option<&str>, ws: Option<&str>) {
    if let Some(ws) = ws {
        let _ = herdr::call("workspace.focus", json!({ "workspace_id": ws }));
    }
    if let Some(p) = pane {
        let _ = herdr::call("pane.focus", json!({ "pane_id": p }));
    }
}

/// Does the keyboard belong to this pane, according to herdr's own census?
///
/// One pane on the machine answers yes: `pane.list` marks the focused pane of
/// the focused workspace and nothing else, which is why this is a census
/// question rather than a workspace one. `self_focused` is the other half of the
/// same picture and is not a substitute — a panel in the workspace being looked
/// at is on screen, and the shell beside it may still be the pane being worked
/// in.
///
/// Current because the loop refetches the moment focus lands anywhere in this
/// workspace, and every 250ms while it is on screen; see the `focused_us` branch
/// for why that covers the keyboard crossing between two panes of one workspace.
///
/// A panel with no pane id of its own — run outside herdr — answers yes, the
/// same way `collect` treats a workspace it cannot find. Something that cannot
/// tell whether it is being worked in should act on what it is told rather than
/// refuse.
fn holds_keyboard(snap: &Snapshot, me: Option<&str>) -> bool {
    let Some(me) = me else { return true };
    snap.panes.iter().find(|p| p.pane_id == me).map(|p| p.focused).unwrap_or(true)
}

pub(super) fn focus(agent: &AgentRef) {
    let _ = herdr::call("workspace.focus", json!({ "workspace_id": agent.workspace }));
    let _ = herdr::call("pane.focus", json!({ "pane_id": agent.pane }));
}

/// Type a work order into an agent's pane, off the loop.
///
/// Off it for the same reason a spawn is: the sentence now goes in behind a
/// clear, and a clear is a second of waiting for the session that replaces the
/// one it ended — a second in which this pane would draw nothing, right after
/// a keystroke, which reads as a panel that has died rather than one that is
/// busy. The footer says what the key did as the key is pressed; the thread
/// speaks only if the typing itself failed.
fn tell(t: Tell, ui: &mut Ui, tx: &Sender<Msg>) {
    say(ui, t.note.clone());
    let tx = tx.clone();
    std::thread::spawn(move || {
        if let Err(e) = send_tell(&t) {
            let _ = tx.send(Msg::Note(e));
        }
    });
}

/// Why the loop stopped.
pub(super) enum Outcome {
    Quit,
    /// The binary changed on disk. Twenty-two panes each holding a stale image
    /// is a real cost while this is under active development — a key silently
    /// doing what it used to do is worse than one that errors.
    Reload,
}

pub fn run(store: &Store) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let self_ws = herdr::Env::read().workspace_id;

    let (tx, rx) = mpsc::channel::<Msg>();
    spawn_input(tx.clone());
    spawn_events(tx.clone());
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(200));
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        });
    }

    // `?1000h` reports button presses and releases; `?1006h` asks for them in
    // SGR form, which is the only encoding with coordinates past column 223 —
    // the older one packs them into single bytes and simply cannot say 240.
    // Motion reporting (`?1002`/`?1003`) is deliberately not asked for: it
    // floods the pane for a feature nobody wanted, and the terminal's own text
    // selection is worth more than anything we would build on top of it.
    print!("\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h");
    let _ = std::io::stdout().flush();
    stty(&["raw", "-echo", "min", "0", "time", "1"]);

    let outcome = event_loop(store, &tx, &rx, self_ws.as_deref());

    stty(&["sane"]);
    // Off in the reverse order, and before the alternate screen goes: a pane
    // left reporting mouse events into a shell prints gibberish on every click.
    print!("\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();

    if let Outcome::Reload = outcome {
        // Replace this process rather than spawning beside it: the pane, its
        // pty and its place in the layout all survive, and nothing has to
        // reattach.
        if let Ok(exe) = std::env::current_exe() {
            use std::os::unix::process::CommandExt;
            let err = Command::new(exe).arg("panel").exec();
            eprintln!("wsp: could not reload: {err}");
            return 1;
        }
    }
    0
}

/// Take the shared view onto ours if it is not already ours, and answer with
/// the row it wants the cursor on and the text we now agree with.
///
/// `agreed` is the last text this panel wrote or adopted. Equal means nobody
/// else has touched it and there is nothing to take; `None` from the file means
/// nothing has been written yet, which is a first run rather than an error.
fn adopt(store: &Store, view: &mut View, agreed: &mut String) -> Cursor {
    match shared::read(store) {
        Some(text) if text != *agreed => {
            let want = shared::parse(&text).apply(view);
            *agreed = text;
            want
        }
        _ => Cursor::default(),
    }
}

/// Put the cursor on a row by what it *is*, never by where it was. Rows move
/// under it constantly — the tree sorts by work — and an index carried across
/// a rebuild lands on whatever slid into the slot.
///
/// Answers whether it found the row. A row can be absent for a while and then
/// appear — a task under a project that was folded when the cursor arrived, or
/// one past the per-project cap — so the caller holds the wish until it lands
/// rather than dropping it on the first rebuild that could not honour it.
fn point_at(ui: &mut Ui, want: &Cursor) -> bool {
    if want.target == Target::Nothing {
        return true;
    }
    match want.find_in(&ui.rows, ui.tree_len()) {
        Some(i) => {
            ui.sel = i;
            true
        }
        None => false,
    }
}

pub(super) fn event_loop(
    store: &Store,
    tx: &Sender<Msg>,
    rx: &Receiver<Msg>,
    self_ws: Option<&str>,
) -> Outcome {
    let started_as = exe_stamp();
    let mut view = View::default();
    // Open on what the last panel was showing rather than on a default tree.
    // A panel installed in every workspace is one panel as far as anyone using
    // it is concerned, and it should not lose the folds and the cursor every
    // time herdr swaps which of them is on screen.
    // What we last wrote or took. Held so a key that changes nothing durable —
    // most of them — costs no write, and so a panel never adopts its own state
    // back on top of itself.
    let mut agreed = String::new();
    // A row the shared view wants the cursor on, held until it exists. Cleared
    // by any key, because a person moving the cursor outranks a wish taken off
    // disk before they did.
    let mut want = adopt(store, &mut view, &mut agreed);
    // Who we are, for taking focus when the mouse says the reader is here, and
    // for knowing whether it was already here when they clicked.
    let me = herdr::Env::read().pane_id;
    let snap = Snapshot::live(store);
    // Whether the keyboard is in this pane. Kept by the loop rather than read
    // off the frame: it decides what a click means, and a click is the one
    // gesture that arrives in a pane nobody is working in.
    let mut keyboard = holds_keyboard(&snap, me.as_deref());
    let mut ui = collect(&snap, &view, self_ws);
    if point_at(&mut ui, &want) {
        want = Cursor::default();
    }
    if agreed.is_empty() {
        agreed = shared::rendered(&shared::Shared::of(&view, ui.cursor()));
    }
    let mut last = String::new();
    let mut dirty = false;
    let mut last_fetch = Instant::now();
    // A scroll is a burst of events, and focus is a socket round-trip. Take it
    // on the first of a burst and not on the ninety after it.
    let mut took_focus = Instant::now() - Duration::from_secs(60);
    let mut last_fingerprint = store.fingerprint();
    // The status the frame in front of the reader was drawn with, and when we
    // last asked herdr whether it still holds. See [`STATUS_POLL`].
    let mut drawn_status: HashMap<String, String> = HashMap::new();
    let mut last_poll = Instant::now();
    // Messages taken off the channel while coalescing a burst of herdr events,
    // to be handled in the order they arrived rather than thrown away.
    let mut carry: std::collections::VecDeque<Msg> = Default::default();

    // `&mut` on the view because the frame is where the tree's scroll offset
    // is decided, and the view keeps it: the click handler two branches below
    // has to read the offset the pane in front of the reader is drawn with.
    let draw = |ui: &Ui, view: &mut View, last: &mut String| {
        let (w, h) = term_size();
        let painted = to_ansi(&frame(ui, view, w, h), w, h);
        if painted != *last {
            print!("{painted}");
            let _ = std::io::stdout().flush();
            *last = painted;
        }
    };
    draw(&ui, &mut view, &mut last);

    loop {
        let mut msg = match carry.pop_front() {
            Some(m) => m,
            None => match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => Msg::Tick,
                Err(RecvTimeoutError::Disconnected) => return Outcome::Quit,
            },
        };

        // Select, then activate. One click moves the cursor and does nothing
        // else; a click on the row already under it means what `↵` means, so
        // it *becomes* that key rather than restating what opening a row does.
        // A click that both selects and opens is how you end up somewhere you
        // did not ask to be, on a row you had not read — and here `↵` can
        // focus another pane, so that is a terminal you were not looking at.
        //
        // Translated before the dispatch rather than inside it: only the loop
        // knows the pane's size, and `render::row_at` is the arithmetic that
        // drew the frame, so the row under the pointer is the row that acts.
        //
        // Whether the pane was being worked in is read before the line that
        // changes it, because that is the question the click is answering.
        let had_keyboard = keyboard;
        if matches!(msg, Msg::Key(Key::Click { .. } | Key::Wheel { .. }))
            && took_focus.elapsed() > Duration::from_millis(400)
        {
            took_focus = Instant::now();
            focus_self(me.as_deref(), self_ws);
            // `focus_self` is a round-trip that has already returned, so this is
            // not a guess: the keyboard is here, and the next click within the
            // 400ms above — which does not ask herdr again — knows it.
            keyboard = true;
        }

        if let Msg::Key(Key::Click { x, y }) = msg {
            let (w, h) = term_size();
            match super::keys::click(&mut ui, &mut view, w, h, x, y, had_keyboard) {
                super::keys::Hit::Activate => msg = Msg::Key(Key::Enter),
                // A mark in the strip is an agent, and going there is the whole
                // of what it means.
                super::keys::Hit::Focus(a) => {
                    focus(&a);
                    keyboard = false;
                    continue;
                }
                // The `⋯`: the agents the strip could not draw. It stands for
                // the same thing `w` does, so it presses it.
                super::keys::Hit::Rest => msg = Msg::Key(Key::Char('w')),
                super::keys::Hit::Select => {
                    shared::share(store, &view, ui.cursor(), &mut agreed);
                    draw(&ui, &mut view, &mut last);
                    continue;
                }
                // The pane was not the one being worked in, and now it is. That
                // is the whole of what the click did: the frame has not changed
                // and there is nothing to draw.
                super::keys::Hit::Keyboard => continue,
                super::keys::Hit::Nothing => continue,
            }
        }

        // The wheel moves the view, so it needs the pane's size the way a
        // click does: how far three rows is depends on nothing, but where the
        // last screen ends depends on how many rows the tree has been given.
        //
        // It is not gated on the keyboard being here the way a click is: a
        // wheel cannot send you anywhere, so there is nothing to bounce, and a
        // pane you cannot scroll without clicking into it first is worse at the
        // one thing the panel is for — being read from across the screen.
        if let Msg::Key(Key::Wheel { up }) = msg {
            let (w, h) = term_size();
            super::keys::wheel(&mut ui, &mut view, w, h, up);
            shared::share(store, &view, ui.cursor(), &mut agreed);
            draw(&ui, &mut view, &mut last);
            continue;
        }

        let mut refetch = false;
        let is_key = matches!(msg, Msg::Key(_));
        if is_key {
            // The person is driving. Whatever the file wanted, they want this.
            want = Cursor::default();
        }
        match msg {
            Msg::Key(k) => match apply_key(k, &mut ui, &mut view) {
                Effect::Quit => return Outcome::Quit,
                Effect::None => {}
                Effect::Refetch => refetch = true,
                // The keyboard has gone to that agent's terminal, and this pane
                // is one somebody is looking at rather than working in until
                // they come back. Said here rather than waited for: the census
                // will agree within the tick, and a click landing inside it
                // would be the bounce with a stopwatch on it.
                Effect::Focus(a) => {
                    focus(&a);
                    keyboard = false;
                }
                Effect::Sync => {
                    let mut cache = crate::sync::Cache::default();
                    let _ = crate::sync::sync(store, &mut cache, true);
                    ui.message = Some(("synced".into(), Instant::now()));
                    refetch = true;
                }
                Effect::Inspect(focus) => {
                    let msg = inspect(store, self_ws, &focus);
                    if msg.is_empty() {
                        view.showing = Some(focus);
                    } else {
                        say(&mut ui, msg);
                    }
                }
                Effect::CloseView => {
                    if close_view(store, self_ws) {
                        say(&mut ui, "closed");
                    }
                    view.showing = None;
                }
                Effect::PopOut { argv, label } => {
                    say(&mut ui, pop_out(&argv, &label, self_ws));
                }
                // Off the loop, deliberately. `wsp spawn --agent` creates a
                // workspace, claims into it, waits for an agent to boot and
                // then tells it what it holds — seconds, not milliseconds, and
                // every one of them a frame the panel would not have drawn.
                // The footer says what is happening now and the thread says how
                // it went; nothing else here waits on either.
                Effect::Spawn { argv, note } => {
                    say(&mut ui, note);
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        let line = match run_wsp(&argv) {
                            Ok(_) => match argv.iter().any(|a| a == "--agent") {
                                true => "agent working".to_string(),
                                false => "workspace open".to_string(),
                            },
                            Err(e) => e,
                        };
                        let _ = tx.send(Msg::Note(line));
                    });
                }
                Effect::Tell(t) => tell(t, &mut ui, tx),
                Effect::Run { argv, escalate, then } => {
                    match (run_wsp(&argv), escalate) {
                        (Ok(m), _) => {
                            // Land the cursor on whatever was just created, so
                            // `E` is the next key rather than a hunt. Quick
                            // capture stays one line; writing it up is a
                            // keystroke away rather than a mode you were forced
                            // through.
                            let created = creates(&argv).then(|| m.id.clone()).flatten();
                            match &created {
                                Some(id) => {
                                    view.land_on = Some(id.clone());
                                    say(&mut ui, format!("{id} added · E to write it up"));
                                    // …and take the keyboard, because that
                                    // sentence names a key. herdr's focus moves
                                    // on its own — a `workspace.focused` for
                                    // every sidebar hover, an agent coming back
                                    // in another workspace — so the pane that
                                    // ran this is not reliably the pane the
                                    // next keystroke reaches, and the `E` the
                                    // footer just asked for lands in whatever
                                    // terminal does hold it. Which is somebody
                                    // else's agent, taking `E` as a prompt.
                                    //
                                    // The same rule as the click: acting here
                                    // is a statement about where you are
                                    // working. `took_focus` moves with it so a
                                    // click landing straight after does not ask
                                    // herdr twice.
                                    took_focus = Instant::now();
                                    focus_self(me.as_deref(), self_ws);
                                    keyboard = true;
                                }
                                None => say(&mut ui, m.label),
                            }
                            // Only now the command has actually worked. The
                            // footer takes the sentence's line over the
                            // command's: what you want to know about a claim
                            // is whether the agent was told.
                            if let Some(t) = then {
                                tell(t, &mut ui, tx);
                            }
                        }
                        // Refused, and there is a stronger form of the same
                        // command: show what the CLI said and ask again.
                        (Err(e), Some(more)) => {
                            view.mode = Mode::Confirm {
                                question: e,
                                argv: more,
                                escalate: None,
                                then,
                            };
                        }
                        (Err(e), None) => say(&mut ui, e),
                    }
                    refetch = true;
                }
            },
            Msg::Herdr(e) => {
                // Coalesce the burst that follows a split or a focus change.
                // Keys are put back rather than dropped: they arrived after the
                // event but have not been acted on, and swallowing a keystroke
                // because a pane somewhere repainted is not a trade anyone
                // would make. `pane.updated` alone is two events a second, so
                // this drain runs constantly and used to eat into every burst
                // of typing.
                let ours = |e: &HerdrEvent| e.workspace.is_some() && e.workspace.as_deref() == self_ws;
                let mut shape = e.shape;
                let mut focused_us = e.focus && ours(&e);
                while let Ok(m) = rx.try_recv() {
                    match m {
                        Msg::Herdr(e) => {
                            shape |= e.shape;
                            focused_us |= e.focus && ours(&e);
                        }
                        Msg::Tick => {}
                        held => carry.push_back(held),
                    }
                }
                dirty = true;
                // A pane appeared, went, or picked up an agent. Draw it now,
                // and not only when this panel happens to be the one being
                // looked at: an agent showing up in the dock half a minute
                // after it started is the whole complaint this came from.
                //
                // Focus arriving on our own workspace is the other one that
                // cannot wait, for a different reason. It is what moves this
                // panel onto the fast cadence, and the cadence is read from a
                // snapshot — so deferring it would leave the panel deciding it
                // is slow *because* the last snapshot said it was not being
                // looked at, and never taking the one that would say otherwise.
                //
                // It is also what keeps [`holds_keyboard`] honest, and this is
                // the event that can do it: measured against the live server,
                // herdr raises `workspace.focused` on every move of the keyboard
                // — including one pane to the next inside a single workspace,
                // where the workspace being focused has not changed at all. So
                // the reader stepping off this pane onto the shell beside it
                // lands here, and the census that says who has the keyboard is
                // re-read before the next click can ask.
                if shape || focused_us {
                    refetch = true;
                    dirty = false;
                }
            }
            // A spawn has landed. Refetch as well as say so: the workspace and
            // its agent are new rows, and the claim moved a task under them.
            Msg::Note(line) => {
                say(&mut ui, line);
                refetch = true;
            }
            Msg::Tick => {
                // Ask the one question the event stream cannot answer, and only
                // where it goes unanswered: an unfocused panel, between its full
                // refetches. A status that has moved is news of exactly the kind
                // [`SHAPE`] exists for, so it refetches now rather than waiting
                // out the rest of the thirty seconds.
                //
                // An empty list is herdr not answering, not everybody finishing
                // at once — `panes()` degrades to empty the same way
                // [`Snapshot::live`] does, and reading that as news would clear
                // the dock every time the socket hiccuped.
                if !ui.self_focused && last_poll.elapsed() >= STATUS_POLL {
                    last_poll = Instant::now();
                    let now = herdr::panes().unwrap_or_default();
                    if !now.is_empty() && status_moved(&drawn_status, &now) {
                        refetch = true;
                        dirty = false;
                    }
                }
                // The one you are looking at should feel immediate; the twenty
                // behind it should cost nothing. Both the fingerprint stat and
                // the two socket calls sit behind this gate, so an idle
                // background panel does no work at all between refreshes.
                let interval = if ui.self_focused {
                    Duration::from_millis(250)
                } else {
                    Duration::from_secs(30)
                };
                if last_fetch.elapsed() >= interval {
                    if started_as.is_some() && exe_stamp() != started_as {
                        return Outcome::Reload;
                    }
                    let store_changed = store.fingerprint() != last_fingerprint;
                    if dirty || store_changed {
                        refetch = true;
                        dirty = false;
                    }
                }
            }
        }

        if refetch {
            last_fetch = Instant::now();
            last_fingerprint = store.fingerprint();
            // Adopt before rebuilding, because the folds and the filters decide
            // which rows there are to rebuild. A panel that has just been
            // switched to refetches on the `workspace.focused` that named it,
            // so this is the moment it catches up — no polling of its own.
            //
            // The panel being driven reads back what it just wrote, which is
            // its own state and therefore a no-op. That is what makes "always
            // adopt" safe: there is one keyboard, so there is one writer.
            let taken = adopt(store, &mut view, &mut agreed);
            if taken.target != Target::Nothing {
                want = taken;
            }
            let snap = Snapshot::live(store);
            // Free: the panes were fetched for the frame either way, and this is
            // the same list herdr answers "who has the keyboard" out of.
            keyboard = holds_keyboard(&snap, me.as_deref());
            // Likewise free, and it has to be taken from the same list the frame
            // is drawn from: the poll asks whether herdr has moved on from what
            // the reader is looking at, so what the reader is looking at is what
            // it must be compared against.
            drawn_status.clear();
            drawn_status
                .extend(snap.panes.iter().map(|p| (p.pane_id.clone(), p.agent_status.clone())));
            refetch_into(&mut ui, &snap, &mut view, self_ws);
            if point_at(&mut ui, &want) {
                want = Cursor::default();
            }
        }

        // Only input can change the durable half, so only input is worth the
        // comparison. A tick that serialised this five times a second in every
        // pane on the machine would be pure heat. The mouse shares from its own
        // branches above, which return before they reach here.
        if is_key {
            shared::share(store, &view, ui.cursor(), &mut agreed);
        }
        draw(&ui, &mut view, &mut last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this came from. A subscription list is validated as a whole, so
    /// one per-pane type sent without a `pane_id` refuses every global type
    /// beside it — and `subscribe` used to read that refusal as an empty
    /// stream. The panel drew a tree that could not change.
    #[test]
    fn no_subscription_is_scoped_to_a_single_pane() {
        for t in SHAPE.iter().chain(CHATTY) {
            assert!(
                !PER_PANE.contains(t),
                "{t} needs a pane_id, and asking for it globally refuses the whole list",
            );
        }
    }

    /// Which pane has the keyboard decides what a click means, and the panel
    /// learns that a pane at a time from the census. What makes the census
    /// timely enough to decide a click is this subscription: measured against
    /// the live server, herdr raises `workspace.focused` on every move of the
    /// keyboard, including one pane to the next inside a single workspace, so
    /// the move that matters most — off this panel onto the shell beside it —
    /// is one the loop is told about rather than one it waits out.
    #[test]
    fn the_keyboard_moving_is_something_the_panel_is_told() {
        assert!(CHATTY.contains(&"workspace.focused"));
        assert!(is(CHATTY, "workspace_focused"), "and by the name the stream uses");
    }

    /// One pane in the census has the keyboard, and a click means one thing in
    /// that pane and another in every other. This is where the loop starts from
    /// and where it recovers to when the event stream has been away.
    #[test]
    fn the_keyboard_is_where_the_census_says_it_is() {
        let census = |panes: &[(&str, bool)]| Snapshot {
            projects: Vec::new(),
            tasks: Vec::new(),
            bindings: Default::default(),
            pins: Default::default(),
            mandates: Default::default(),
            claims: Default::default(),
            workspaces: Vec::new(),
            panes: panes
                .iter()
                .map(|(id, has)| herdr::Pane {
                    pane_id: (*id).to_string(),
                    focused: *has,
                    ..Default::default()
                })
                .collect(),
        };

        let snap = census(&[("w1:p1", false), ("w0:p3", true)]);
        assert!(!holds_keyboard(&snap, Some("w1:p1")));
        assert!(holds_keyboard(&snap, Some("w0:p3")));
        // A panel that cannot name itself is one running outside herdr, where
        // there is no focus to lose. It acts on what it is told rather than
        // swallowing every click it is ever sent.
        assert!(holds_keyboard(&snap, None));
        assert!(holds_keyboard(&snap, Some("w9:p9")), "and a pane herdr has never heard of");
    }

    /// A pane gaining an agent is the event the dock exists for, and herdr
    /// raises the same one when the agent goes away. It has to be urgent
    /// rather than chatty: measured against the live server, herdr knew about
    /// a new claude 2.4s after it launched and the panel drew it at 31.2s,
    /// because it was waiting for the background tick.
    #[test]
    fn a_pane_gaining_or_losing_an_agent_is_urgent() {
        assert!(SHAPE.contains(&"pane.agent_detected"));
        assert!(!CHATTY.contains(&"pane.agent_detected"));
        // The pane going entirely is the other half of the same question.
        assert!(SHAPE.contains(&"pane.closed"));
        assert!(SHAPE.contains(&"pane.exited"));
        assert!(SHAPE.contains(&"workspace.closed"));
        // And the one that cannot be had globally is nowhere near either list.
        assert!(!SHAPE.contains(&"pane.agent_status_changed"));
        assert!(!CHATTY.contains(&"pane.agent_status_changed"));
    }

    /// herdr sends `workspace.focused` on every sidebar hover and
    /// `pane.updated` about twice a second per working agent. Treating either
    /// as urgent would have every panel on the machine doing socket
    /// round-trips continuously.
    #[test]
    fn the_noisy_ones_are_not_urgent() {
        for t in ["workspace.focused", "pane.updated"] {
            assert!(CHATTY.contains(&t), "{t} should be tick-gated");
            assert!(!SHAPE.contains(&t), "{t} is far too noisy to refetch on");
        }
    }

    /// The whole of the asymmetry, in one function. An agent stopping has no
    /// event of its own that reaches every panel, so it arrives inside the
    /// noisiest one there is — and the only thing separating it from a spinner
    /// frame is what the pane's status was a moment ago.
    ///
    /// Measured against the live server with four agents running: 120s of
    /// `pane.updated` is 335 events carrying 18 status transitions. Promoting
    /// eighteen a couple of minutes is the handful a minute `SHAPE` is costed
    /// for; promoting all 335 is the continuous socket traffic across every
    /// panel on the machine that `CHATTY` exists to avoid.
    #[test]
    fn only_a_status_that_actually_changed_is_worth_a_refetch() {
        let pane = |id: &str, status: &str| {
            json!({ "pane": { "pane_id": id, "agent_status": status } })
        };
        let mut seen = HashMap::new();

        // Meeting a pane teaches without firing: the first second of a
        // subscription is every pane on the machine arriving at once.
        assert!(!status_changed(&mut seen, &pane("w1:p6", "working")));
        // And the spinner frames behind it say nothing new.
        for _ in 0..10 {
            assert!(!status_changed(&mut seen, &pane("w1:p6", "working")));
        }

        // Stopping is the one that must not wait.
        assert!(status_changed(&mut seen, &pane("w1:p6", "idle")));
        assert!(!status_changed(&mut seen, &pane("w1:p6", "idle")), "and only once");
        // Starting again is the same fact from the other side: an arrow that
        // says an agent wants you must come down as promptly as it went up.
        assert!(status_changed(&mut seen, &pane("w1:p6", "working")));

        // Panes are tracked apart. One agent working through another's stop
        // used to be the same event as far as the reducer could tell.
        assert!(!status_changed(&mut seen, &pane("w0:p3", "idle")));
        assert!(status_changed(&mut seen, &pane("w0:p3", "working")));
        assert!(!status_changed(&mut seen, &pane("w1:p6", "working")));

        // A pane with no agent in it has no status to change. herdr sends the
        // field empty rather than leaving it out, and an empty string is not a
        // transition into anything.
        assert!(!status_changed(&mut seen, &pane("w2:p1", "")));
        assert!(!status_changed(&mut seen, &pane("w2:p1", "")));
        // Nor is an event that carries no pane at all — `workspace.focused`
        // goes through the same reducer.
        assert!(!status_changed(&mut seen, &json!({ "workspace_id": "w1" })));
    }

    /// The other half of the same asymmetry, and the half that has no event at
    /// all behind it.
    ///
    /// herdr is never told an agent's status — it classifies from the
    /// transcript the hook reports — so a finish is the *absence* of writes to
    /// a file, and nothing fires on the passing of time. Measured against the
    /// live server, the `pane.updated` carrying a finish arrived 6.8s late
    /// once and 28.9s late once, with 147s of silence on the stream across the
    /// second. `pane.list` reads the same rule when asked and so has it at
    /// once. That is what this poll is for; see [`STATUS_POLL`].
    #[test]
    fn a_finish_that_never_reached_the_stream_is_still_found_by_asking() {
        let census = |panes: &[(&str, &str)]| {
            panes
                .iter()
                .map(|(id, status)| herdr::Pane {
                    pane_id: (*id).to_string(),
                    agent_status: (*status).to_string(),
                    ..Default::default()
                })
                .collect::<Vec<_>>()
        };
        let drawn: HashMap<String, String> = [("w0:p3", "working"), ("w1:p6", "idle")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();

        // The frame still says what herdr says. Nothing to do, and this is the
        // answer on all but a few polls a minute.
        assert!(!status_moved(&drawn, &census(&[("w0:p3", "working"), ("w1:p6", "idle")])));

        // The one the event stream lost: an agent that stopped while the panel
        // was not being looked at.
        assert!(status_moved(&drawn, &census(&[("w0:p3", "done"), ("w1:p6", "idle")])));
        // And the same fact from the other side.
        assert!(status_moved(&drawn, &census(&[("w0:p3", "working"), ("w1:p6", "working")])));

        // A pane the frame has never drawn is structural, and `SHAPE` has
        // already refetched on it. Counting it here refetches twice for one
        // event.
        assert!(!status_moved(&drawn, &census(&[("w2:p1", "working")])));
        // A pane that has gone likewise: it is absent from the census, which
        // is not a status that moved.
        assert!(!status_moved(&drawn, &census(&[("w0:p3", "working")])));
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wsp-{}-{}", tag, std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// The rule the ordering in the loop depends on. A panel writes the shared
    /// view *after* it has rebuilt, so at the moment it rebuilds the file still
    /// says where the cursor was before the key. Adopting there would drag the
    /// cursor back off whatever was just created — `land_on` exists precisely to
    /// put it there — so a panel only takes the file when someone else moved it.
    #[test]
    fn a_panel_does_not_adopt_its_own_state_back() {
        let dir = scratch("own-state");
        let store = Store::at(dir.clone(), dir.clone());

        let mut mine = View::default();
        mine.collapsed.insert("audio".into());
        let text = shared::rendered(&shared::Shared::of(&mine, Target::Task("t-1".into())));
        shared::write(&store, &text);

        let mut agreed = text.clone();
        assert_eq!(
            adopt(&store, &mut mine, &mut agreed),
            Cursor::default(),
            "a panel that already agrees has nothing to take",
        );

        let mut theirs = View::default();
        let mut unseen = String::new();
        assert_eq!(
            adopt(&store, &mut theirs, &mut unseen),
            Cursor::from(Target::Task("t-1".into())),
            "a panel that has not seen it takes it, cursor and all",
        );
        assert!(theirs.collapsed.contains("audio"));
        assert_eq!(unseen, text, "and now agrees, so it will not take it twice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// First run. No file is the ordinary state of a machine that has never
    /// opened the panel, and it must not blank the view it was given.
    #[test]
    fn nothing_shared_yet_leaves_the_view_alone() {
        let dir = scratch("first-run");
        let store = Store::at(dir.clone(), dir.clone());
        let mut view = View::default();
        view.show_done = true;
        let mut agreed = String::new();
        assert_eq!(adopt(&store, &mut view, &mut agreed), Cursor::default());
        assert!(view.show_done, "a missing file is not an instruction to reset");
        assert!(agreed.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Subscriptions are spelt with dots and the stream answers in underscores.
    /// Classifying on the subscription's own spelling would make every event
    /// non-urgent, which is the same silence by another route.
    #[test]
    fn events_are_matched_by_the_names_the_stream_actually_uses() {
        assert!(is(SHAPE, "pane_agent_detected"));
        assert!(is(SHAPE, "pane_created"));
        assert!(is(CHATTY, "workspace_focused"));
        assert!(!is(SHAPE, "pane_updated"));
        assert!(!is(SHAPE, "pane.agent_detected"));
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    /// The footer sentence names a key and the panel takes the keyboard to
    /// make sure it lands, so getting this wrong is not cosmetic: it moves the
    /// cursor off what you were looking at and pulls focus out of whatever
    /// pane had it. `project` as a whole word was the test until `project set`
    /// existed, and `project rm` was already wrong under it — it answers with
    /// the id it removed, so `X` on a project announced the removal as an
    /// addition and sent the cursor after a row that had just gone.
    #[test]
    fn only_the_verbs_that_make_something_move_the_cursor_to_it() {
        assert!(creates(&argv(&["add", "Retune the early reflections", "-p", "verb"])));
        assert!(creates(&argv(&["project", "add", "verb"])));

        assert!(!creates(&argv(&["project", "set", "wsp", "name=wsp control plane"])));
        assert!(!creates(&argv(&["project", "set", "verb", "parent=tooling"])));
        assert!(!creates(&argv(&["project", "rm", "verb"])));
        // And the rest, which never carried an id anyway and must not start.
        assert!(!creates(&argv(&["rename", "t-001", "Retune the early reflections"])));
        assert!(!creates(&argv(&["tag", "t-001", "--", "+dsp"])));
        assert!(!creates(&argv(&["pin", "trance", "-w", "w0"])));
        assert!(!creates(&argv(&[])));
    }
}
