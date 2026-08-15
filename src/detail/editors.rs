//! Closing the editors a pop-out opened.
//!
//! An editor is another program in another pane, and the only thing we can say
//! to it is keystrokes. So this is a small vocabulary of what each one wants to
//! hear, and the care needed to make it listen.

use std::time::Duration;

use serde_json::json;

use crate::herdr;

/// What to send an editor to make it save and quit.
///
/// Driving another program by keystroke is a guess, so the guess is written
/// down where it can be corrected rather than buried. `vi` is the default
/// because that is what an unset `$EDITOR` means.
///
/// Each sequence leads with that editor's "abort whatever is pending", which
/// is doing more work than it looks like. `q:` opens vim's command-line
/// window, and `Esc` does not close it — the buffer just takes `:wq` as text.
/// `Ctrl-C` leaves it, and also drops insert mode, a half-typed `:` command, a
/// pending operator and a "Press ENTER" prompt.
///
/// The quit is `wqa`, not `wq`: someone who has split inside their editor
/// would otherwise close one window and leave the pane sitting there.
pub(super) fn save_and_quit_keys(editor: &str, force: bool) -> Option<(&'static str, String)> {
    let name = editor
        .rsplit('/')
        .next()
        .unwrap_or(editor)
        .split_whitespace()
        .next()
        .unwrap_or("");
    let bang = if force { "!" } else { "" };
    match name {
        "" | "vi" | "vim" | "nvim" | "view" | "hx" | "helix" => {
            Some(("\x03", format!("\x1b:wqa{bang}\r")))
        }
        "nano" | "pico" => Some(("\x03", "\x0f\r\x18".into())),
        "emacs" | "emacsclient" => Some(("\x07", "\x18\x13\x18\x03".into())),
        "micro" => Some(("\x1b", "\x13\x11".into())),
        _ => None,
    }
}


/// What to send an editor to make it leave *without* saving — the counterpart
/// to `save_and_quit_keys`, and what `q` means once there is a `W` that saves.
pub(super) fn discard_and_quit_keys(editor: &str) -> Option<(&'static str, &'static str)> {
    let name = editor
        .rsplit('/')
        .next()
        .unwrap_or(editor)
        .split_whitespace()
        .next()
        .unwrap_or("");
    match name {
        "" | "vi" | "vim" | "nvim" | "view" | "hx" | "helix" => Some(("\x03", "\x1b:qa!\r")),
        // nano asks; `n` is the answer to "save modified buffer?".
        "nano" | "pico" => Some(("\x03", "\x18n")),
        "emacs" | "emacsclient" => Some(("\x07", "\x18\x03n\r")),
        "micro" => Some(("\x1b", "\x11n")),
        _ => None,
    }
}

/// The panes this view shares a tab with, and whether they are editors we put
/// there. A view opened from the sidebar shares its tab with the panel and
/// whatever you were working in — closing *that* tab would take the sidebar
/// and your work pane with it, which is emphatically not what `q` means.
///
/// The test is the section vocabulary rather than two names, so a pane holding
/// `decisions` is as much an editor as one holding `overview`. Naming them
/// literally is what left the menu's third section out of every check that
/// matters — `q` would have walked past it and closed the tab around it.
pub(super) fn edit_tab_siblings(me: &str) -> Vec<herdr::Pane> {
    siblings_of(me).into_iter().filter(|p| is_section_label(&p.label)).collect()
}

/// Whether a pane label names an editable section.
pub(crate) fn is_section_label(label: &str) -> bool {
    crate::model::PROSE.iter().any(|s| s.eq_ignore_ascii_case(label))
}

/// Where an editor pane is told which section to open next.
///
/// One file per pane, holding a section name. The pane's shell loop writes the
/// section it is about to open and reads the file again when the editor exits:
/// unchanged means the person really quit, changed means the menu re-pointed
/// the pane while they were in it. That indirection is what lets the top pane
/// swap a section in without knowing anything about `$EDITOR` beyond how to
/// ask it to leave — and it is why a swap does not trip the tab-closing
/// marker, which only sees a loop that ran out.
pub(crate) fn slot_path(pane_id: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("wsp-slot-{}", pane_id.replace(':', "-")))
}

/// The outcome of asking the editors to leave.
pub(super) enum Closing {
    /// Everything asked to go has gone, or there was nothing to ask.
    Done(String),
    /// These panes did not close. A second `W` forces them.
    Stuck(Vec<String>),
}

pub(super) fn siblings_of(me: &str) -> Vec<herdr::Pane> {
    let Ok(panes) = herdr::panes() else { return Vec::new() };
    let Some(mine) = panes.iter().find(|p| p.pane_id == me) else { return Vec::new() };
    let tab = mine.tab_id.clone();
    panes.into_iter().filter(|p| p.tab_id == tab && p.pane_id != me).collect()
}

/// Tell every editor sharing this tab to save and quit, and check that they
/// did. The tab closes itself once the second one exits — that machinery
/// already exists — so this only has to get them to leave.
pub(super) fn close_editors(force_these: &[String]) -> Closing {
    let env = herdr::Env::read();
    let Some(me) = env.pane_id else {
        return Closing::Done("no pane id — cannot find the editors".into());
    };

    // A second W after a stuck one: the user has now said it twice, so take
    // the panes down and accept losing whatever would not save.
    if !force_these.is_empty() {
        let mut gone = 0;
        for p in force_these {
            if herdr::call("pane.close", json!({ "pane_id": p })).is_ok() {
                gone += 1;
            }
        }
        return Closing::Done(format!("forced {gone} closed"));
    }

    let editor = std::env::var("EDITOR").unwrap_or_default();
    let targets: Vec<String> = siblings_of(&me).into_iter().map(|p| p.pane_id).collect();
    if targets.is_empty() {
        return Closing::Done("nothing open beside this".into());
    }
    let Some((abort, commit)) = save_and_quit_keys(&editor, false) else {
        return Closing::Done(format!("don't know how to save {editor} — quit it yourself"));
    };

    let send = |panes: &[String], text: &str| {
        for p in panes {
            let _ = herdr::call("pane.send_text", json!({ "pane_id": p, "text": text }));
        }
    };
    let still_open = |want: &[String]| -> Vec<String> {
        let live: Vec<String> = siblings_of(&me).into_iter().map(|p| p.pane_id).collect();
        want.iter().filter(|p| live.contains(p)).cloned().collect()
    };

    // Two sends, not one. Vim throws away pending type-ahead when it takes an
    // interrupt, so a Ctrl-C and the command in the same write means the
    // command is eaten and the editor just sits there — which is exactly the
    // stuck pane this was meant to prevent.
    send(&targets, abort);
    std::thread::sleep(Duration::from_millis(150));
    send(&targets, &commit);
    std::thread::sleep(Duration::from_millis(900));
    let left = still_open(&targets);
    if left.is_empty() {
        return Closing::Done(format!("saved {}", targets.len()));
    }

    // Something refused. Most often a buffer vim will not write without a
    // bang, or a modal it wanted an answer to. Try once harder before giving
    // up, since a stuck pane with no explanation is the worst outcome.
    if let Some((abort, forced)) = save_and_quit_keys(&editor, true) {
        send(&left, abort);
        std::thread::sleep(Duration::from_millis(150));
        send(&left, &forced);
        std::thread::sleep(Duration::from_millis(900));
    }
    let left = still_open(&targets);
    if left.is_empty() {
        Closing::Done("saved".into())
    } else {
        Closing::Stuck(left)
    }
}
