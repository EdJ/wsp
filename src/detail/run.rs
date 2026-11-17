//! The pane itself: paint whatever the panel is pointed at, and answer keys.

use std::io::Write;
use std::time::Duration;


use crate::input::{self, Key};
use crate::panel::{self, Line, Style};
use crate::resolve::Index;
use crate::store::Store;
use crate::util;

use super::editors::{close_editors, Closing, MAX_COLUMNS};
use super::render::{frame, Ctx};
use super::{get_focus, Focus, Placed};

pub fn run(store: &Store, args: &crate::Args) -> i32 {
    let ws = args
        .get("workspace")
        .unwrap_or_else(|| "-".into());

    // An explicit id pins the view; otherwise it follows whatever the panel in
    // this workspace last opened.
    let pinned = args.rest.first().and_then(|needle| {
        store
            .find_task(needle)
            .map(|t| Focus::Task(t.id))
            .or_else(|| Index::new(store.projects()).find(needle).map(|p| Focus::Project(p.id.clone())))
    });

    print!("\x1b[?1049h\x1b[?25l");
    let _ = std::io::stdout().flush();
    panel::stty(&["raw", "-echo", "min", "0", "time", "1"]);

    // No event stream here: the view is a reader, and a second subscriber per
    // workspace would double herdr's fan-out for a pane that can wait. Instead
    // poll often and do nothing unless something moved — reading the target
    // and stat-ing the store is cheap, re-reading every task file is not.
    let started_as = crate::util::exe_stamp();
    let mut last = String::new();
    // What the frame in front of the reader was drawn from. The offset is in
    // here with the target and the fingerprint because it is the third thing
    // that changes what is drawn — without it a scroll would move the number
    // and repaint nothing until the store next moved.
    //
    // The attention stamp is the fourth, and it was missing: this pane draws
    // raised hands and messages — see [`super::render`], which filters on
    // `Message::needs_attention` — and `Store::fingerprint` deliberately cannot
    // see either, because both are machine-local state rather than a change to
    // the work. So a hand raised on the task in front of the reader sat
    // undrawn until something unrelated touched the store, which on a quiet
    // machine is nothing at all. Found while fixing the same fault one storey
    // up in `worklist-009`: a gate blind to something its own surface draws.
    // Kept as a separate `u64` rather than mixed into the fingerprint, because
    // two stamps folded into one number can agree by collision and the whole
    // point of this tuple is that it is compared for equality.
    let mut seen: Option<(Focus, u64, u64, usize)> = None;
    // How far down the write-up this pane is, in drawn lines.
    //
    // A local, and there is nowhere else it could be: a `wsp view` pane is one
    // process reading one thing, and when it ends there is nothing to remember.
    // Never clamped here — `frame` is the only thing that knows how long the
    // write-up is, so it clamps and hands it back. See its doc.
    let mut off: usize = 0;
    let mut quit = false;
    let mut reload = false;
    // Panes a previous W could not get rid of. A second W closes them outright.
    let mut stuck: Vec<String> = Vec::new();
    // Half of a key chain: `d` waiting for the column to put details in.
    let mut pending: Option<&'static str> = None;
    let Ok(mut tty) = std::fs::File::open("/dev/tty") else { return 1 };
    let mut keys = input::Keys::new();
    let mut pressed: Vec<Key> = Vec::new();

    while !quit && !reload {
        if started_as.is_some() && crate::util::exe_stamp() != started_as {
            reload = true;
            break;
        }
        let focus = pinned.clone().unwrap_or_else(|| get_focus(store, &ws));
        let fp = store.fingerprint();
        let attention = store.attention_stamp();
        if seen.as_ref() != Some(&(focus.clone(), fp, attention, off)) {
            let (w, h) = panel::term_size();
            let ctx = Ctx::live(store);
            let painted = panel::to_ansi(&frame(&ctx, &focus, w, h, Placed::Pane, &mut off), w, h);
            // After the frame, because the frame is what clamps `off`: storing
            // what was asked for rather than what was drawn would leave a pane
            // at the foot repainting on every `j`.
            seen = Some((focus.clone(), fp, attention, off));
            if painted != last {
                print!("{painted}");
                let _ = std::io::stdout().flush();
                last = painted;
            }
        }

        use std::io::Read;
        let mut buf = [0u8; 1];
        match tty.read(&mut buf) {
            // `min 0 time 1` gives a 100ms read timeout, so this doubles as
            // the poll interval — no separate sleep, and a keypress is felt
            // immediately rather than after the rest of a tick. The empty read
            // is also what settles a lone Esc, so it goes to the parser too.
            Ok(1) => keys.feed(buf[0], &mut pressed),
            Ok(_) => keys.idle(&mut pressed),
            Err(_) => {
                keys.idle(&mut pressed);
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        for k in pressed.drain(..) {
            // Resolve a half-typed chain first. A digit completes it; anything
            // else cancels and is then handled normally, so `d` followed by a
            // change of mind still quits, scrolls or saves rather than being
            // swallowed by a menu the user has stopped talking to.
            if let Some(section) = pending.take() {
                if let Key::Char('1'..='9') = k {
                    let msg = "no pane id".to_string();
                    let _ = (&focus, section);
                    footer(&msg, Style::Accent);
                    continue;
                }
                footer(&format!("{} — cancelled", section.to_lowercase()), Style::Muted);
            }
            match k {
                // The menu, as a chain: a section, then the column to put it
                // in. The letters are the sections' own initials rather than
                // positions — `h`/`l` took the positional meaning already, and
                // a key that means "left" in one row of the footer and
                // "overview" in the next is a footer nobody reads twice. `D` is
                // shifted because `d` is spoken for.
                Key::Char('o') | Key::Char('d') | Key::Char('D') => {
                    pending = Some(match k {
                        Key::Char('o') => "Overview",
                        Key::Char('d') => "Details",
                        _ => "Decisions",
                    });
                    let s = pending.unwrap().to_lowercase();
                    footer(&format!("{s} → which column? 1–{MAX_COLUMNS}"), Style::Accent);
                }
                // A digit with no section in front of it is the column count.
                // Same keys, two meanings, disambiguated by what came before —
                // which is the whole reason the chain exists rather than six
                // separate keys.
                Key::Char('1'..='9') => {
                    let msg = "no pane id".to_string();
                    footer(&msg, Style::Accent);
                }
                Key::Char('q') | Key::Interrupt => {
                    quit = true;
                }
                // Arrows say the same thing as h/l for anyone who does not think
                // in vim, and now say it by the same path — so an arrow reports
                // what it could not do, which it used to swallow.
                Key::Char('h') | Key::Char('l') | Key::Left | Key::Right => {
                    footer("no pane id", Style::Warn);
                }
                // Reading keys, in the panel's own vocabulary — see
                // `panel::run`'s `task_key`, which binds the same set on a
                // page. The same frame truncates in both places, so the same
                // gap was in both; the offset being a parameter of `frame`
                // rather than a field of the panel is what let this cost ten
                // lines rather than a second implementation.
                //
                // No wheel: this pane never turns mouse reporting on, so a
                // notch here is the terminal's own scrollback and not ours.
                // And only the vertical arrows, because `h`/`l` and `←`/`→`
                // are the arm above — they focus the pane beside this one, and
                // that meaning was here first.
                // Saturating downward too: `G` leaves `usize::MAX` for the
                // frame to clamp, and a burst of keys is drained here before
                // the next frame is drawn.
                Key::Char('j') | Key::Down => off = off.saturating_add(1),
                Key::Char('k') | Key::Up => off = off.saturating_sub(1),
                Key::Char(' ') => {
                    off = off.saturating_add(panel::term_size().1.saturating_sub(3).max(1))
                }
                Key::Char('g') | Key::Home => off = 0,
                Key::Char('G') | Key::End => off = usize::MAX,
                Key::Char('W') => {
                    let outcome = close_editors(&stuck);
                    let msg = match outcome {
                        Closing::Done(m) => {
                            stuck.clear();
                            m
                        }
                        Closing::Stuck(panes) => {
                            let names: Vec<&str> =
                                panes.iter().map(|p| p.as_str()).collect();
                            stuck = panes.clone();
                            format!("{} would not close — W again to force", names.join(" "))
                        }
                    };
                    footer(&msg, Style::Accent);
                    // Force a repaint once they are gone.
                    seen = None;
                }
                _ => {}
            }
        }
    }

    panel::stty(&["sane"]);
    print!("\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();

    // And give the entry back. The file says what this workspace is reading,
    // and the panel reads it to decide whether `↵` opens or closes — so an
    // entry outliving its reader turns open into close on the wrong row, on
    // every panel in the workspace, until something else wrote over it. A
    // reload skips this: the process replaces itself and comes back pointed at
    // the same thing, and clearing in between would be a blink of nothing-open
    // that nobody asked for. A pinned view skips it too — it never wrote the
    // workspace's entry and does not speak for it.
    if !reload && pinned.is_none() {
        super::set_focus(store, &ws, &Focus::Nothing);
    }

    if reload {
        if let Ok(exe) = std::env::current_exe() {
            let mut c = wsp_base::calls::Command::new(exe);
            c.arg("view");
            if let Some(id) = args.rest.first() {
                c.arg(id);
            }
            let err = c.exec();
            eprintln!("wsp: could not reload: {err}");
            return 1;
        }
    }
    0
}

/// One line of feedback along the bottom of the pane, over whatever is there.
///
/// This block was written out three times before the menu needed a fourth,
/// which is how `W`'s messages and the menu's came to disagree about colour.
/// One function, and the caller picks the style rather than rebuilding the
/// escape sequence.
fn footer(msg: &str, style: Style) {
    if msg.is_empty() {
        return;
    }
    let (w, _) = panel::term_size();
    let mut l = Line::default();
    l.push(style, util::truncate(msg, w));
    l.fit(w);
    print!("\x1b[999;1H{}", panel::to_ansi(&[l], w, 1).trim_start_matches("\x1b[H\x1b[2J"));
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    const SRC: &str = include_str!("run.rs");

    /// A `wsp view` pane draws raised hands and messages, so it has to refetch
    /// on them.
    ///
    /// It gated on [`crate::store::Store::fingerprint`] alone, and that stamp
    /// deliberately cannot see either: both are machine-local state, not a
    /// change to the work — the argument is on `fingerprint` itself. So a hand
    /// raised on the task in front of the reader stayed undrawn until something
    /// unrelated touched the store, which on a quiet machine is never. The
    /// panel had already learned this and reads `attention_stamp` beside its
    /// fingerprint; this pane, which renders the same records through
    /// [`super::render`], had not.
    ///
    /// Asserted against the source for the reason the panel's own tick test is:
    /// what is wrong here is *where a call is*, and a loop that owns the
    /// terminal cannot be driven from a test to find that out. Both stamps must
    /// reach the comparison, so both are checked ahead of it.
    #[test]
    fn a_view_pane_repaints_on_a_raised_hand_and_not_only_on_the_work() {
        let loop_body = SRC.split("while !quit && !reload {").nth(1).expect("the loop moved");
        let fp = loop_body.find("store.fingerprint()").expect("the work stamp is gone");
        let attention =
            loop_body.find("store.attention_stamp()").expect("the pane cannot see a raised hand");
        let gate = loop_body.find("if seen.as_ref() !=").expect("the repaint gate moved");
        assert!(
            fp < gate && attention < gate,
            "a stamp is read after the gate that decides whether to repaint, so \
             it cannot be part of that decision",
        );
        assert!(
            loop_body[..gate].contains("attention"),
            "the attention stamp is not among the things the frame was drawn from",
        );
    }
}
