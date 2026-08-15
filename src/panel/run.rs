//! The pane: the terminal it runs in, and the loop that keeps it honest.
//!
//! Everything impure lives here. Raw mode, the input thread, herdr's event
//! stream, the tick, and the loop that turns an [`Effect`] into something that
//! actually happens.

use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::herdr;
use crate::input::Key;
use crate::store::Store;

use super::keys::{apply_key, say, Effect, Mode, View};
use super::render::{frame, to_ansi};
use super::rows::{collect, refetch_into, AgentRef, Snapshot, Ui};
use super::verbs::{close_view, inspect, open_workspace, pop_out, run_wsp, send_tell};


pub(super) enum Msg {
    Key(Key),
    Herdr(HerdrEvent),
    Tick,
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
/// though it is the noisiest thing herdr sends.
const CHATTY: &[&str] = &["workspace.focused", "workspace.renamed", "pane.updated"];

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

pub(super) fn spawn_events(tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        let tx2 = tx.clone();
        let types: Vec<&str> = SHAPE.iter().chain(CHATTY).copied().collect();
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
            let ev = HerdrEvent {
                workspace: ws,
                shape: is(SHAPE, e),
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

/// Size and mtime of the binary we are running. Cheap enough to check on every
/// tick, and enough to notice an `install` underneath us.
pub(crate) fn exe_stamp() -> Option<(u64, u64)> {
    let path = std::env::current_exe().ok()?;
    let m = std::fs::metadata(path).ok()?;
    let secs = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((m.len(), secs))
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

    let outcome = event_loop(store, &rx, self_ws.as_deref());

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

pub(super) fn event_loop(store: &Store, rx: &Receiver<Msg>, self_ws: Option<&str>) -> Outcome {
    let started_as = exe_stamp();
    let mut view = View::default();
    let mut ui = collect(&Snapshot::live(store), &view, self_ws);
    let mut last = String::new();
    let mut dirty = false;
    let mut last_fetch = Instant::now();
    // Who we are, for taking focus when the mouse says the reader is here.
    let me = herdr::Env::read().pane_id;
    // A scroll is a burst of events, and focus is a socket round-trip. Take it
    // on the first of a burst and not on the ninety after it.
    let mut took_focus = Instant::now() - Duration::from_secs(60);
    let mut last_fingerprint = store.fingerprint();
    // Messages taken off the channel while coalescing a burst of herdr events,
    // to be handled in the order they arrived rather than thrown away.
    let mut carry: std::collections::VecDeque<Msg> = Default::default();

    let draw = |ui: &Ui, view: &View, last: &mut String| {
        let (w, h) = term_size();
        let painted = to_ansi(&frame(ui, view, w, h), w, h);
        if painted != *last {
            print!("{painted}");
            let _ = std::io::stdout().flush();
            *last = painted;
        }
    };
    draw(&ui, &view, &mut last);

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
        if matches!(msg, Msg::Key(Key::Click { .. } | Key::Wheel { .. }))
            && took_focus.elapsed() > Duration::from_millis(400)
        {
            took_focus = Instant::now();
            focus_self(me.as_deref(), self_ws);
        }

        if let Msg::Key(Key::Click { y, .. }) = msg {
            let (w, h) = term_size();
            match super::keys::click(&mut ui, &mut view, w, h, y) {
                super::keys::Hit::Activate => msg = Msg::Key(Key::Enter),
                super::keys::Hit::Select => {
                    draw(&ui, &view, &mut last);
                    continue;
                }
                super::keys::Hit::Nothing => continue,
            }
        }

        // The tree scrolls by holding the cursor near the middle of the pane,
        // so it has no scroll offset of its own to move: the wheel moves the
        // cursor and the view follows. A separate offset would fight that
        // centring every time a key moved the selection.
        if let Msg::Key(Key::Wheel { up }) = msg {
            super::keys::wheel(&mut ui, &mut view, up);
            draw(&ui, &view, &mut last);
            continue;
        }

        let mut refetch = false;
        match msg {
            Msg::Key(k) => match apply_key(k, &mut ui, &mut view) {
                Effect::Quit => return Outcome::Quit,
                Effect::None => {}
                Effect::Refetch => refetch = true,
                Effect::Focus(a) => focus(&a),
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
                Effect::Open { label, cwd, project, task } => {
                    match open_workspace(&label, cwd.as_deref(), project.as_deref(), task.as_deref())
                    {
                        Ok((_ws, pane)) => {
                            // The workspace exists; the durable record of what
                            // it is for is the claim, so make it now rather
                            // than relying on the env surviving a restart.
                            match &task {
                                Some(t) => {
                                    let r = run_wsp(&[
                                        "claim".into(),
                                        t.clone(),
                                        "--pane".into(),
                                        pane.clone(),
                                    ]);
                                    say(&mut ui, match r {
                                        Ok(m) => m.label,
                                        Err(e) => e,
                                    });
                                }
                                None => say(&mut ui, format!("opened {label}")),
                            }
                        }
                        Err(e) => say(&mut ui, e),
                    }
                    refetch = true;
                }
                Effect::Tell(t) => match send_tell(&t) {
                    Ok(()) => say(&mut ui, t.note),
                    Err(e) => say(&mut ui, e),
                },
                Effect::Run { argv, escalate, then } => {
                    match (run_wsp(&argv), escalate) {
                        (Ok(m), _) => {
                            // Land the cursor on whatever was just created, so
                            // `E` is the next key rather than a hunt. Quick
                            // capture stays one line; writing it up is a
                            // keystroke away rather than a mode you were forced
                            // through.
                            match (&m.id, argv.first().map(|s| s.as_str())) {
                                (Some(id), Some("add")) | (Some(id), Some("project")) => {
                                    view.land_on = Some(id.clone());
                                    say(&mut ui, format!("{id} added · E to write it up"));
                                }
                                _ => say(&mut ui, m.label),
                            }
                            // Only now the command has actually worked. The
                            // footer takes the sentence's line over the
                            // command's: what you want to know about a claim
                            // is whether the agent was told.
                            if let Some(t) = then {
                                match send_tell(&t) {
                                    Ok(()) => say(&mut ui, t.note),
                                    Err(e) => say(&mut ui, e),
                                }
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
                if shape || focused_us {
                    refetch = true;
                    dirty = false;
                }
            }
            Msg::Tick => {
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
            refetch_into(&mut ui, &Snapshot::live(store), &mut view, self_ws);
        }
        draw(&ui, &view, &mut last);
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
}
