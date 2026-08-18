//! `wsp surface` — the panel drawn by somebody else.
//!
//! The panel has always been a pane: a tty it measured, escapes it painted,
//! and a pane id it asked herdr about. This is the same panel with none of
//! those, driven over a pipe by a host that owns the cells — herdr's forked
//! sidebar today, and whatever comes after it.
//!
//! # Why the surface and not the host draws
//!
//! The host could have linked wsp in and called `frame` itself. The reason it
//! does not is a deployment cost, and it is the whole argument for this file
//! existing: a herdr release build is five minutes and restarting herdr kills
//! every running agent, where `wsp install` swaps one file and costs nothing
//! that is running. Every row, colour and count that lives on this side of the
//! pipe is one that can be changed without either.
//!
//! So the split is: **wsp decides what the cells are, the host puts them on a
//! screen.** Nothing across the pipe knows what a task, a project or an agent
//! is, in either direction.
//!
//! # What crosses it
//!
//! One JSON object per line, both ways, and anything a side does not recognise
//! is ignored rather than fatal — so the two binaries can be upgraded in
//! either order, which given that one of them is expensive to rebuild is not a
//! nicety.
//!
//! In: `host` (the cells there are, whether the keyboard is here), `key` as it
//! was **typed**, `mouse` in the surface's own coordinates, and `live` — what
//! the host is running. A key arrives as a code and a character rather than as
//! [`Key`], deliberately: binding Tab here tomorrow must not need a new host.
//!
//! Out: `frame`, as rows of spans carrying **attributes** rather than style
//! names, for the mirror-image reason — the palette is wsp's, and changing it
//! must not need a new host either.
//!
//! # Told what is running, rather than asking
//!
//! `live` is the one that changes the shape of the thing. A panel in a pane is
//! a tenant of herdr and has to *ask* what is running — `workspace.list` and
//! `pane.list`, on a clock, whether or not anything moved. A surface's host
//! **is** herdr, and herdr knows the instant an agent starts, stops or asks a
//! question, because it is the thing that noticed. So it pushes the same rows
//! down this pipe when they change, and the surface stops asking: no socket
//! round-trips and no event subscription, which measured 6.9% of a core down
//! to 2.3% in a live herdr on 2026-08-18. See [`Running`].
//!
//! It removes **one of the two clocks**, and the honest version says which.
//! What a task is, whether it is claimed, whether somebody raised a hand — all
//! of that is in the store, nothing in herdr knows it changed, and no event
//! could carry it. So the store is still polled, and what is left of the 2.3%
//! is very nearly all of that.
//!
//! # What it is not
//!
//! Not a second panel. [`super::run::event_loop`] is the same loop the tty
//! runs, with a different [`Screen`] under it; the rows, the keys, the verbs
//! and the effects are the ones `wsp panel` has. If this file ever grows a
//! decision about what to draw, it is in the wrong place.
//!
//! # When it stops
//!
//! Two things end a surface, and both end it by **exiting**: the host starts a
//! surface that has exited again, so an exit is the only restart either side
//! has to build.
//!
//! - **stdin reached EOF.** The host has gone — closed the pipe on its way out,
//!   or died and had it closed by the kernel. Nothing will read another frame
//!   and nothing is left to reap us. Measured on 2026-08-18: a surface that
//!   drew on through this was reparented to init and spun at about 4% of a core
//!   for as long as the machine stayed up, once per herdr exit, reported by
//!   nobody. herdr does kill a child that ignores the EOF, but only over the
//!   two seconds it is alive to do it, and SIGKILL is the case no side can
//!   handle from over there. So it is handled here, where a read simply ends.
//! - **The binary changed on disk.** `wsp install` under a running herdr is the
//!   property the whole split above was chosen for, and it is not delivered
//!   until the process drawing the sidebar is the new one. The check is the
//!   loop's, and is the one every pane panel already makes; what differs is the
//!   answer. A panel re-execs, because it owns a pane and a tty it would lose;
//!   a surface owns nothing, so it goes and comes back as the new binary. Until
//!   it does, the host draws its own sidebar — see `sync` in herdr's
//!   `wsp_sidebar.rs`, where the wait before the restart is decided.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::herdr::{Pane, Workspace};
use crate::input::Key;
use crate::live::{self, Live};
use crate::store::Store;

use super::render::{rgb_of, Line, Style};
use super::run::{event_loop, Msg, Outcome, Screen, Where};

/// The wire version this build speaks. Bumped only when a change is not
/// backwards compatible; most additions are not, because unknown messages are
/// dropped on both sides.
const PROTOCOL: u32 = 1;

/// How long the surface waits to be told what herdr is running before it goes
/// back to asking.
///
/// A host that pushes the census sends it in the same breath as the first
/// `host` message, so this is not a wait anybody sees — it is how the surface
/// tells the two kinds of host apart without either having to declare itself.
/// An older herdr, or no host at all, simply never says, and after this the
/// surface subscribes to herdr's event stream and polls exactly as a pane
/// panel does. Nobody has to upgrade the two binaries in step.
const TOLD_BY: std::time::Duration = std::time::Duration::from_secs(1);

