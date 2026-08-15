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
use super::verbs::{close_view, inspect, open_workspace, pop_out, run_wsp};


pub(super) enum Msg {
    Key(Key),
    /// Carries the workspace the event was about, when it named one.
    Herdr(Option<String>),
    Tick,
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

pub(super) fn spawn_events(tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        let tx2 = tx.clone();
        let res = herdr::subscribe(
            &[
                "workspace.created",
                "workspace.closed",
                "workspace.renamed",
                "workspace.focused",
                "pane.created",
                "pane.exited",
                "pane.agent_status_changed",
            ],
            move |_e, d| {
                let ws = d
                    .get("workspace_id")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                tx2.send(Msg::Herdr(ws)).is_ok()
            },
        );
        if res.is_err() {
            std::thread::sleep(Duration::from_secs(3));
        } else {
            std::thread::sleep(Duration::from_millis(500));
        }
    });
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
    let mut last_fingerprint = store.fingerprint();

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
        let msg = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => Msg::Tick,
            Err(RecvTimeoutError::Disconnected) => return Outcome::Quit,
        };

        let mut refetch = false;
        match msg {
            // Step one of mouse support, and the only question that matters:
            // does a click on a pane you are not focused in reach the program
            // running in it, or does herdr take it to move focus? Report what
            // arrives and let a person click. Until that is answered, a click
            // does nothing else — landing the cursor somewhere on the strength
            // of coordinates nobody has checked is how you find out they were
            // off by a header.
            Msg::Key(k @ (Key::Click { .. } | Key::Wheel { .. })) => {
                match k {
                    Key::Click { x, y } => say(&mut ui, format!("click  col {x}  row {y}")),
                    Key::Wheel { up } => {
                        say(&mut ui, format!("wheel {}", if up { "up" } else { "down" }))
                    }
                    _ => {}
                }
                draw(&ui, &view, &mut last);
            }
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
                Effect::Run { argv, escalate } => {
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
                        }
                        // Refused, and there is a stronger form of the same
                        // command: show what the CLI said and ask again.
                        (Err(e), Some(more)) => {
                            view.mode = Mode::Confirm {
                                question: e,
                                argv: more,
                                escalate: None,
                            };
                        }
                        (Err(e), None) => say(&mut ui, e),
                    }
                    refetch = true;
                }
            },
            Msg::Herdr(ws) => {
                // An event naming our own workspace means we are probably about
                // to be looked at. Coalesce the burst that follows a split or a
                // focus change, then mark dirty — the tick is 200ms away and
                // will pick it up, so there is no reason to sleep here.
                while rx.try_recv().is_ok() {}
                let concerns_us = ws.is_some() && ws.as_deref() == self_ws;
                dirty = true;
                if concerns_us || ui.self_focused {
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
