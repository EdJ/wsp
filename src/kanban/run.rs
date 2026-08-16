//! The pane the board runs in.
//!
//! Everything impure: raw mode, the poll, and turning an [`Action`] into a
//! command. The same bargain [`crate::detail`] makes — no event stream, because
//! a board is a reader and a second subscriber per workspace would double
//! herdr's fan-out for a pane that can wait a tenth of a second.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::herdr;
use crate::input::{self, Key};
use crate::panel::{self, to_ansi};
use crate::resolve::Index;
use crate::store::Store;

use super::{apply_key, collect, frame, Action, Board, Ctx, Cursor, Scope};

/// How long the footer keeps what it was told.
const NOTE: Duration = Duration::from_secs(4);

pub fn run(store: &Store, args: &crate::Args) -> i32 {
    let index = Index::new(store.projects());
    let scope = match Scope::of(store, args, &index) {
        Ok(s) => s,
        Err(code) => return code,
    };

    // Not a terminal: print the board once, as text, and go. `wsp kanban | cat`
    // is how anyone first looks at it, and a pane full of escape codes is a
    // worse answer than a plain one.
    if !crate::util::stdout_is_tty() {
        let board = collect(&Ctx::live(store), &scope, args.has("done"));
        let (w, _) = panel::term_size();
        // Tall enough for the longest column: a pipe has no bottom edge, and a
        // board that scrolled would be hiding cards from a reader who cannot
        // press a key.
        let tallest = board.columns.iter().map(|c| c.cards.len()).max().unwrap_or(0);
        for l in frame(&board, &Cursor::default(), w.max(80), tallest + 9, "") {
            println!("{}", l.text().trim_end());
        }
        return 0;
    }

    print!("\x1b[?1049h\x1b[?25l");
    let _ = std::io::stdout().flush();
    panel::stty(&["raw", "-echo", "min", "0", "time", "1"]);
    let code = board_loop(store, &scope, args.has("done"));
    panel::stty(&["sane"]);
    print!("\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();

    // The binary changed under a pane that will live for hours. Same answer the
    // panel and the detail pane give: replace this process, so the pane, its
    // pty and its place in the layout all survive.
    if code == RELOAD {
        if let Ok(exe) = std::env::current_exe() {
            use std::os::unix::process::CommandExt;
            let mut c = std::process::Command::new(exe);
            c.arg("kanban");
            if let Some(name) = args.rest.first() {
                c.arg(name);
            }
            let err = c.exec();
            eprintln!("wsp: could not reload: {err}");
            return 1;
        }
    }
    0
}

/// Not an exit code anyone sees — the loop's way of saying the binary moved.
const RELOAD: i32 = -1;

fn board_loop(store: &Store, scope: &Scope, mut show_done: bool) -> i32 {
    let started_as = crate::util::exe_stamp();
    let mut board = collect(&Ctx::live(store), scope, show_done);
    let mut cur = Cursor::default();
    let mut note: Option<(String, Instant)> = None;
    let mut painted = String::new();
    let mut fingerprint = store.fingerprint();
    let mut last_poll = Instant::now();

    let Ok(mut tty) = std::fs::File::open("/dev/tty") else { return 1 };
    let mut keys = input::Keys::new();
    let mut pressed: Vec<Key> = Vec::new();

    loop {
        let text = note
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTE)
            .map(|(m, _)| m.clone())
            .unwrap_or_default();
        let (w, h) = panel::term_size();
        let now = to_ansi(&frame(&board, &cur, w, h, &text), w, h);
        if now != painted {
            print!("{now}");
            let _ = std::io::stdout().flush();
            painted = now;
        }

        let mut buf = [0u8; 1];
        match tty.read(&mut buf) {
            // `min 0 time 1` gives a 100ms read timeout, so this doubles as the
            // poll interval: a keypress is felt at once rather than after the
            // rest of a tick, and the empty read is also what settles a lone
            // Esc.
            Ok(1) => keys.feed(buf[0], &mut pressed),
            Ok(_) => keys.idle(&mut pressed),
            Err(_) => {
                keys.idle(&mut pressed);
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        let mut rebuild = false;
        // A card the next rebuild has to put the cursor back on, wherever the
        // command just run has moved it to.
        let mut follow: Option<String> = None;
        for k in pressed.drain(..) {
            match apply_key(k, &board, &mut cur) {
                Action::None => {}
                Action::Quit => return 0,
                Action::Refetch => rebuild = true,
                Action::ShowDone => {
                    show_done = !show_done;
                    note = Some((
                        if show_done { "showing done" } else { "hiding done" }.into(),
                        Instant::now(),
                    ));
                    rebuild = true;
                }
                Action::Say(m) => note = Some((m, Instant::now())),
                // Hand the task to the panel's own detail pane and go. Only if
                // it landed: with no panel in this workspace there is nowhere
                // to hand it to, and a board that closed anyway would take the
                // task off the screen instead of putting it on one.
                Action::Open { id } => {
                    let ws = herdr::Env::read().workspace_id;
                    let focus = crate::detail::Focus::Task(id);
                    match panel::inspect(store, ws.as_deref(), &focus) {
                        m if m.is_empty() => return 0,
                        m => note = Some((m, Instant::now())),
                    }
                }
                Action::Edit { id } => {
                    let ws = herdr::Env::read().workspace_id;
                    let msg = panel::pop_out(
                        &["edit".to_string(), id.clone()],
                        &id,
                        ws.as_deref(),
                    );
                    note = Some((msg, Instant::now()));
                }
                // The CLI does the work, exactly as the panel's keys do: the
                // event log, the hooks and the commit all happen because it is
                // the same code path a person at a shell would take.
                Action::Run { argv, task } => {
                    match panel::run_wsp(&argv) {
                        Ok(m) => note = Some((m.label, Instant::now())),
                        Err(e) => note = Some((e, Instant::now())),
                    }
                    // The card is about to be in another column, and the cursor
                    // goes with it. Named here, while the id is still in hand:
                    // after the rebuild the slot it was in belongs to whatever
                    // moved up into it.
                    follow = Some(task);
                    rebuild = true;
                }
            }
        }

        // Somebody else changed the store — an agent finishing a task is the
        // common case, and a board that only moved when you pressed a key would
        // be a photograph.
        if last_poll.elapsed() >= Duration::from_millis(400) {
            last_poll = Instant::now();
            if started_as.is_some() && crate::util::exe_stamp() != started_as {
                return RELOAD;
            }
            if store.fingerprint() != fingerprint {
                rebuild = true;
            }
        }

        if rebuild {
            let keep = follow.or_else(|| board.card_at(&cur).map(|c| c.id.clone()));
            board = collect(&Ctx::live(store), scope, show_done);
            fingerprint = store.fingerprint();
            cur = keep.and_then(|id| board.find(&id)).unwrap_or_else(|| clamped(&board, cur));
        }
    }
}

/// The nearest place the cursor can actually be, after the card it was on has
/// gone. A column that empties under the cursor is ordinary — it is what
/// finishing the last card in it looks like.
fn clamped(board: &Board, cur: Cursor) -> Cursor {
    let col = cur.col.min(board.columns.len().saturating_sub(1));
    let row = cur.row.min(board.columns.get(col).map(|c| c.cards.len()).unwrap_or(0).saturating_sub(1));
    Cursor { col, row }
}