/// How often the surface asks the *other* machines what they are running.
///
/// A host can only push what it is. wsp can be pointed at other machines and
/// joins their panes into the same tree, and no herdr on this one has anything
/// to say about those — so they keep a clock, and it is the slow one: a far
/// listing crosses a tunnel, and a pane on another laptop that turns up three
/// seconds late is nobody's complaint. Skipped entirely when there are no far
/// machines, which is the usual case.
const FAR_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// How long after the sidebar comes up to look for panel panes an older wsp
/// left behind.
///
/// Slack rather than a measurement. herdr restores its workspaces, tabs and
/// panes and starts this surface, and nothing here is told which of those
/// finished first — a sweep that ran before the layout came back would find
/// nothing to sweep. Being early costs a husk that stays until the next start;
/// being late costs nothing at all, because it runs on a thread nobody is
/// waiting for. See [`super::install::sweep_husks`].
const SETTLE: std::time::Duration = std::time::Duration::from_secs(3);

/// Size to draw at before the host has said. It is `term_size`'s own fallback,
/// for the same reason: a first frame at a nonsense width is worse than a
/// first frame at a plausible one.
const UNTOLD: (usize, usize) = (26, 40);

/// What the host has told us, shared with the loop it is read by.
#[derive(Debug)]
struct Host {
    size: (usize, usize),
    /// Whether the host says the keyboard is in this surface. It is the answer
    /// to the question a pane has to ask herdr's census for, and a surface is
    /// simply told — which is the one place this is *better* than a pane and
    /// not merely different.
    focused: bool,
}

/// What is running, as far as this surface knows.
///
/// The point of `fork-001`, held in one struct. A panel in a pane learns what
/// herdr is running by **asking** — `workspace.list` and `pane.list`, four
/// times a second, whether or not anything moved — which is a child
/// interrogating its own parent, and it was most of what a surface cost while
/// showing a picture that never changed. A surface is *told* instead: its host
/// is herdr, herdr knows the moment an agent starts, stops or asks something,
/// and it pushes the same rows down the pipe the frames come back on.
///
/// Two fields because there are two sources and only one of them can push.
#[derive(Debug, Default)]
struct Running {
    /// The host's own machine, as the host last said it. `None` until it says
    /// anything, which is also how a host that does not push is recognised —
    /// see [`TOLD_BY`].
    here: Option<(Vec<Workspace>, Vec<Pane>)>,
    /// Everywhere else. Nothing can push these, so they are polled; see
    /// [`FAR_POLL`].
    far: (Vec<Workspace>, Vec<Pane>),
}

/// The panel drawn into a host's cells.
struct Wire {
    host: Arc<Mutex<Host>>,
    running: Arc<Mutex<Running>>,
    /// The frame already sent. A tick that changes nothing is most ticks, and
    /// a surface that resent its whole frame five times a second would put the
    /// cost the tty screen avoids onto a pipe instead.
    last: String,
    /// And where a copy of it is left for a reader that has no pipe to us. See
    /// [`remember`].
    frame_file: PathBuf,
}

impl Screen for Wire {
    fn size(&self) -> (usize, usize) {
        self.host.lock().map(|host| host.size).unwrap_or(UNTOLD)
    }

    fn paint(&mut self, lines: &[Line], w: usize, h: usize) -> (usize, usize) {
        let rows = frame_rows(lines, w, h);
        let painted = frame_json(&rows);
        if painted != self.last {
            println!("{painted}");
            let _ = std::io::stdout().flush();
            self.last = painted;
            // Inside the same guard as the print, and after it: the host is
            // what is waiting for this frame, and the copy on disk is for
            // whoever asks later.
            remember(&self.frame_file, &rows, w, h);
        }
        (w, h)
    }

    /// A surface is chrome: it is on screen whenever its host is drawing it,
    /// so it never falls to the background cadence the way a panel in a
    /// workspace nobody is looking at does. Whether it holds the keyboard is
    /// the host's word rather than a census question.
    fn focus(&self, _live: &Live, _me: Option<&str>, _ws: Option<&str>) -> (bool, bool) {
        let focused = self.host.lock().map(|host| host.focused).unwrap_or(false);
        (focused, true)
    }

    /// What the host has said, joined with what only a socket can answer for.
    ///
    /// No round-trip in the ordinary case, which is the whole of `fork-001`:
    /// the rows were pushed when they changed and this hands back the last
    /// ones, so a frame built while nothing is happening costs a clone.
    ///
    /// A host that has never said anything is an older herdr, or no host at
    /// all, and then this asks exactly as a pane panel would. That is the only
    /// compatibility either binary needs: the two can be upgraded in either
    /// order, and the older pair simply keeps the older cost.
    fn live(&self) -> Live {
        let Ok(running) = self.running.lock() else {
            return live::read();
        };
        let Some((workspaces, panes)) = running.here.as_ref() else {
            return live::read();
        };
        let (far_workspaces, far_panes) = &running.far;
        live::of(
            workspaces.iter().chain(far_workspaces).cloned().collect(),
            panes.iter().chain(far_panes).cloned().collect(),
        )
    }
}

/// A frame, as the host reads it.
///
/// Separate from [`Wire::paint`], which prints it, because this is the part
/// the two binaries have to agree on and printing is not something a test can
/// look at.
fn frame_json(rows: &[Value]) -> String {
    json!({ "t": "frame", "lines": rows }).to_string()
}

