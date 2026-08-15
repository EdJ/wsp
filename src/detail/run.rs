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

use super::editors::{close_editors, discard_and_quit_keys, edit_tab_siblings, siblings_of, Closing};
use super::render::{frame, Ctx};
use super::{get_focus, Focus};

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
    let started_as = panel::exe_stamp();
    let mut last = String::new();
    let mut seen: Option<(Focus, u64)> = None;
    let mut quit = false;
    let mut reload = false;
    // Panes a previous W could not get rid of. A second W closes them outright.
    let mut stuck: Vec<String> = Vec::new();
    let Ok(mut tty) = std::fs::File::open("/dev/tty") else { return 1 };
    let mut keys = input::Keys::new();
    let mut pressed: Vec<Key> = Vec::new();

    while !quit && !reload {
        if started_as.is_some() && panel::exe_stamp() != started_as {
            reload = true;
            break;
        }
        let focus = pinned.clone().unwrap_or_else(|| get_focus(store, &ws));
        let fp = store.fingerprint();
        if seen.as_ref() != Some(&(focus.clone(), fp)) {
            seen = Some((focus.clone(), fp));
            let (w, h) = panel::term_size();
            let ctx = Ctx::live(store);
            let painted = panel::to_ansi(&frame(&ctx, &focus, w, h), w, h);
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
            match k {
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
                    let msg = focus_by_position(left);
                    if !msg.is_empty() {
                        let (w, _) = panel::term_size();
                        let mut l = Line::default();
                        l.push(Style::Warn, util::truncate(&msg, w));
                        l.fit(w);
                        print!("\x1b[999;1H{}", panel::to_ansi(&[l], w, 1).trim_start_matches("\x1b[H\x1b[2J"));
                        let _ = std::io::stdout().flush();
                    }
                }
                // The menu. One key per section, and the letters are the
                // sections' own initials rather than positions — `h`/`l` took
                // the positional meaning already, and a key that means "left"
                // in one row of the footer and "overview" in the next is a
                // footer nobody reads twice. `D` is shifted because `d` is
                // spoken for and details is the one you reach for more often.
                Key::Char('o') | Key::Char('d') | Key::Char('D') => {
                    let want = match k {
                        Key::Char('o') => "overview",
                        Key::Char('d') => "details",
                        _ => "decisions",
                    };
                    let msg = show_section(want);
                    if !msg.is_empty() {
                        let (w, _) = panel::term_size();
                        let mut l = Line::default();
                        l.push(Style::Accent, util::truncate(&msg, w));
                        l.fit(w);
                        print!("\x1b[999;1H{}", panel::to_ansi(&[l], w, 1).trim_start_matches("\x1b[H\x1b[2J"));
                        let _ = std::io::stdout().flush();
                    }
                }
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
                    let (w, _) = panel::term_size();
                    let mut l = Line::default();
                    l.push(Style::Accent, util::truncate(&msg, w));
                    l.fit(w);
                    print!("\x1b[999;1H{}", panel::to_ansi(&[l], w, 1).trim_start_matches("\x1b[H\x1b[2J"));
                    let _ = std::io::stdout().flush();
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

/// Put a section into the editor panes, and say what happened.
///
/// Two panes, three sections, so a key has to mean something definite about
/// which pane moves. The rule is one sentence: **the section you press lands on
/// the right, and if it is already on the left the two trade places.** That
/// reaches every arrangement, it never disturbs a pane that is already showing
/// what you asked for, and the common case — swapping `details` out for
/// `decisions` while you keep writing the overview — leaves the pane you are
/// working in alone.
///
/// The swap goes through the slot file and the editor's own save-and-quit, not
/// through closing the pane. Closing it would lose the shell that owns the
/// tab-closing marker, and re-splitting would move every pane on screen while
/// somebody is typing in one of them.
fn show_section(want: &str) -> String {
    let Some(me) = herdr::Env::read().pane_id else {
        return "no pane id".into();
    };
    let editors = edit_tab_siblings(&me);
    if editors.is_empty() {
        return "no editors open — this is a view, not an edit".into();
    }

    // Left and right by where they actually are, not by the order herdr lists
    // them. `focus_by_position` learned the same lesson: the layout is the
    // authority on which pane is on the left, and the order panes were created
    // in stops being that the first time one is closed.
    //
    // The layout is fetched once and sorted against, not asked for inside the
    // comparator — a round-trip per comparison is a socket call in a sort.
    let xs = pane_xs(&me);
    let mut ordered: Vec<&herdr::Pane> = editors.iter().collect();
    ordered.sort_by_key(|p| xs.get(&p.pane_id).copied().unwrap_or(0));
    let (Some(left), Some(right)) = (ordered.first(), ordered.last()) else {
        return "could not place the editors".into();
    };
    if left.pane_id == right.pane_id {
        return "only one editor open".into();
    }

    if right.label.eq_ignore_ascii_case(want) {
        return format!("{want} is already there");
    }
    // Already on the left: trade, so the pane that was on the right is not
    // simply overwritten with a section that is on screen twice.
    let pairs: Vec<(&herdr::Pane, String)> = if left.label.eq_ignore_ascii_case(want) {
        vec![(left, right.label.clone()), (right, want.to_string())]
    } else {
        vec![(right, want.to_string())]
    };

    // Write every slot before asking any editor to leave. An editor that exits
    // between the two steps would re-open on the section it already had, and
    // the trade would come apart with one pane moved and one not.
    for (pane, section) in &pairs {
        let _ = std::fs::write(super::slot_path(&pane.pane_id), section);
    }

    let ed = std::env::var("EDITOR").unwrap_or_default();
    let Some((abort, commit)) = super::editors::save_and_quit_keys(&ed, false) else {
        for (pane, _) in &pairs {
            let _ = std::fs::remove_file(super::slot_path(&pane.pane_id));
        }
        return format!("don't know how to save {ed} — quit it yourself");
    };
    let send = |pane: &str, text: &str| {
        let _ = herdr::call("pane.send_text", json!({ "pane_id": pane, "text": text }));
    };
    // Two sends, for the reason `close_editors` documents: vim throws away
    // pending type-ahead when it takes an interrupt, so the command in the
    // same write as the Ctrl-C is eaten.
    for (pane, _) in &pairs {
        send(&pane.pane_id, abort);
    }
    std::thread::sleep(Duration::from_millis(150));
    for (pane, _) in &pairs {
        send(&pane.pane_id, &commit);
    }
    for (pane, section) in &pairs {
        let _ = herdr::call(
            "pane.rename",
            json!({ "pane_id": pane.pane_id, "label": section }),
        );
    }
    format!("{want} →")
}

/// Every pane's x in this tab, for ordering siblings left to right.
fn pane_xs(me: &str) -> std::collections::BTreeMap<String, i64> {
    herdr::call("pane.layout", json!({ "pane_id": me }))
        .ok()
        .and_then(|r| r.get("layout").and_then(|l| l.get("panes").cloned()))
        .and_then(|p| p.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| {
            let id = p.get("pane_id")?.as_str()?.to_string();
            let x = p.get("rect")?.get("x")?.as_i64()?;
            Some((id, x))
        })
        .collect()
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
