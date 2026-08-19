//! The pane: the terminal it runs in, and the loop that keeps it honest.
//!
//! Everything impure lives here. Raw mode, the input thread, herdr's event
//! stream, the tick, and the loop that turns an [`Effect`] into something that
//! actually happens.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::herdr;
use crate::input::Key;
use crate::kanban;
use crate::live::{self, AgentRef};
use crate::store::Store;
use crate::util::exe_stamp;

use super::keys::{apply_input, say, Effect, Input, Mode, View};
use super::render::{frame, to_ansi};
use super::rows::{collect, refetch_into, Cursor, Snapshot, Target, Ui};
use super::shared;
use super::verbs::{
    close_view, expand, inspect, open_board, open_full, pop_out, run_wsp, send_tell, Tell,
};

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

/// How wide and how tall the pane is, asked of the terminal itself.
///
/// This ran `stty size` in a child process, and the `draw` below calls it on
/// every pass of the loop — five forks a second in every panel, for ever,
/// whether or not anything on screen had changed. Measured 2026-08-17 against
/// the live machine: a *background* panel burned 12 ms of CPU a second doing
/// nothing, 59% of its on-CPU samples in this one call site, plus ~2.4 ms a
/// call in the `stty` child, which no panel's `ps` time shows at all.
///
/// `TIOCGWINSZ` is where `stty` reads it from, so this is the same answer with
/// no process in it — checked against `stty size` on a pty sized 137×41, and
/// timed beside it in one process: microseconds where the fork was
/// milliseconds. Declared here rather than taken from
/// `libc` for the reason `die_on_broken_pipe` in `main` gives: a struct and a
/// constant against a dependency the README promises not to add. Two things in
/// the declaration are load-bearing. `ioctl` must be variadic as C declares it,
/// because on Apple silicon a variadic argument is passed on the stack and a
/// fixed one in a register. And the request number is per-platform — the BSDs
/// encode direction and struct size into it, Linux numbers its ioctls flat.
///
/// The fd is opened once and kept, because with the process gone the open is
/// what the call costs: opening and closing `/dev/tty` measured 50× the ioctl
/// through it. A panel already holds one open for its whole life — see
/// [`spawn_input`] — and a binary that never draws never opens it at all. A
/// held fd still sees a resize: the size is the terminal's, not the fd's.
pub(crate) fn term_size() -> (usize, usize) {
    #[repr(C)]
    struct Winsize {
        rows: u16,
        cols: u16,
        xpixel: u16,
        ypixel: u16,
    }
    extern "C" {
        fn ioctl(fd: i32, request: std::os::raw::c_ulong, ...) -> i32;
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const TIOCGWINSZ: std::os::raw::c_ulong = 0x5413;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const TIOCGWINSZ: std::os::raw::c_ulong = 0x4008_7468;

    static TTY: OnceLock<Option<File>> = OnceLock::new();
    if let Some(tty) = TTY.get_or_init(|| File::open("/dev/tty").ok()) {
        let mut ws = Winsize { rows: 0, cols: 0, xpixel: 0, ypixel: 0 };
        // The kernel writes the four shorts it is defined over into `ws`, which
        // outlives the call; nothing else is read or written through the fd.
        let asked = unsafe { ioctl(tty.as_raw_fd(), TIOCGWINSZ, &mut ws as *mut Winsize) };
        // A terminal that does not know its size answers zero rather than
        // failing — a pty nobody has sized yet is the usual way to see it — so
        // that is as much a miss as an error, and takes the same fallback.
        if asked == 0 && ws.cols > 0 && ws.rows > 0 {
            return ((ws.cols as usize).max(16), (ws.rows as usize).max(6));
        }
    }
    (26, 40)
}

/// Where the panel is drawn, and what herdr can tell us about it.
///
/// The panel used to *be* a pane: it measured `/dev/tty`, painted escapes onto
/// it, and asked herdr which pane had the keyboard. herdr's sidebar is the
/// first place it is drawn that is none of those things — no tty, no pane id,
/// no workspace — and this is the whole of the difference, held in three
/// methods so that [`event_loop`] does not know which one it is driving.
///
/// It is not a second renderer. `frame` still builds the same [`Line`]s, and a
/// screen only decides how many cells there are and where the rows go.
///
/// [`Line`]: super::render::Line
pub(super) trait Screen {
    /// Columns and rows to build the next frame for.
    fn size(&self) -> (usize, usize);

    /// Put a frame on it. Answers the size it drew at, which is the size the
    /// tick decides the panel's *shape* from — a second measurement between the
    /// frame and that decision can disagree with the frame, and mid-drag it
    /// will.
    fn paint(&mut self, lines: &[super::render::Line], w: usize, h: usize) -> (usize, usize);

    /// The two focus questions, asked together because they are one picture:
    /// whether the keyboard is here, which decides what a click means, and
    /// whether this is on screen, which decides the cadence.
    ///
    /// Answered from herdr's census for a pane, because a pane has to ask. A
    /// surface drawn as chrome does not: it is always on screen, and whether it
    /// has the keyboard is something its host says outright.
    fn focus(&self, live: &live::Live, me: Option<&str>, ws: Option<&str>) -> (bool, bool) {
        (live.has_keyboard(me), live.on_screen(ws))
    }

    /// What herdr is running, for the frame about to be built.
    ///
    /// A pane asks, because a pane is a tenant of herdr and has no other way to
    /// know: two socket round-trips, every time the loop decides the picture
    /// might have moved. A surface does not have to. Its host **is** herdr, and
    /// herdr knows the instant an agent changes what it is doing, because it is
    /// the thing that noticed — so it pushes the answer down the pipe the
    /// surface is already on and this returns what it was told. See
    /// `super::surface`.
    ///
    /// The seam is here, at the point of use, rather than at a flag the loop
    /// reads: the loop should not know which of the two it is driving, which is
    /// the whole reason this trait exists.
    fn live(&self) -> live::Live {
        live::read()
    }

    /// Ask the host to draw this panel `cols` wide, and say whether there was
    /// anybody to ask. `0` gives the room back.
    ///
    /// The one thing a panel can ask its host for, and deliberately the only
    /// one. A panel has three reasons to want more room than a sidebar has —
    /// the whole tree, a task written up, a board — and if each of them were a
    /// verb on the wire, each would be a herdr release standing between wsp and
    /// a change to its own surface. So the host is told a number of columns and
    /// never what is going in them, and the next caller is one line here rather
    /// than a message on both sides.
    ///
    /// **Nothing is promised.** The host owns the rect, gives what it has, and
    /// reports what it gave as the size of the next frame — which it already
    /// does on every resize, so there is no reply to wait for and no second
    /// state to keep in step. A caller draws whatever it is given: the rows
    /// already decide what they show from the width they are built at, which is
    /// why asking is the whole of the change.
    ///
    /// `false` is a host that cannot be asked — a tty panel, which is its
    /// pane's size and has nothing to negotiate with, and any host too old to
    /// have said it takes widths. A caller that gets it falls back to whatever
    /// it did before, which is what keeps this shippable against a herdr that
    /// has not been rebuilt: see [`super::verbs::open_full`].
    fn ask_width(&mut self, _cols: usize) -> bool {
        false
    }
}

/// The panel in a herdr pane: a tty it measures, and escapes painted onto it.
///
/// `last` is the frame already on screen. A panel redraws on every tick and
/// most ticks change nothing, so the comparison is what keeps a quiet panel
/// from writing its whole frame to a pty five times a second.
pub(super) struct Tty {
    last: String,
}

impl Tty {
    pub(super) fn new() -> Tty {
        Tty { last: String::new() }
    }
}

impl Screen for Tty {
    fn size(&self) -> (usize, usize) {
        term_size()
    }

    fn paint(&mut self, lines: &[super::render::Line], w: usize, h: usize) -> (usize, usize) {
        let painted = to_ansi(lines, w, h);
        if painted != self.last {
            print!("{painted}");
            let _ = std::io::stdout().flush();
            self.last = painted;
        }
        (w, h)
    }
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

/// Subscribe to herdr's event stream, and mark the panel dirty on what comes
/// out of it.
///
/// This is how a panel in a *pane* learns that something moved: it is a tenant
/// of herdr with no other way to know. A surface has one — its host is herdr,
/// and pushes the answer outright — so it starts this only for a host that
/// turns out not to. See [`super::surface`].
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

/// The pane a panel *is*, and the workspace it is in.
///
/// A panel in a pane has both, and reads them where every wsp process reads
/// them: the environment herdr sets on the pane's shell. A surface has
/// neither — it is a child of herdr drawing chrome that belongs to no
/// workspace — and **it has to say so rather than find out**, which is the
/// only reason this is a type and not two arguments read from the environment
/// at the top of the loop.
///
/// The failure it is against is specific and is the way this fork is built:
/// herdr passes its own environment to the surface it spawns, and a herdr
/// started from a terminal inside another herdr — which is how the fork is
/// developed, and how it is installed — hands its sidebar a `HERDR_PANE_ID`
/// and a `HERDR_WORKSPACE_ID` belonging to whichever pane it was launched
/// from. A surface that read those would split its detail pane into another
/// workspace, take that pane's focus on every key that creates something, and
/// close a view somebody else was reading.
#[derive(Default)]
pub(super) struct Where {
    workspace: Option<String>,
    pane: Option<String>,
}

impl Where {
    /// A panel in a pane: it is where the environment says it is.
    pub(super) fn env() -> Where {
        let env = herdr::Env::read();
        Where { workspace: env.workspace_id, pane: env.pane_id }
    }

    /// A panel that is in no pane and no workspace, and knows it.
    pub(super) fn nowhere() -> Where {
        Where::default()
    }

    fn ws(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    fn pane(&self) -> Option<&str> {
        self.pane.as_deref()
    }
}

/// Why the loop stopped.
pub(super) enum Outcome {
    Quit,
    /// The binary changed on disk. Twenty-two panes each holding a stale image
    /// is a real cost while this is under active development — a key silently
    /// doing what it used to do is worse than one that errors.
    Reload,
}

/// `full` is the panel `Z` opens in a tab of its own: the same panel, drawn at
/// the width of the workspace, and quit rather than kept when you have finished
/// with it. See [`super::keys::View::full`].
pub fn run(store: &Store, full: bool) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let here = Where::env();

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

    let outcome = event_loop(store, &tx, &rx, &here, full, &mut Tty::new());

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
            let mut c = Command::new(exe);
            c.arg("panel");
            // The fullscreen panel comes back as itself. Without this a reload
            // would land a sidebar in a tab of its own, with no `q` in it.
            if full {
                c.arg("--full");
            }
            let err = c.exec();
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

/// What the panel is drawing in the room it asked for.
///
/// The axis `fork-011` said the next page would pay for again, and this is it
/// paying: `expand` generalised for free — a third caller is the same call with
/// a third number — and *what is drawn in the room* did not, because that never
/// crosses the wire and is this process's alone. Two variants is the whole of
/// the cost, and it is the right cost: herdr is told a number of columns and
/// learns nothing about which of these is in them.
///
/// One at a time, deliberately. There is one pane, and a page replaces the tree
/// rather than stacking on it — so `↵` from a board is this enum changing
/// variant, not a second page opening over the first.
enum Page {
    /// The project by state, at whatever width the host will give — `K`.
    Board(BoardPage),
    /// A task or a project written up, at a page's width — `↵`.
    Task(TaskPage),
}

/// A task or a project in full, drawn where the tree was.
///
/// **The last pane the panel rented for itself.** `wsp-080` gave back every
/// other one; `↵` kept splitting a pane to put the detail in, and from a
/// surface that pane is not even the panel's own — `view_target` falls through
/// to "the widest pane that is not ours", which is the pane somebody is working
/// in. So `↵` has been taking 45% of the reader's terminal to show them
/// something the panel could draw itself. Now it draws it.
///
/// `E` is untouched and stays a tab. The rule underneath both is `pop_out`'s
/// own: the panel stops renting screen for *itself*, and goes on opening the
/// full-size things a person asked for. A task read is the panel showing you
/// something; `$EDITOR` on it is a process in a pty, which no width makes
/// drawable here.
struct TaskPage {
    /// What is open. Mirrored on [`View::showing`], which is what `↵` on the
    /// same row reads to know it means close — the same field, and the same
    /// meaning, as when the detail was a pane.
    focus: crate::detail::Focus,
    /// Everything the frame reads, rebuilt on the refetch the tree uses. Held
    /// rather than gathered per frame because a frame is drawn on every key and
    /// this is six reads of the store.
    ctx: crate::detail::Ctx,
}

impl TaskPage {
    fn open(store: &Store, focus: crate::detail::Focus, panes: Vec<AgentRef>) -> TaskPage {
        TaskPage { focus, ctx: crate::detail::Ctx::page(store, panes) }
    }
}

/// The board, drawn in the room the host gave, in place of the tree.
///
/// The third caller of [`expand`] and the one that shows what the seam is for.
/// `Z` asks for [`super::render::PAGE_MIN`] and the *same* rows get more room;
/// this asks for [`super::render::WHOLE_SCREEN`] and draws something else
/// entirely — and the difference between those two costs herdr nothing at all,
/// because the wire carries a number of columns and never a reason for wanting
/// them.
///
/// It sits here, in the loop, rather than in [`View`]. A [`kanban::Board`] is
/// the store joined with the census — the same thing [`Ui`] is, and it is
/// rebuilt on the same refetch — where `View` is the small durable half a
/// second panel adopts. Two panels are looking at the same folds; they are not
/// looking at the same board, any more than they share a cursor.
///
/// Everything on it is either the store's or this pane's, so there is nothing
/// to lose when it closes and nothing to come back to when it opens. That is
/// the same argument [`kanban`] makes for its own pane, and it is why `q` here
/// gives the room back rather than putting a lid on something.
struct BoardPage {
    /// What the board is a board of, kept because a rebuild has to ask the
    /// same question the first build asked.
    scope: kanban::Scope,
    /// Its own copy, opened from [`View::show_done`] and free to differ after
    /// that. Inherited because arriving at a board with a `done` column the
    /// tree was hiding — or without one it was showing — reads as the key
    /// having changed a setting; kept separate because after that they are two
    /// questions, and `A` on the board is a column appearing rather than rows.
    show_done: bool,
    cur: kanban::Cursor,
    board: kanban::Board,
}

impl BoardPage {
    fn open(
        store: &Store,
        scope: kanban::Scope,
        show_done: bool,
        panes: Vec<AgentRef>,
    ) -> BoardPage {
        let board = kanban::collect(&kanban::Ctx::of(store, panes), &scope, show_done);
        BoardPage { scope, show_done, cur: kanban::Cursor::default(), board }
    }

    /// Rebuild from the store, and put the cursor back on the card it was on.
    ///
    /// `follow` is where a command has just moved a card to, named while the id
    /// was still in hand: after the rebuild the slot it sat in belongs to
    /// whatever moved up into it, so the cursor would otherwise be left
    /// pointing at a neighbour.
    fn rebuild(&mut self, store: &Store, follow: Option<String>, panes: Vec<AgentRef>) {
        let keep = follow.or_else(|| self.board.card_at(&self.cur).map(|c| c.id.clone()));
        self.board =
            kanban::collect(&kanban::Ctx::of(store, panes), &self.scope, self.show_done);
        self.cur =
            keep.and_then(|id| self.board.find(&id)).unwrap_or_else(|| self.cur.clamped(&self.board));
    }
}

impl Page {
    /// The frame this page draws, at the size the host actually gave.
    ///
    /// `note` is the panel's own footer message, on whichever footer this page
    /// has: one message and one clock across all of them, so a sentence written
    /// by a key on the tree does not vanish the moment a page takes the pane.
    ///
    /// The board takes it as an argument because it has always had a footer of
    /// its own. The task page does not, so the note is written over its last
    /// line here — which is the same trade the tree and the board both make,
    /// spending the key hint on the sentence for four seconds. Without it `E`
    /// on a page would be the one key in the panel that can fail in silence:
    /// [`pop_out`] answers in words, and there would be nowhere to put them.
    fn frame(&self, note: &str, w: usize, h: usize) -> Vec<super::render::Line> {
        match self {
            Page::Board(p) => kanban::frame(&p.board, &p.cur, w, h, note),
            Page::Task(p) => {
                let mut out =
                    crate::detail::frame(&p.ctx, &p.focus, w, h, crate::detail::Placed::Page);
                if !note.is_empty() {
                    if let Some(last) = out.last_mut() {
                        *last = super::render::line(
                            super::render::Style::Accent,
                            crate::util::truncate(note, w),
                        );
                    }
                }
                out
            }
        }
    }

    /// Read the store again, because something changed. See the refetch below.
    ///
    /// `panes` is the census the loop has already fetched for the tree. Handed
    /// down rather than fetched again: a page repaints on the same cadence the
    /// panel does, and a socket call per repaint is the clock `fork-001`
    /// removed. The store read is not saved and is the honest remaining cost —
    /// a second pass over files the snapshot beside it has just read, for as
    /// long as a page is open.
    fn rebuild(&mut self, store: &Store, follow: Option<String>, panes: Vec<AgentRef>) {
        match self {
            Page::Board(p) => p.rebuild(store, follow, panes),
            Page::Task(p) => p.ctx = crate::detail::Ctx::page(store, panes),
        }
    }

    /// The width to ask the host for. Different per page and never explained to
    /// the host — see [`super::verbs::expand`].
    ///
    /// A board absorbs every column it is given, so it asks past the end. A
    /// task written up does not: it is prose, laid out to a measure, and the
    /// fields and sub-task rows around it are short. [`super::render::PAGE_MIN`]
    /// is the same number `Z` asks for and the same reasoning — the width at
    /// which this stops being a thing you glance at.
    fn width(&self) -> usize {
        match self {
            Page::Board(_) => super::render::WHOLE_SCREEN,
            Page::Task(_) => super::render::PAGE_MIN,
        }
    }
}

/// What a key pressed on a page leaves for the loop to do.
///
/// The board's own [`kanban::Action`] speaks of cards and columns; this speaks
/// of the pane. Two enums rather than one because the board is drawn in two
/// places — its own tab and this page — and only one of them has a room to
/// give back.
enum FromPage {
    /// Nothing the loop has to do; the cursor moved, or the footer was written.
    Nothing,
    /// Rebuild from the store, putting the cursor on this card if it is named.
    /// The card is the board's; a task page has nothing to land on.
    Refetch(Option<String>),
    /// The page is finished with. Give the room back and draw the tree.
    Close,
    /// Finished with, and this is what to draw instead — `↵` on a card. The
    /// room is not given back and taken again: it is one ask at the new page's
    /// width, so the sidebar never blinks through its own width in between.
    Show(crate::detail::Focus),
}

/// A key, while the board has the pane.
///
/// Every verb here is one the tree's own keys already reach, through the same
/// functions: the CLI does the work, the detail pane is the panel's, and an
/// editor is a tab. What the board changes is which card they are aimed at, and
/// that is the whole of it — see [`kanban::apply_key`], which decides that part
/// and is shared with `wsp kanban`.
fn board_key(
    k: Key,
    p: &mut BoardPage,
    ui: &mut Ui,
    self_ws: Option<&str>,
) -> FromPage {
    match kanban::apply_key(k, &p.board, &mut p.cur) {
        kanban::Action::None => FromPage::Nothing,
        kanban::Action::Say(m) => {
            say(ui, m);
            FromPage::Nothing
        }
        kanban::Action::Refetch => FromPage::Refetch(None),
        kanban::Action::ShowDone => {
            p.show_done = !p.show_done;
            say(ui, if p.show_done { "showing done" } else { "hiding done" });
            FromPage::Refetch(None)
        }
        // The CLI, exactly as the tree's keys run it: the event log, the hooks
        // and the commit all happen because it is the same path a person at a
        // shell would take. The card is about to be in another column and the
        // cursor goes with it, so its id is named while it is still in hand.
        kanban::Action::Run { argv, task } => {
            match run_wsp(&argv) {
                Ok(m) => say(ui, m.label),
                Err(e) => say(ui, e),
            }
            FromPage::Refetch(Some(task))
        }
        // An editor is a tab here too, and for the reason it is one everywhere
        // else: `wsp edit` runs `$EDITOR`, which is a process in a pty, and the
        // room this page is drawn in is cells this panel paints. See
        // [`pop_out`].
        kanban::Action::Edit { id } => {
            say(ui, pop_out(&["edit".to_string(), id.clone()], &id, self_ws));
            FromPage::Nothing
        }
        // `↵` means what it means on the tree: the task written up, in place.
        // The board stands down on the way, because it holds nothing of its own
        // and leaving it behind the thing you opened from it would be a view
        // you have finished with, waiting to be closed.
        //
        // It used to hand the task to the sidebar's detail *pane*. There is no
        // pane to hand it to any more, and that is the point of this task —
        // whichever key opens the detail, it is the panel that draws it.
        kanban::Action::Open { id } => FromPage::Show(crate::detail::Focus::Task(id)),
        kanban::Action::Quit => FromPage::Close,
    }
}

/// A key, while a task written up has the pane.
///
/// Two things to do with something you are reading — stop, and edit it — and
/// four keys for the first, because `q`, `esc` and `ctrl-c` all mean "out" to
/// some hand or other. `↵` is among them for the reason `Z` and `K` close with
/// the key that opened them: the way out of a page is the way in.
///
/// Everything else is deliberately nothing. The tree's keys are aimed at a row
/// under a cursor, and there is no cursor on this page; letting them through
/// would act on whatever the tree happened to be pointing at behind it.
fn task_key(k: Key, p: &TaskPage, ui: &mut Ui, self_ws: Option<&str>) -> FromPage {
    match k {
        Key::Enter | Key::Char('q') | Key::Esc | Key::Interrupt => FromPage::Close,
        // The one thing this page is not, reached from it in one key. `E` means
        // what it means everywhere: `$EDITOR` on the prose, full size, in the
        // reader's own terminal. See [`pop_out`].
        Key::Char('E') => {
            let (argv, label) = match &p.focus {
                crate::detail::Focus::Task(id) => {
                    (vec!["edit".to_string(), id.clone()], id.clone())
                }
                crate::detail::Focus::Project(id) => {
                    (vec!["project".to_string(), "edit".to_string(), id.clone()], id.clone())
                }
                crate::detail::Focus::Nothing => return FromPage::Nothing,
            };
            say(ui, pop_out(&argv, &label, self_ws));
            FromPage::Nothing
        }
        _ => FromPage::Nothing,
    }
}

pub(super) fn event_loop(
    store: &Store,
    tx: &Sender<Msg>,
    rx: &Receiver<Msg>,
    here: &Where,
    full: bool,
    screen: &mut dyn Screen,
) -> Outcome {
    let started_as = exe_stamp();
    let mut view = View::default();
    view.takes_the_tab(full);
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
    // for knowing whether it was already here when they clicked. Both come off
    // the [`Where`] the caller handed in rather than out of the environment,
    // because a surface's environment is herdr's and says nothing true about
    // where the surface is.
    let (self_ws, me) = (here.ws(), here.pane());
    let live = screen.live();
    // The two focus questions, asked of the reading the frame is built from and
    // kept by the loop rather than carried in the view.
    //
    // Whether the keyboard is in this pane decides what a click means, and a
    // click is the one gesture that arrives in a pane nobody is working in.
    // One pane on the machine answers yes: `pane.list` marks the focused pane
    // of the focused workspace and nothing else, which is why it is a census
    // question rather than a workspace one.
    //
    // Whether this workspace is on screen is the other half of the same picture
    // and is not a substitute — a panel in the workspace being looked at is on
    // screen, and the shell beside it may still be the pane being worked in. It
    // decides the cadence, far below.
    //
    // Neither is drawn, which is why neither is in the [`Snapshot`]: see
    // `crate::live`, and `crate::draw`'s "focus is not an input".
    let (mut keyboard, mut self_focused) = screen.focus(&live, me, self_ws);
    let snap = Snapshot::live(store, live.panes);
    // What shape of thing this pane is, before the rows are built from it: a
    // page shows a project's tasks all of them and a sidebar shows six. See
    // [`View::wide`], and the tick below, which is where a pane that changes
    // shape under a running panel is noticed.
    let mut drawn_size = screen.size();
    view.fit_to_pane(drawn_size.0);
    let mut ui = collect(&snap, &view);
    if point_at(&mut ui, &want) {
        want = Cursor::default();
    }
    if agreed.is_empty() {
        agreed = shared::rendered(&shared::Shared::of(&view, ui.cursor()));
    }
    let mut dirty = false;
    let mut last_fetch = Instant::now();
    // A scroll is a burst of events, and focus is a socket round-trip. Take it
    // on the first of a burst and not on the ninety after it.
    let mut took_focus = Instant::now() - Duration::from_secs(60);
    let mut last_fingerprint = store.fingerprint();
    // The other half of "has anything changed": a hand raised or lowered. It
    // is a state file rather than a task, so the fingerprint above — which
    // walks `projects/` and `tasks/` — cannot see it, and a panel that watched
    // only the store would show a flag whenever the store next happened to
    // change and never on a quiet machine.
    let mut last_flags = store.flags_stamp();
    // The status the frame in front of the reader was drawn with, and when we
    // last asked herdr whether it still holds. See [`STATUS_POLL`].
    let mut drawn_status: HashMap<String, String> = HashMap::new();
    let mut last_poll = Instant::now();
    // Messages taken off the channel while coalescing a burst of herdr events,
    // to be handled in the order they arrived rather than thrown away.
    let mut carry: std::collections::VecDeque<Msg> = Default::default();

    // The page, when one is up, and `None` for the tree. The whole of "what is
    // drawn in the room" — see [`Page`] — and it is one variable because there
    // is one pane: a page replaces the tree rather than sitting over it.
    let mut page: Option<Page> = None;

    // `&mut` on the view because the frame is where the tree's scroll offset
    // is decided, and the view keeps it: the click handler two branches below
    // has to read the offset the pane in front of the reader is drawn with.
    //
    // The board is drawn from the same size and painted through the same
    // backend, so nothing below this line knows which of the two it is looking
    // at. That is what makes the page cheap: the host was told a number of
    // columns and the choice of what goes in them never left this process.
    fn draw(
        screen: &mut dyn Screen,
        ui: &Ui,
        view: &mut View,
        page: Option<&Page>,
    ) -> (usize, usize) {
        let (w, h) = screen.size();
        match page {
            Some(p) => {
                // The panel's own footer message, handed to whichever page is
                // up. One message and one clock across all of them, so a
                // sentence written by a key on the tree does not vanish the
                // moment a page takes the pane. See [`super::render::NOTE`].
                let note = ui
                    .message
                    .as_ref()
                    .filter(|(_, at)| at.elapsed() < super::render::NOTE)
                    .map(|(m, _)| m.as_str())
                    .unwrap_or_default();
                screen.paint(&p.frame(note, w, h), w, h)
            }
            None => screen.paint(&frame(ui, view, w, h), w, h),
        }
    }
    drawn_size = draw(screen, &ui, &mut view, page.as_ref());

    loop {
        let msg = match carry.pop_front() {
            Some(m) => m,
            None => match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => Msg::Tick,
                Err(RecvTimeoutError::Disconnected) => return Outcome::Quit,
            },
        };

        // Pointing at a pane is a statement that you are working in it, so
        // the mouse takes the keyboard before it means anything else. Only the
        // round-trip is here: `apply_input` owns the fact, and this owns the
        // clock, because a scroll is a burst of events and focus is a socket
        // call. Everything the click then *means* — select, activate, the strip
        // — is in the reducer, where a fixture can drive it.
        if matches!(msg, Msg::Key(Key::Click { .. } | Key::Wheel { .. }))
            && took_focus.elapsed() > Duration::from_millis(400)
        {
            took_focus = Instant::now();
            focus_self(me, self_ws);
        }

        let mut refetch = false;
        let is_key = matches!(msg, Msg::Key(_));
        // The person is driving. Whatever the file wanted, they want this.
        //
        // The wheel is the exception, and it is the same exception it makes
        // everywhere: it moves the view and deliberately leaves the cursor
        // alone, so a wish about where the cursor belongs is not answered by a
        // scroll and still stands after one.
        if matches!(msg, Msg::Key(k) if !matches!(k, Key::Wheel { .. })) {
            want = Cursor::default();
        }
        // The pane's size is the loop's to read, and `render::row_at` is the
        // arithmetic that drew the frame — so the row a click lands on is the
        // row that acts.
        let (w, h) = screen.size();
        // Where a command has just moved a card to, for the board's rebuild.
        // Named while the id is still in hand, because after the rebuild the
        // slot it sat in belongs to whatever moved up into it.
        let mut follow: Option<String> = None;
        match msg {
            // A page has the pane and the keyboard with it. The tree's reducer
            // is not consulted at all — not to fall through to and not to
            // suppress: what is in front of the reader is a board or a task
            // written up, and a key that quietly moved a cursor they cannot see
            // would be the seam showing. See [`board_key`] and [`task_key`].
            Msg::Key(k) if page.is_some() => {
                let acted = page.as_mut().map(|p| match p {
                    Page::Board(b) => board_key(k, b, &mut ui, self_ws),
                    Page::Task(t) => task_key(k, t, &mut ui, self_ws),
                });
                match acted {
                    None | Some(FromPage::Nothing) => {}
                    Some(FromPage::Refetch(card)) => {
                        follow = card;
                        refetch = true;
                    }
                    // One ask at the new page's width rather than a give-back
                    // and a fresh take: the sidebar never blinks through its
                    // own width on the way from a board to a task.
                    Some(FromPage::Show(focus)) => {
                        let panes = screen.live().panes;
                        let next = Page::Task(TaskPage::open(store, focus.clone(), panes));
                        expand(screen, &mut view, Some(next.width()));
                        view.showing = Some(focus);
                        page = Some(next);
                        // Deliberately no refetch. The page was built from the
                        // store a line ago and nothing about this keystroke
                        // changed it; the tree behind is not on screen and is
                        // rebuilt when the room goes back. A refetch here would
                        // be a second read of the store, an `adopt` and a focus
                        // round-trip, on every `↵` from a board.
                    }
                    Some(FromPage::Close) => {
                        // The room goes back through the same seam it was asked
                        // for. The page is dropped whatever the host says: a
                        // host that has stopped answering must not be able to
                        // leave a page nobody can close.
                        expand(screen, &mut view, None);
                        page = None;
                        // `↵` on the same row has to mean open again, and the
                        // tree behind has not been rebuilt while the page was
                        // up unless something asked it to — which a command run
                        // on a board is exactly.
                        view.showing = None;
                        refetch = true;
                    }
                }
            }
            Msg::Key(k) => match apply_input(Input::Key(k), &mut ui, &mut view, w, h, &mut keyboard)
            {
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
                // `↵`, the second caller of the seam and the last pane the
                // panel rented for itself. See [`TaskPage`]: from a surface,
                // the pane this used to split is not the panel's own but the
                // one somebody is working in.
                //
                // A host that cannot be asked still splits it, because a `↵`
                // that did nothing at all would be worse than one that costs a
                // pane — that is the fallback, and it is the whole of what a
                // tty panel and an older herdr still do.
                Effect::Inspect(focus) => {
                    // Built before it is asked for, so that the number asked
                    // for is the page's own and is named in one place. On a
                    // host that refuses, the reads are thrown away — six of the
                    // store, and a census that a surface already has in hand —
                    // which is nothing beside the pane split that follows.
                    let next =
                        Page::Task(TaskPage::open(store, focus.clone(), screen.live().panes));
                    if expand(screen, &mut view, Some(next.width())) {
                        page = Some(next);
                        view.showing = Some(focus);
                    } else {
                        let msg = inspect(store, self_ws, &focus, me);
                        if msg.is_empty() {
                            view.showing = Some(focus);
                        } else {
                            say(&mut ui, msg);
                        }
                    }
                }
                // Reached only on the fallback: a page takes the keyboard, so
                // `↵` and `q` on one go to [`task_key`] and never through the
                // tree's reducer. What is left here is the pane, and closing it
                // is what it always was.
                Effect::CloseView => {
                    if close_view(store, self_ws, me) {
                        say(&mut ui, "closed");
                    }
                    view.showing = None;
                }
                Effect::PopOut { argv, label } => {
                    say(&mut ui, pop_out(&argv, &label, self_ws));
                }
                // `K`, the third caller of the one seam, and a line of it is
                // the ask. What is different is the number — a board has no
                // width that is enough, so it takes what the host has; see
                // [`super::render::WHOLE_SCREEN`] — and what is drawn in the
                // room, which herdr is never told. A host that cannot be asked
                // opens the tab it always opened.
                Effect::Board { scope, label } => {
                    // Built now rather than on the next refetch: this is a
                    // board of the store as it is, and the frame at the foot of
                    // this loop has to have something to draw.
                    let next = Page::Board(BoardPage::open(
                        store,
                        scope.clone(),
                        view.show_done,
                        screen.live().panes,
                    ));
                    if expand(screen, &mut view, Some(next.width())) {
                        page = Some(next);
                        // Nothing said, where `Z` says "the whole tree". A
                        // widened tree still looks like the tree and is worth a
                        // word; a board looks like nothing else on this pane.
                        // And the footer it would be written on is the board's
                        // key hint — four seconds of naming what is obviously
                        // on screen, in place of the keys that work on it.
                    } else {
                        say(&mut ui, open_board(&scope, &label, self_ws));
                    }
                }
                // `Z`, both ways. A host that owns the rect is asked for the
                // room and asked to take it back again, which is one key and
                // one field rather than a mode; a host that cannot be asked
                // opens the tab it always opened. See [`expand`].
                Effect::Full => {
                    let want = view
                        .asked_width
                        .is_none()
                        .then_some(super::render::PAGE_MIN);
                    if expand(screen, &mut view, want) {
                        // Nothing is refetched and nothing is redrawn here. The
                        // host answers by resizing us, the tick notices the new
                        // shape, and the rows are rebuilt then — see
                        // [`View::fit_to_pane`]. Doing it now would build them
                        // for a width we have only asked for.
                        say(
                            &mut ui,
                            match want {
                                Some(_) => "the whole tree",
                                None => "the sidebar",
                            },
                        );
                    } else {
                        let m = open_full(self_ws);
                        say(&mut ui, m);
                    }
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
                                    focus_self(me, self_ws);
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
                                deed: Box::new(Effect::Run {
                                    argv: more,
                                    escalate: None,
                                    then,
                                }),
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
                // It is also what keeps `keyboard` honest, and this is the
                // event that can do it: measured against the live server, herdr
                // raises `workspace.focused` on every move of the keyboard
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
                // The pane has changed shape since the rows were built — `Z`,
                // a split dragged wider, a window resized. Almost all of that
                // the frame answers by itself, because it is drawn to whatever
                // it measures; this is the part it cannot, because a page and a
                // sidebar do not have the same rows in them. See [`View::wide`].
                //
                // Read off the last frame rather than measured here, so the
                // shape and the frame in front of the reader agree — see the
                // `draw` closure.
                let was = view.wide;
                view.fit_to_pane(drawn_size.0);
                if view.wide != was {
                    refetch = true;
                    dirty = false;
                }
                // A hand raised outranks the cadence.
                //
                // Everything else the background gate defers is *news about
                // work* — a task started somewhere, an agent picking something
                // up — and half a minute late is fine for that, because nobody
                // is waiting on this pane to notice. A card is the opposite: an
                // agent has stopped and asked, and the answer is a keystroke on
                // a panel. Thirty seconds of that is thirty seconds of an agent
                // idle for no reason, and worse in the other direction — a card
                // answered on the panel in front of you stayed up on the other
                // twenty-one for the rest of the interval, so switching
                // workspaces inside it landed on a settled question holding the
                // keyboard.
                //
                // It can be checked this often because of what it costs: one
                // `stat` of one file, against the `readdir` of two directories
                // and the two socket round-trips a refetch makes. At five ticks
                // a second across twenty-two panels that is a hundred stats a
                // second and no allocation — cheaper than the status poll
                // below, which is a socket call and already runs on every
                // unfocused panel.
                let flags_now = store.flags_stamp();
                if flags_now != last_flags {
                    refetch = true;
                    dirty = false;
                }
                // Ask the one question the event stream cannot answer, and only
                // where it goes unanswered: an unfocused panel, between its full
                // refetches. A status that has moved is news of exactly the kind
                // [`SHAPE`] exists for, so it refetches now rather than waiting
                // out the rest of the thirty seconds.
                //
                // An empty list is herdr not answering, not everybody finishing
                // at once — this call degrades to empty the same way
                // [`crate::live::read`] does, and reading that as news would
                // clear the dock every time the socket hiccuped.
                if !self_focused && last_poll.elapsed() >= STATUS_POLL {
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
                let interval = if self_focused {
                    Duration::from_millis(250)
                } else {
                    Duration::from_secs(30)
                };
                if last_fetch.elapsed() >= interval {
                    if started_as.is_some() && exe_stamp() != started_as {
                        return Outcome::Reload;
                    }
                    // Flags are not read here: they are checked on every tick
                    // above, at the top of this arm, and a second reading on
                    // the slow gate would only ever be the same answer later.
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
            last_flags = store.flags_stamp();
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
            let live = screen.live();
            // Free: the panes were fetched for the frame either way, and this is
            // the same reading herdr answers "who has the keyboard" and "which
            // workspace is on screen" out of. Both are read here rather than off
            // the frame, because a frame is what a renderer draws and neither of
            // these is ever drawn.
            //
            // The keyboard half goes in through the same door a key does, and
            // not because the assignment needs help: focus entering or leaving
            // the panel is an input, it changes what the next click means, and
            // a harness that could only *set the flag* would be driving a path
            // this loop does not take. See [`Input::Focus`].
            let (has_keyboard, on_screen) = screen.focus(&live, me, self_ws);
            apply_input(Input::Focus(has_keyboard), &mut ui, &mut view, w, h, &mut keyboard);
            self_focused = on_screen;
            let snap = Snapshot::live(store, live.panes);
            // Likewise free, and it has to be taken from the same list the frame
            // is drawn from: the poll asks whether herdr has moved on from what
            // the reader is looking at, so what the reader is looking at is what
            // it must be compared against.
            drawn_status.clear();
            drawn_status.extend(snap.panes.iter().map(|p| (p.pane.clone(), p.state.clone())));
            refetch_into(&mut ui, &snap, &mut view);
            if point_at(&mut ui, &want) {
                want = Cursor::default();
            }
            // The board is the same store read a different way, so it is
            // rebuilt on exactly the news the tree is — including a card an
            // agent moved while nobody here pressed anything. The tree above is
            // rebuilt too, unseen, and that is worth its one pass over rows
            // already fetched: it is what makes giving the room back land on
            // the tree as it is now rather than as it was before the board
            // started changing it.
            if let Some(p) = page.as_mut() {
                p.rebuild(store, follow.take(), snap.panes.clone());
            }
        }

        // Only input can change the durable half, so only input is worth the
        // comparison. A tick that serialised this five times a second in every
        // pane on the machine would be pure heat. The mouse shares from its own
        // branches above, which return before they reach here.
        if is_key {
            shared::share(store, &view, ui.cursor(), &mut agreed);
        }
        drawn_size = draw(screen, &ui, &mut view, page.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The source of this file, for the one test below that is about *where* a
    /// line sits rather than what it computes. Read the way the help test in
    /// `main.rs` reads its own dispatch: the check then reads exactly what the
    /// binary does, rather than a restatement of it that can drift.
    const SRC: &str = include_str!("run.rs");

    /// A raised hand is not on the background cadence.
    ///
    /// An unfocused panel refetches every thirty seconds, which is right for
    /// news about work — nobody is waiting on that pane to notice a task
    /// changing status. A card is the opposite: an agent has stopped and asked,
    /// and the answer is a keystroke on a panel. Half a minute of that is half
    /// a minute of an agent idle for nothing, and worse the other way round — a
    /// card answered on the panel in front of you stayed up on the other
    /// twenty-one until their interval came round, so switching workspaces
    /// inside it arrived at a settled question holding the keyboard.
    ///
    /// So the stamp is read at the top of the tick, before the gate, and this
    /// is a test about that order. It is one `stat` against the `readdir` and
    /// two socket calls a refetch costs, which is what makes the position
    /// affordable — put it back inside the gate and the cost is unchanged and
    /// the cards are late.
    #[test]
    fn a_flag_is_read_on_every_tick_rather_than_on_the_cadence() {
        // The newline matters: `Msg::Tick => {}` appears first, in the drain
        // that coalesces a burst of herdr events, and splitting on the bare
        // prefix would land in that one — which reads no flags and would fail
        // this test for a reason that has nothing to do with it.
        let tick = SRC
            .split("Msg::Tick => {\n")
            .nth(1)
            .expect("the tick arm moved");
        let stamp = tick.find("flags_stamp()").expect("the tick no longer reads the flags at all");
        let gate = tick
            .find("if last_fetch.elapsed() >= interval")
            .expect("the cadence gate moved");
        assert!(
            stamp < gate,
            "the flag check sits behind the cadence gate — an unfocused panel \
             would take up to thirty seconds to raise a card, and as long again \
             to put away one somebody else has answered",
        );
    }

    /// The page's keys, and the ones it deliberately swallows.
    ///
    /// `↵` is the way out as well as the way in, for the reason `Z` and `K`
    /// close what they opened. Everything else must be *nothing*: the tree's
    /// keys are aimed at a row under a cursor and there is no cursor on this
    /// page, so a `d` that fell through would move a card the reader cannot
    /// see, on whatever row the tree happened to be pointing at behind it.
    #[test]
    fn a_page_of_prose_is_left_only_by_the_keys_that_say_so() {
        let page = TaskPage {
            focus: crate::detail::Focus::Task("wsp-013".into()),
            ctx: crate::detail::Ctx {
                tasks: Vec::new(),
                index: crate::resolve::Index::new(Vec::new()),
                claims: Default::default(),
                worked: Default::default(),
                bindings: Default::default(),
                panes: Vec::new(),
                columns: Vec::new(),
            },
        };
        let mut ui = collect(&Snapshot::default(), &View::default());

        for k in [Key::Enter, Key::Char('q'), Key::Esc, Key::Interrupt] {
            assert!(
                matches!(task_key(k, &page, &mut ui, None), FromPage::Close),
                "{k:?} should give the room back",
            );
        }
        // A sample of the tree's own vocabulary, none of which has a target here.
        for k in [Key::Char('j'), Key::Char('d'), Key::Char('X'), Key::Char('Z'), Key::Char('K')] {
            assert!(
                matches!(task_key(k, &page, &mut ui, None), FromPage::Nothing),
                "{k:?} reached past the page to the tree behind it",
            );
        }
    }

    /// `↵` must never become a key that does nothing.
    ///
    /// The page is what a host that answers gives you; a tty panel and any
    /// herdr from before widths existed cannot be asked, and there the detail
    /// still has to appear somewhere — which is the pane this task exists to
    /// stop renting. Keeping both is what makes the wsp half shippable against
    /// a herdr nobody has rebuilt, and losing the second one is a change that
    /// works perfectly for whoever makes it.
    ///
    /// Read out of the source because no fixture drives this loop; the honest
    /// half — that a host with nothing to give says so — is tested where it can
    /// be, on `expand`.
    #[test]
    fn the_key_that_opens_a_page_still_opens_a_pane_where_there_is_nobody_to_ask() {
        let arm = SRC
            .split("Effect::Inspect(focus) => {")
            .nth(1)
            .expect("the inspect arm moved");
        let arm = &arm[..arm.find("Effect::CloseView").expect("the arm below it moved")];
        assert!(arm.contains("if expand("), "`↵` no longer asks for the room at all");
        assert!(
            arm.contains("inspect(store"),
            "the fallback went: on a tty panel and on an older herdr `↵` now does nothing",
        );
    }

    /// A page has the keyboard, and the thing that makes that true is where one
    /// arm sits relative to another.
    ///
    /// Both arms match `Msg::Key`, and the board's is a *guarded* one — so it
    /// only works above the tree's. Put it below and the guard never fires:
    /// every key would go to the tree's reducer, moving a cursor nobody can
    /// see, and `q` would quit the panel instead of giving the room back. That
    /// is silent in a way nothing else here is — the board would keep drawing,
    /// correctly, of a board that no longer answers its own keys.
    ///
    /// Read out of the source for the same reason the tick test above is: the
    /// order of two match arms is not something the type checker has an opinion
    /// about, and no fixture can drive this loop.
    #[test]
    fn a_page_takes_the_keys_before_the_tree_does() {
        let page = SRC.find("Msg::Key(k) if page.is_some()").expect("the board's arm went");
        let tree = SRC
            .find("Msg::Key(k) => match apply_input")
            .expect("the tree's arm moved");
        assert!(
            page < tree,
            "the board's arm is below the tree's, so its guard never fires and \
             every key on the board goes to the tree behind it",
        );
    }

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

    /// Measuring the pane costs no process.
    ///
    /// `draw` asks for the size on every pass of a 200 ms loop, so whatever
    /// this function costs is paid five times a second, by every panel on the
    /// machine, for as long as they are open. As `stty size` that was 12 ms of
    /// CPU a second in a panel that was doing nothing and 59% of its on-CPU
    /// samples. A frame drawn from a spawned `stty` looks exactly like a frame
    /// drawn from `TIOCGWINSZ`, which is why the guard is a test and not a
    /// comment: nothing on screen would tell anyone it had come back.
    #[test]
    fn measuring_the_pane_spawns_no_process() {
        let after = SRC.split("fn term_size()").nth(1).expect("term_size was renamed");
        let body = &after[..after.find("\n}\n").expect("term_size has no end")];
        assert!(
            !body.contains("Command::new"),
            "term_size spawns a process again, five times a second in every panel",
        );
    }
}