/// The rows of a frame, before either of the two things done with one.
///
/// Split out from [`frame_json`] because the frame goes two places now — down
/// the pipe, and into the file [`remember`] writes — and building it twice or
/// parsing it back would be two ways for the picture on disk to stop being the
/// picture the host was given.
fn frame_rows(lines: &[Line], w: usize, h: usize) -> Vec<Value> {
    let mut rows = Vec::with_capacity(lines.len().min(h));
    for line in lines.iter().take(h) {
        let mut line = line.clone();
        // Padded to the full width here rather than by the host, so an inverse
        // row is inverse to the edge: the selection is a bar, and a bar that
        // stops at the last character is a highlight.
        line.fit(w);
        let spans = line.spans.iter().map(|s| span(s.style, &s.text, line.selected)).collect();
        rows.push(Value::Array(spans));
    }
    rows
}

/// One span, as attributes.
///
/// `selected` rides on every span of its row rather than on the row, because
/// the row is a list on the wire and an attribute on the row would be a second
/// shape for a host to handle for one flag.
fn span(style: Style, text: &str, selected: bool) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("s".into(), json!(text));
    if let Some((r, g, b)) = rgb_of(style) {
        out.insert("fg".into(), json!(format!("#{r:02x}{g:02x}{b:02x}")));
    }
    if let Style::Bold = style {
        out.insert("bold".into(), json!(true));
    }
    if let Style::Dim = style {
        out.insert("dim".into(), json!(true));
    }
    if selected {
        out.insert("inv".into(), json!(true));
    }
    Value::Object(out)
}

/// Leave the frame the host was just given where something that is not the
/// host can read it.
///
/// `wsp peek panel` is how a governor sees what a person is actually looking
/// at, and on 2026-08-18 it found two real rendering bugs by reading a frame
/// rather than the code that built it. It worked by asking herdr to read a
/// *pane*. A surface is not a pane — that is the property the fork exists to
/// get — so there is nothing to ask, and the frame existed only on a pipe
/// between two processes, neither of them `peek`.
///
/// The alternative was for `peek` to draw a fresh frame out of the same store.
/// That is simpler, and it answers a different question: what the sidebar
/// *would* show, at a size and a cursor position `peek` would have to invent.
/// A picture that has stopped moving is a whole class of bug for something
/// that draws from pushed events rather than polling, and the only thing that
/// can show one is the frame the host was actually handed.
///
/// Cost is the frames that change and no others: this sits inside the same
/// guard as the print, so a surface showing a still picture writes nothing.
/// [`crate::store::write_atomic_unsynced`] rather than the durable write
/// beside it — a reader must never see half a frame, which the rename gives,
/// but a frame is worth nothing after a crash, and an fsync per keystroke
/// would put back a share of the cost `fork-001` measured out of this loop.
fn remember(path: &Path, rows: &[Value], w: usize, h: usize) {
    let record = json!({
        // Not read by anything yet. Written because the one question this file
        // cannot otherwise answer is *whose* picture it is, and a peek that
        // starts doubting the frame will want it.
        "pid": std::process::id(),
        "at": crate::util::now_iso(),
        "cols": w,
        "rows": h,
        "lines": rows,
    });
    let _ = crate::store::write_atomic_unsynced(path, &record.to_string());
}

/// Where that file sits: beside the bindings and the daemon marker, because it
/// is machine state of exactly their kind — true only while a process is up,
/// and keyed on the state directory, so a sandbox's surface and the live one
/// do not paint over each other's picture.
///
/// Nothing tidies it away when a surface exits. A frame from a surface that
/// has gone is not misleading on its own; what would be is reading one as
/// live, and that is [`crate::daemon::surface_drawing`]'s question rather than
/// this file's.
pub(crate) fn frame_path(state: &Path) -> PathBuf {
    state.join("surface-frame.json")
}

/// The last frame a surface drew, as something to print.
pub(crate) struct Frame {
    /// The cells it was built for — the sidebar's rect, as the host reported
    /// it. Worth printing beside the frame: half of what this is used to find
    /// is a row costing more of them than it should.
    pub cols: usize,
    pub rows: usize,
    /// When it was drawn, ISO. The one field that says whether the picture is
    /// live: a surface writes only a frame that *changed*, so an old stamp
    /// means the sidebar has not moved — which is either nothing happening or
    /// the bug being looked for.
    pub at: String,
    /// One string per row, right-trimmed. The spans on disk carry colour, bold
    /// and the selection bar; nothing reads them yet, and a reader that wants
    /// to paint them has them without a surface change.
    pub lines: Vec<String>,
}

/// Read it back. `None` for a state directory no surface has ever drawn in, and
/// for a record this build cannot parse — a frame that cannot be read is not a
/// frame, and the caller has something to say either way.
pub(crate) fn last_frame(state: &Path) -> Option<Frame> {
    let raw = std::fs::read_to_string(frame_path(state)).ok()?;
    let record: Value = serde_json::from_str(&raw).ok()?;
    let lines = record
        .get("lines")?
        .as_array()?
        .iter()
        .map(|row| {
            let text: String = row
                .as_array()
                .map(|spans| {
                    spans.iter().filter_map(|s| s.get("s").and_then(Value::as_str)).collect()
                })
                .unwrap_or_default();
            // The padding is the wire's, so a selection bar reaches the edge of
            // a host's cells. On a page it is trailing whitespace.
            text.trim_end().to_string()
        })
        .collect();
    let cells = |name: &str| record.get(name).and_then(Value::as_u64).unwrap_or(0) as usize;
    Some(Frame {
        cols: cells("cols"),
        rows: cells("rows"),
        at: record.get("at").and_then(Value::as_str).unwrap_or_default().to_string(),
        lines,
    })
}

