//! The pane itself: paint whatever the panel is pointed at, and answer keys.

use std::io::Write;
use std::time::Duration;

use serde_json::json;

use crate::herdr;
use crate::input::{self, Key};
use crate::panel::{self, Line, Style};
use crate::resolve::Index;
use crate::store::Store;
use crate::util;

use super::editors::{
    close_editors, discard_and_quit_keys, edit_tab_siblings, editor_panes, siblings_of, Closing,
    Columns, MAX_COLUMNS,
};
use super::render::{frame, Ctx};
use super::{get_focus, Focus, Placed};

pub fn run(store: &Store, args: &crate::Args) -> i32 {
    let ws = args
        .get("workspace")
        .or_else(|| herdr::Env::read().workspace_id)
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
    // The tab closes when the last editor goes, and the count is the only
    // signal for that now the finished-editor marker is gone. Armed only once
    // editors have actually been seen, so a context pane that paints before
    // they have registered does not close the tab on the way up; and it takes
    // two consecutive empty readings, so a momentary gap in what herdr reports
    // is not mistaken for the end.
    let mut had_editors = false;
    let mut empty_readings = 0u8;
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

        // Watch for the last editor going. Cheap — one herdr call per poll, the
        // same one the menu and `W` already make.
        if let Some(me) = herdr::Env::read().pane_id {
            let n = edit_tab_siblings(&me).len();
            if n > 0 {
                had_editors = true;
                empty_readings = 0;
            } else if had_editors {
                empty_readings += 1;
                if empty_readings >= 2 {
                    if let Ok(panes) = herdr::panes() {
                        if let Some(mine) = panes.iter().find(|p| p.pane_id == me) {
                            let _ = herdr::call("tab.close", json!({ "tab_id": mine.tab_id }));
                        }
                    }
                    // `break` rather than setting `quit`: the loop condition
                    // is not consulted again, and the cleanup after it is what
                    // has to run.
                    break;
                }
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
                if let Key::Char(c @ '1'..='9') = k {
                    let msg = place_section(&focus, section, c as usize - '0' as usize);
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
                Key::Char(c @ '1'..='9') => {
                    let msg = set_columns(&focus, c as usize - '0' as usize);
                    footer(&msg, Style::Accent);
                }
                Key::Char('q') | Key::Interrupt => {
                    // In an edit tab, `q` takes the whole thing down — leaving two
                    // editors behind with the context gone is a tab that can only
                    // confuse. `W` is the one that saves; this is its opposite, and
                    // the footer says so.
                    let me = herdr::Env::read().pane_id.unwrap_or_default();
                    let editors = edit_tab_siblings(&me);
                    if !editors.is_empty() {
                        let ed = std::env::var("EDITOR").unwrap_or_default();
                        if let Some((abort, discard)) = discard_and_quit_keys(&ed) {
                            for p in &editors {
                                let _ = herdr::call(
                                    "pane.send_text",
                                    json!({ "pane_id": p.pane_id, "text": abort }),
                                );
                            }
                            std::thread::sleep(Duration::from_millis(150));
                            for p in &editors {
                                let _ = herdr::call(
                                    "pane.send_text",
                                    json!({ "pane_id": p.pane_id, "text": discard }),
                                );
                            }
                            std::thread::sleep(Duration::from_millis(600));
                        }
                        // Close the tab whether or not they went quietly: `q` is a
                        // decision to be rid of this, and a pane that would not
                        // quit must not be able to veto it.
                        if let Ok(panes) = herdr::panes() {
                            if let Some(mine) = panes.iter().find(|p| p.pane_id == me) {
                                let _ = herdr::call(
                                    "tab.close",
                                    json!({ "tab_id": mine.tab_id }),
                                );
                            }
                        }
                    }
                    quit = true;
                }
                // Arrows say the same thing as h/l for anyone who does not think
                // in vim, and now say it by the same path — so an arrow reports
                // what it could not do, which it used to swallow.
                Key::Char('h') | Key::Char('l') | Key::Left | Key::Right => {
                    let left = matches!(k, Key::Char('h') | Key::Left);
                    footer(&focus_by_position(left), Style::Warn);
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

    if reload {
        if let Ok(exe) = std::env::current_exe() {
            use std::os::unix::process::CommandExt;
            let mut c = std::process::Command::new(exe);
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

/// What the columns currently are, read back off the panes.
///
/// Deliberately not held as state in this process. The panes are the truth —
/// a pane can go because somebody quit its editor by hand, and a remembered
/// layout would then describe a column that is not there. Reading it back each
/// time costs one herdr call and cannot be wrong.
fn current_columns(me: &str) -> Columns {
    let mut cols = Columns::new(1);
    let panes = editor_panes(me);
    for (i, p) in panes.iter().enumerate() {
        if let Some(s) = crate::model::PROSE.iter().find(|s| s.eq_ignore_ascii_case(&p.label)) {
            cols.place(s, i + 1);
        }
    }
    cols.resize(panes.len().max(1));
    cols
}

/// Make the panes match `want`, and say what happened.
///
/// The whole of the menu ends here: a key press changes a [`Columns`], and this
/// is the diff between what the panes show and what it now says. Three kinds of
/// difference, in an order that matters — **grow first, then re-point, then
/// shrink** — because a section being moved out of a column that is about to
/// close must reach its new home before its old one goes.
fn apply_columns(focus: &Focus, want: &Columns) -> String {
    let Some(me) = herdr::Env::read().pane_id else {
        return "no pane id".into();
    };
    let Some(cmd) = crate::detail::edit_command(focus) else {
        return "nothing open to edit".into();
    };
    let have = editor_panes(&me);
    if have.is_empty() {
        return "no editors open — this is a view, not an edit".into();
    }
    let want = want.sections();

    // Grow. A new column is split off the rightmost pane, and starts life
    // exactly as one opened with the tab does — same label, same loop — or
    // `W`, `q` and the sibling count would all need to know the difference.
    let mut panes: Vec<String> = have.iter().map(|p| p.pane_id.clone()).collect();
    while panes.len() < want.len() {
        let Some(last) = panes.last().cloned() else { break };
        let ratio = 1.0 / (want.len() - panes.len() + 1) as f64;
        let Some(new) = herdr::call(
            "pane.split",
            json!({ "direction": "right", "target_pane_id": last, "ratio": ratio, "focus": false }),
        )
        .ok()
        .and_then(|r| Some(r.get("pane")?.get("pane_id")?.as_str()?.to_string())) else {
            return "could not split another column".into();
        };
        crate::detail::start_editor(&new, want[panes.len()], &cmd);
        panes.push(new);
    }

    // Re-point. Write the slot, then ask the editor to save and quit: its loop
    // reads the slot back, sees a different section, and opens that instead.
    let ed = std::env::var("EDITOR").unwrap_or_default();
    let keys = super::editors::save_and_quit_keys(&ed, false);
    let mut moving: Vec<&String> = Vec::new();
    for (i, section) in want.iter().enumerate() {
        let Some(pane) = panes.get(i) else { continue };
        let showing = have.iter().find(|p| &p.pane_id == pane).map(|p| p.label.clone());
        if showing.as_deref().map(|l| l.eq_ignore_ascii_case(section)).unwrap_or(true) {
            continue;
        }
        let _ = std::fs::write(super::slot_path(pane), section.to_lowercase());
        moving.push(pane);
    }

    // Shrink. Leave the slot alone and send the same keys: the loop reads back
    // what it already had, breaks, and the shell exits, which is what takes the
    // pane with it. Removing a column and re-pointing one are the same gesture
    // — only whether the slot was written first tells them apart.
    let closing: Vec<&String> = panes.iter().skip(want.len()).collect();

    let targets: Vec<&String> = moving.iter().copied().chain(closing.iter().copied()).collect();
    if targets.is_empty() {
        return String::new();
    }
    let Some((abort, commit)) = keys else {
        for p in &moving {
            let _ = std::fs::remove_file(super::slot_path(p));
        }
        return format!("don't know how to save {ed} — quit it yourself");
    };
    // Two sends, for the reason `close_editors` documents: vim throws away
    // pending type-ahead when it takes an interrupt, so the command in the same
    // write as the Ctrl-C is eaten and the editor just sits there.
    let send = |pane: &str, text: &str| {
        let _ = herdr::call("pane.send_text", json!({ "pane_id": pane, "text": text }));
    };
    for p in &targets {
        send(p, abort);
    }
    std::thread::sleep(Duration::from_millis(150));
    for p in &targets {
        send(p, &commit);
    }
    for (i, section) in want.iter().enumerate() {
        if let Some(pane) = panes.get(i) {
            let _ = herdr::call(
                "pane.rename",
                json!({ "pane_id": pane, "label": section.to_lowercase() }),
            );
        }
    }
    String::new()
}

/// `d 2` — put a section in a column.
fn place_section(focus: &Focus, section: &'static str, col: usize) -> String {
    let Some(me) = herdr::Env::read().pane_id else { return "no pane id".into() };
    let mut cols = current_columns(&me);
    cols.place(section, col);
    let msg = apply_columns(focus, &cols);
    if msg.is_empty() {
        format!("{} → {col}", section.to_lowercase())
    } else {
        msg
    }
}

/// A bare `1`, `2` or `3` — how many columns there are.
fn set_columns(focus: &Focus, n: usize) -> String {
    let Some(me) = herdr::Env::read().pane_id else { return "no pane id".into() };
    let mut cols = current_columns(&me);
    cols.resize(n);
    let msg = apply_columns(focus, &cols);
    if msg.is_empty() {
        format!("{n} column{}", if n == 1 { "" } else { "s" })
    } else {
        msg
    }
}

/// Focus the leftmost or rightmost pane below this one.
///
/// By position, not by name. Naming them — `o` for overview, `d` for details —
/// put the key for the left pane under the right hand and vice versa, which is
/// backwards every single time. `h` and `l` are left and right on the keyboard,
/// in vim, and in herdr's own `prefix+h/l`, so the same reach means the same
/// thing in all three.
///
/// The geometry comes from herdr rather than from the layout this code happens
/// to build, so it stays true if that layout ever changes.
fn focus_by_position(leftmost: bool) -> String {
    let Some(me) = herdr::Env::read().pane_id else {
        return "no pane id".into();
    };
    let sibs = siblings_of(&me);
    if sibs.is_empty() {
        return "nothing open beside this".into();
    }

    let rects = herdr::call("pane.layout", json!({ "pane_id": me }))
        .ok()
        .and_then(|r| r.get("layout").and_then(|l| l.get("panes").cloned()))
        .and_then(|p| p.as_array().cloned())
        .unwrap_or_default();
    let x_of = |id: &str| -> i64 {
        rects
            .iter()
            .find(|p| p.get("pane_id").and_then(|x| x.as_str()) == Some(id))
            .and_then(|p| p.get("rect"))
            .and_then(|r| r.get("x"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0)
    };

    let mut ordered: Vec<&herdr::Pane> = sibs.iter().collect();
    ordered.sort_by_key(|p| x_of(&p.pane_id));
    let target = if leftmost { ordered.first() } else { ordered.last() };
    match target {
        Some(p) => {
            let _ = herdr::call("pane.focus", json!({ "pane_id": p.pane_id }));
            String::new()
        }
        None => "nothing to focus".into(),
    }
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