/// Read the host until it stops talking.
///
/// Every message is a `Msg::Tick` as well as whatever else it is: a host
/// message changes the size the next frame is built for, and a tick is how the
/// loop is told to build one without inventing a second kind of wake-up.
fn read_host(
    reader: impl BufRead,
    host: Arc<Mutex<Host>>,
    running: Arc<Mutex<Running>>,
    tx: Sender<Msg>,
) {
    let mut said_protocol = false;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message.get("t").and_then(Value::as_str) {
            Some("host") => {
                // Said once, loudly, on the channel the host logs. A wire this
                // build cannot read is not something to keep drawing through:
                // the frames would be right and the keys silently wrong.
                if let Some(spoken) = message.get("protocol").and_then(Value::as_u64) {
                    if spoken != u64::from(PROTOCOL) && !said_protocol {
                        said_protocol = true;
                        eprintln!(
                            "wsp surface: host speaks protocol {spoken}, this build speaks {PROTOCOL}"
                        );
                    }
                }
                if let Ok(mut host) = host.lock() {
                    let cols = message.get("cols").and_then(Value::as_u64).unwrap_or(0);
                    let rows = message.get("rows").and_then(Value::as_u64).unwrap_or(0);
                    if cols > 0 && rows > 0 {
                        // The same floor `term_size` applies. A host that hands
                        // us four columns gets a frame built for sixteen and
                        // clipped, which is a bad picture; laying rows out in
                        // four columns is not a picture at all.
                        host.size = ((cols as usize).max(16), (rows as usize).max(6));
                    }
                    // Absent means unchanged, the way an absent size does two
                    // lines above. They used not to agree: a message carrying
                    // only new cells cleared `focused`, so a resize moved the
                    // keyboard out of a sidebar nobody had left — and the next
                    // click into it would be swallowed as the one that takes
                    // focus back. Nothing on either side of the pipe reports
                    // that; it reads as a click that did not register.
                    if let Some(f) = message.get("focused").and_then(Value::as_bool) {
                        host.focused = f;
                    }
                }
            }
            // What herdr is running, pushed rather than asked for. See
            // [`Running`], and `wsp_sidebar.rs` in the fork for the half that
            // decides when to send one.
            //
            // It goes on to the loop as a herdr event of the kind that cannot
            // wait, because that is exactly what it is: the host does not send
            // one unless something moved, so every one of these is news. The
            // rows are read by the same parsers a socket answer is, and a
            // message with neither list is still an answer — a host with no
            // panes at all is a herdr somebody just closed everything in.
            Some("live") => {
                let workspaces = message
                    .get("workspaces")
                    .and_then(Value::as_array)
                    .map(|rows| rows.iter().map(crate::herdr::parse_workspace).collect())
                    .unwrap_or_default();
                let panes = message
                    .get("panes")
                    .and_then(Value::as_array)
                    .map(|rows| rows.iter().map(crate::herdr::parse_pane).collect())
                    .unwrap_or_default();
                if let Ok(mut running) = running.lock() {
                    running.here = Some((workspaces, panes));
                }
                if tx.send(Msg::Herdr(told())).is_err() {
                    return;
                }
                continue;
            }
            Some("key") => {
                if let Some(key) = key_of(&message) {
                    if tx.send(Msg::Key(key)).is_err() {
                        return;
                    }
                    continue;
                }
            }
            Some("mouse") => {
                if let Some(key) = mouse_of(&message) {
                    if tx.send(Msg::Key(key)).is_err() {
                        return;
                    }
                    continue;
                }
            }
            // A message from a newer host. Dropped, not fatal.
            _ => continue,
        }
        if tx.send(Msg::Tick).is_err() {
            return;
        }
    }
    // The host has gone, or the loop it was being fed has. Returning is all
    // this does with either: the caller is what knows there is a process to
    // end, and it is the only one that can — dropping this sender does *not*
    // end the loop's `recv`, because the ticker holds a sender of its own.
    // Reading it as if it did is how a surface came to outlive its host.
}

/// What the loop is handed when the host says what is running.
///
/// The loop's own vocabulary, and the urgent end of it: `shape` is the flag
/// that means *refetch now* rather than *at the next cadence*, and a pushed
/// census earns it by construction — the host sends one only when something it
/// draws has changed. It names no workspace because it is about all of them.
fn told() -> super::run::HerdrEvent {
    super::run::HerdrEvent { workspace: None, shape: true, focus: false }
}

/// A typed key, given wsp's meaning.
///
/// The two control keys are recognised here rather than sent as themselves,
/// because that is where the panel's vocabulary is: `Key::Interrupt` and
/// `Key::KillLine` exist because a raw terminal delivers ctrl-c and ctrl-u as
/// bytes, and a host that has already decoded the keyboard would otherwise
/// have to know which control characters wsp happens to care about.
fn key_of(message: &Value) -> Option<Key> {
    let code = message.get("code").and_then(Value::as_str)?;
    let ctrl = message
        .get("mods")
        .and_then(Value::as_array)
        .is_some_and(|mods| mods.iter().any(|m| m.as_str() == Some("ctrl")));
    Some(match code {
        "char" => {
            let ch = message.get("ch").and_then(Value::as_str)?.chars().next()?;
            match (ctrl, ch.to_ascii_lowercase()) {
                (true, 'c') => Key::Interrupt,
                (true, 'u') => Key::KillLine,
                (true, _) => return None,
                (false, _) => Key::Char(ch),
            }
        }
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "enter" => Key::Enter,
        "esc" => Key::Esc,
        "backspace" => Key::Backspace,
        "home" => Key::Home,
        "end" => Key::End,
        // Everything else a keyboard can send, and nothing here binds. Dropped
        // rather than mapped onto whatever is nearest: a key that does the
        // wrong thing is worse than a key that does nothing.
        _ => return None,
    })
}

/// A press or the wheel, in the surface's own cells.
///
/// Only a left press becomes a click. The panel has no use for a release, a
/// drag or another button, and forwarding them would put three more shapes
/// through `keys::click` for nothing.
fn mouse_of(message: &Value) -> Option<Key> {
    let kind = message.get("kind").and_then(Value::as_str)?;
    let at = |name: &str| message.get(name).and_then(Value::as_u64).unwrap_or(0) as usize;
    Some(match kind {
        "down" if message.get("button").and_then(Value::as_str) == Some("left") => {
            Key::Click { x: at("col"), y: at("row") }
        }
        "scrollup" => Key::Wheel { up: true },
        "scrolldown" => Key::Wheel { up: false },
        _ => return None,
    })
}

/// End the process, saying why where the host will see it.
///
/// Both exits come through here: stderr is piped and logged by the host, and
/// the one thing a person wants after a sidebar has blinked is a line saying
/// what took it away. Exiting rather than returning because one of the two
/// happens on a thread with nothing to return to.
fn stop(why: &str) -> ! {
    eprintln!("wsp surface: {why}");
    std::process::exit(0)
}

/// Run the panel against a host on stdin and stdout.
pub fn run(store: &Store) -> i32 {
    if !crate::herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    // The conversion's tidy-up, on a thread of its own: closing somebody else's
    // dead panes is not what the person watching this sidebar come up is
    // waiting for, and it must not be able to delay the first frame.
    {
        let state = store.state.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SETTLE);
            match super::install::sweep_husks(&state) {
                0 => {}
                n => eprintln!("wsp surface: closed {n} panel pane(s) an older wsp left behind"),
            }
        });
    }
    let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
    let running = Arc::new(Mutex::new(Running::default()));
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        });
    }
    {
        let host = Arc::clone(&host);
        let running = Arc::clone(&running);
        let tx = tx.clone();
        std::thread::spawn(move || {
            read_host(std::io::stdin().lock(), host, running, tx);
            stop("the host closed its end");
        });
    }
    // Subscribing to herdr's event stream is what a panel does to find out
    // that something changed, and a surface whose host pushes the answer
    // outright has no use for it: the stream's job was to say *look again*,
    // and there is nothing left to look at. `pane.updated` alone is two events
    // a second per working agent, each one a full pane parsed out of JSON, and
    // each one used to mark the panel dirty and buy a full refetch on the next
    // tick — which is where most of what this task removed actually went.
    //
    // So it is started only for a host that turns out not to push. Waiting is
    // the way to tell, because the message that says so is the one that never
    // comes; see [`TOLD_BY`].
    {
        let running = Arc::clone(&running);
        let tx = tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(TOLD_BY);
            if running.lock().is_ok_and(|running| running.here.is_none()) {
                super::run::spawn_events(tx);
            }
        });
    }
    // The machines the host cannot speak for. One thread, one slow clock, and
    // only when there is somewhere else to ask; see [`FAR_POLL`].
    if crate::herdr::anywhere_else() {
        let running = Arc::clone(&running);
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(FAR_POLL);
            let far = (
                crate::herdr::workspaces_elsewhere(),
                crate::herdr::panes_elsewhere(),
            );
            let moved = match running.lock() {
                Ok(mut running) if running.far != far => {
                    running.far = far;
                    true
                }
                Ok(_) => false,
                Err(_) => return,
            };
            // Only when it moved. A far machine sitting still should cost the
            // rows nothing: waking the loop would rebuild every row and read
            // the whole store to draw the same frame.
            if moved && tx.send(Msg::Herdr(told())).is_err() {
                return;
            }
        });
    }

    // No workspace and no pane: that is what a surface *is*, and it is said
    // here rather than read from the environment, which is herdr's and belongs
    // to whatever pane herdr itself was started from. What a key acts *on* is
    // resolved from the workspace on screen at the moment it is pressed — see
    // `stage` in [`super::verbs`].
    let mut screen =
        Wire { host, running, last: String::new(), frame_file: frame_path(&store.state) };
    match event_loop(store, &tx, &rx, &Where::nowhere(), false, &mut screen) {
        // Exit, where a panel re-execs. The host is already watching for a
        // surface that ended and already starts another; re-execing here would
        // be a second way to do the same thing, and the one the host cannot
        // see. Reached with the loop finished and the last frame flushed, so
        // what the host has on screen when its own sidebar takes over is a
        // whole frame rather than half of one.
        Outcome::Reload => stop("the wsp binary changed on disk, exiting for a new one"),
        Outcome::Quit => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The source of this file, for the one test below that is about *where* a
    /// line sits rather than what it computes — read the way `run.rs` reads its
    /// own tick arm.
    const SRC: &str = include_str!("surface.rs");

    fn message(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    /// A surface nobody has told anything: what a host that does not push
    /// leaves behind, and what every test about the *other* messages wants.
    fn nothing_told() -> Arc<Mutex<Running>> {
        Arc::new(Mutex::new(Running::default()))
    }

    /// The failure this is against is not a wrong picture, it is a process.
    /// Measured 2026-08-18: every herdr exit left a surface reparented to init,
    /// spinning on a pipe with nobody at the other end, and neither side said
    /// anything about it. The read ending is the only notice a child gets of a
    /// host that was killed, so it has to end rather than block for ever.
    #[test]
    fn a_host_that_has_gone_ends_the_read_rather_than_leaving_a_surface_behind() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        // Held, so the read ends because stdin did and not because the loop it
        // sends to went away first — which is the other way out of `read_host`
        // and would pass this test without testing anything.
        let (tx, _rx) = std::sync::mpsc::channel::<Msg>();
        let (done, ended) = std::sync::mpsc::channel::<()>();

        std::thread::spawn(move || {
            read_host("{\"t\":\"host\",\"cols\":30,\"rows\":40}\n".as_bytes(), host, nothing_told(), tx);
            let _ = done.send(());
        });

        assert!(
            ended.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the reader has to come back when the host's end closes: it is what \
             the caller turns into an exit, and nothing else ever will",
        );
    }

    /// Both exits are the process ending, and the second one is easy to get
    /// wrong by copying: a pane panel answers the same reload by re-execing
    /// itself, because it owns a pane and a tty it would otherwise lose. A
    /// surface owns neither, and its host already watches for a child that
    /// ended and starts another — so replacing ourselves here would be a
    /// restart the host cannot see, beside the one it can.
    #[test]
    fn a_new_binary_ends_the_surface_where_it_would_replace_a_panel() {
        let arm = SRC
            .split("Outcome::Reload =>")
            .nth(1)
            .expect("the reload arm moved");
        let arm = &arm[..arm.find("Outcome::Quit").expect("the quit arm moved")];
        assert!(arm.contains("stop("), "a new binary has to end the surface");

        let after = SRC
            .split("read_host(std::io::stdin().lock(), host, running, tx);")
            .nth(1)
            .expect("the reader thread moved");
        assert!(
            after.trim_start().starts_with("stop("),
            "a read that has ended has to end the process on the next line, or \
             the ticker goes on waking a loop drawing into a closed pipe",
        );
    }

    /// What makes the reload prompt rather than eventual. The loop checks the
    /// binary on the fast cadence and only there — 250ms when the panel is on
    /// screen, thirty seconds when it is not — and a surface is chrome, so it
    /// is on screen whenever anything at all is drawn. Answer this `false` and
    /// `wsp install` takes up to half a minute to reach the sidebar somebody is
    /// looking at while they change it.
    #[test]
    fn a_surface_is_on_screen_so_a_new_binary_is_seen_within_a_tick_of_install() {
        let wire = Wire {
            host: Arc::new(Mutex::new(Host { size: UNTOLD, focused: false })),
            running: nothing_told(),
            last: String::new(),
            frame_file: PathBuf::new(),
        };

        let (keyboard, on_screen) = wire.focus(&Live::default(), None, None);

        assert!(on_screen, "a surface is drawn by its host or it is not running");
        assert!(!keyboard, "and whether it has the keyboard is the host's word");
    }

    /// `fork-001`, from this side: the rows arrive down the pipe the frames go
    /// back on, and are read by the parser a socket answer is read by.
    ///
    /// The event that goes with them is the urgent kind on purpose. A pushed
    /// census only exists because something moved — the host does not send one
    /// otherwise — so waiting for the next cadence to redraw would be waiting
    /// for news already in hand.
    #[test]
    fn what_the_host_is_running_arrives_down_the_pipe_rather_than_over_a_socket() {
        let running = nothing_told();
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();

        read_host(
            concat!(
                r#"{"t":"live","workspaces":[{"workspace_id":"w9","label":"fork","focused":true}],"#,
                r#""panes":[{"pane_id":"w9:p1","workspace_id":"w9","label":"fork-001","#,
                r#""agent":"claude","agent_status":"working","cwd":"/tmp","focused":true}]}"#,
                "\n",
            )
            .as_bytes(),
            Arc::new(Mutex::new(Host { size: UNTOLD, focused: false })),
            Arc::clone(&running),
            tx,
        );

        let told = running.lock().unwrap();
        let (workspaces, panes) = told.here.as_ref().expect("the host said what it is running");
        assert_eq!(workspaces[0].label, "fork");
        assert_eq!(panes[0].pane_id, "w9:p1");
        assert_eq!(panes[0].agent_status, "working");
        assert!(
            matches!(rx.recv(), Ok(Msg::Herdr(e)) if e.shape),
            "a census is news, and the kind that redraws now rather than at the next tick",
        );
    }

    /// The saving, stated as a behaviour: a surface that has been told does not
    /// ask. The ids below exist on no herdr on this machine, so an answer
    /// carrying them can only have come from what the host said.
    ///
    /// The far machine is in the same reading because a host can only push
    /// what it is: wsp joins other machines' panes into the same tree, and
    /// nothing on this one can speak for those — see [`FAR_POLL`].
    #[test]
    fn a_surface_that_has_been_told_what_is_running_does_not_ask_for_it() {
        let running = nothing_told();
        {
            let mut told = running.lock().unwrap();
            told.here = Some((
                vec![Workspace { id: "w9".into(), label: "here".into(), ..Workspace::default() }],
                vec![Pane { pane_id: "w9:p1".into(), workspace_id: "w9".into(), ..Pane::default() }],
            ));
            told.far = (
                vec![Workspace { id: "w2@far".into(), label: "far".into(), ..Workspace::default() }],
                vec![Pane {
                    pane_id: "w2:p1@far".into(),
                    workspace_id: "w2@far".into(),
                    ..Pane::default()
                }],
            );
        }
        let wire = Wire {
            host: Arc::new(Mutex::new(Host { size: UNTOLD, focused: false })),
            running,
            last: String::new(),
            frame_file: PathBuf::new(),
        };

        let live = wire.live();

        let panes: Vec<&str> = live.panes.iter().map(|p| p.pane.as_str()).collect();
        assert_eq!(panes, vec!["w9:p1", "w2:p1@far"]);
        assert_eq!(
            live.panes[0].workspace_label, "here",
            "and the join against the workspace list is the one live.rs already does",
        );
        assert_eq!(live.panes[1].workspace_label, "far");
    }

    /// A host with nothing running is a herdr somebody has just closed
    /// everything in, and it has to be readable as an answer. Read as silence
    /// instead, the surface would go on asking for ever — and worse, would
    /// keep drawing panes that are gone.
    #[test]
    fn a_host_running_nothing_has_still_said_so() {
        let running = nothing_told();
        let (tx, _rx) = std::sync::mpsc::channel::<Msg>();

        read_host(
            "{\"t\":\"live\",\"panes\":[],\"workspaces\":[]}\n".as_bytes(),
            Arc::new(Mutex::new(Host { size: UNTOLD, focused: false })),
            Arc::clone(&running),
            tx,
        );

        assert!(running.lock().unwrap().here.is_some());
    }

    #[test]
    fn a_key_arrives_as_it_was_typed_and_is_given_its_meaning_here() {
        assert!(matches!(
            key_of(&message(r#"{"code":"char","ch":"j","mods":[]}"#)),
            Some(Key::Char('j'))
        ));
        assert!(matches!(
            key_of(&message(r#"{"code":"char","ch":"c","mods":["ctrl"]}"#)),
            Some(Key::Interrupt)
        ));
        assert!(matches!(
            key_of(&message(r#"{"code":"esc","mods":[]}"#)),
            Some(Key::Esc)
        ));
    }

    #[test]
    fn a_key_this_wsp_does_not_bind_is_dropped_rather_than_guessed_at() {
        assert!(key_of(&message(r#"{"code":"f","ch":"7","mods":[]}"#)).is_none());
        assert!(key_of(&message(r#"{"code":"tab","mods":[]}"#)).is_none());
        assert!(key_of(&message(r#"{"code":"char","ch":"k","mods":["ctrl"]}"#)).is_none());
    }

    #[test]
    fn the_wheel_and_a_left_press_are_the_only_mouse_the_panel_has_a_use_for() {
        assert!(matches!(
            mouse_of(&message(r#"{"kind":"down","button":"left","col":3,"row":7}"#)),
            Some(Key::Click { x: 3, y: 7 })
        ));
        assert!(matches!(
            mouse_of(&message(r#"{"kind":"scrollup","col":0,"row":0}"#)),
            Some(Key::Wheel { up: true })
        ));
        assert!(mouse_of(&message(r#"{"kind":"up","button":"left","col":3,"row":7}"#)).is_none());
        assert!(mouse_of(&message(r#"{"kind":"down","button":"right","col":3,"row":7}"#)).is_none());
    }

    #[test]
    fn a_host_message_sets_the_size_the_next_frame_is_built_for() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        let line = format!(
            "{{\"t\":\"host\",\"protocol\":{PROTOCOL},\"cols\":34,\"rows\":50,\"focused\":true}}\n"
        );

        read_host(line.as_bytes(), Arc::clone(&host), nothing_told(), tx);

        let told = host.lock().unwrap();
        assert_eq!(told.size, (34, 50));
        assert!(told.focused);
        assert!(
            matches!(rx.recv(), Ok(Msg::Tick)),
            "a resize has to wake the loop, or the frame stays the old size"
        );
    }

    /// Focus arriving and leaving, which is the host's word and nothing this
    /// side can ask about.
    ///
    /// A pane panel asks herdr's census who has the keyboard. A surface cannot:
    /// its host *is* herdr, and the answer comes down the pipe with everything
    /// else. Both directions, because the one that matters is the second —
    /// nothing on the panel changes when the keyboard leaves except what the
    /// next click means, so a leave that did not arrive is invisible until
    /// somebody points at the sidebar and nothing happens.
    #[test]
    fn the_keyboard_arriving_and_leaving_the_sidebar_is_told_by_the_host() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        let wire = || Wire {
            host: Arc::clone(&host),
            running: nothing_told(),
            last: String::new(),
            frame_file: PathBuf::new(),
        };
        let (tx, _rx) = std::sync::mpsc::channel::<Msg>();

        let told = |line: &str, tx: Sender<Msg>| {
            read_host(line.as_bytes(), Arc::clone(&host), nothing_told(), tx);
        };

        told("{\"t\":\"host\",\"cols\":34,\"rows\":50,\"focused\":true}\n", tx.clone());
        assert!(wire().focus(&Live::default(), None, None).0, "the keyboard never arrived");

        told("{\"t\":\"host\",\"cols\":34,\"rows\":50,\"focused\":false}\n", tx);
        assert!(!wire().focus(&Live::default(), None, None).0, "and never left again");
    }

    /// And a message that says nothing about it leaves it alone.
    ///
    /// The same rule the size beside it has always had. A resize that moved the
    /// keyboard would make the next click into the sidebar the one that takes
    /// focus back rather than the one that acts — a click that reads, from the
    /// outside, as a click that did not register.
    #[test]
    fn a_host_that_only_says_the_size_does_not_move_the_keyboard() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        let (tx, _rx) = std::sync::mpsc::channel::<Msg>();

        read_host(
            "{\"t\":\"host\",\"cols\":34,\"rows\":50,\"focused\":true}\n\
             {\"t\":\"host\",\"cols\":40,\"rows\":50}\n"
                .as_bytes(),
            Arc::clone(&host),
            nothing_told(),
            tx,
        );

        let told = host.lock().unwrap();
        assert_eq!(told.size, (40, 50), "the resize was dropped");
        assert!(told.focused, "the resize took the keyboard with it");
    }

    #[test]
    fn a_narrow_host_gets_a_clipped_frame_rather_than_a_layout_in_four_columns() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        let (tx, _rx) = std::sync::mpsc::channel::<Msg>();

        read_host(
            "{\"t\":\"host\",\"cols\":4,\"rows\":2}\n".as_bytes(),
            Arc::clone(&host),
            nothing_told(),
            tx,
        );

        assert_eq!(host.lock().unwrap().size, (16, 6));
    }

    #[test]
    fn a_message_from_a_newer_host_is_ignored_rather_than_fatal() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();

        read_host(
            concat!(
                "{\"t\":\"weather\",\"outlook\":\"fine\"}\n",
                "not json at all\n",
                "{\"t\":\"host\",\"cols\":30,\"rows\":40}\n",
            )
            .as_bytes(),
            Arc::clone(&host),
            nothing_told(),
            tx,
        );

        assert_eq!(
            host.lock().unwrap().size,
            (30, 40),
            "the reader has to get past what it does not understand"
        );
        assert!(rx.recv().is_ok());
    }

    #[test]
    fn a_selected_row_is_inverse_to_the_edge_and_a_style_carries_its_colour() {
        let mut line = super::super::render::line(Style::Accent, "wsp");
        line.selected = true;

        let frame: Value = serde_json::from_str(&frame_json(&frame_rows(&[line], 10, 4))).unwrap();
        let spans = frame["lines"][0].as_array().unwrap();

        assert_eq!(spans[0]["s"], json!("wsp"));
        assert_eq!(spans[0]["fg"], json!("#5fbfa4"));
        assert_eq!(spans[0]["inv"], json!(true));
        assert!(
            spans.iter().all(|s| s["inv"] == json!(true)),
            "the selection is a bar: a row inverse only up to its last character is a highlight"
        );
        assert_eq!(
            spans.iter().map(|s| s["s"].as_str().unwrap_or_default().chars().count()).sum::<usize>(),
            10,
            "the row is padded here rather than by the host, so the bar reaches the edge"
        );
    }

    /// The frame on the pipe and the frame on disk are the same frame, and what
    /// comes back off disk is rows a person can read off a page — which is the
    /// whole of what `wsp peek panel` is for: seeing what somebody is looking
    /// at without asking them.
    #[test]
    fn the_frame_the_host_was_given_is_left_where_a_reader_with_no_pipe_can_read_it() {
        let dir = std::env::temp_dir().join(format!("wsp-surface-frame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(last_frame(&dir).is_none(), "nothing has drawn here yet");

        let lines = vec![
            super::super::render::line(Style::Plain, "wsp"),
            super::super::render::line(Style::Dim, "fork-009"),
        ];
        remember(&frame_path(&dir), &frame_rows(&lines, 12, 4), 12, 4);

        let frame = last_frame(&dir).expect("a frame was just written");
        assert_eq!((frame.cols, frame.rows), (12, 4));
        assert_eq!(
            frame.lines,
            vec!["wsp".to_string(), "fork-009".to_string()],
            "the padding is the wire's, so the bar reaches a host's edge; on a page it is trailing space"
        );
        assert!(!frame.at.is_empty(), "an undated frame cannot be told from a stale one");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_is_clipped_to_the_rows_the_host_has() {
        let lines: Vec<Line> = (0..9)
            .map(|i| super::super::render::line(Style::Plain, format!("row {i}")))
            .collect();

        let frame: Value = serde_json::from_str(&frame_json(&frame_rows(&lines, 10, 4))).unwrap();

        assert_eq!(frame["lines"].as_array().unwrap().len(), 4);
    }
}
