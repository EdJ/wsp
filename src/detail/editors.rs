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

/// Which section each editor column is showing, left to right.
///
/// The rules of the menu live here, apart from the panes they act on, because
/// everything else in this feature needs a terminal and a herdr to exercise and
/// this does not. What a key press means is a function from one of these to the
/// next; moving panes around afterwards is a diff between the two, and a diff
/// has no opinions to get wrong.
///
/// The invariant the whole thing rests on: **no section appears twice.** Two
/// columns on one section means two editors writing one buffer, and `wsp edit`
/// re-reads and writes back a whole section, so the second one to exit wins and
/// the first person's typing is gone. Every operation below preserves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Columns(Vec<&'static str>);

impl Columns {
    /// The first `n` sections, which is the layout an edit tab opens with.
    pub(crate) fn new(n: usize) -> Columns {
        Columns(crate::model::PROSE.iter().copied().take(n.clamp(1, MAX_COLUMNS)).collect())
    }

    pub(crate) fn sections(&self) -> &[&'static str] {
        &self.0
    }

    /// The first section not on screen, for a column that needs filling.
    fn missing(&self) -> Option<&'static str> {
        crate::model::PROSE.iter().copied().find(|s| !self.0.contains(s))
    }

    /// Put `section` in column `col`, counting from 1.
    ///
    /// Asking for a column past the end grows the layout to reach it, because
    /// `D 3` from a two-column tab is a clear enough instruction that refusing
    /// it and making the user press `3` first would be pedantry.
    ///
    /// If the section is already on screen the two columns **trade** rather
    /// than the target being overwritten. Overwriting would drop a section
    /// nobody asked to lose and leave the one you moved sitting in two places
    /// at once — which is the duplicate the invariant forbids.
    pub(crate) fn place(&mut self, section: &'static str, col: usize) {
        let col = col.clamp(1, MAX_COLUMNS);
        while self.0.len() < col {
            match self.missing() {
                Some(s) => self.0.push(s),
                None => break,
            }
        }
        let Some(to) = col.checked_sub(1).filter(|i| *i < self.0.len()) else { return };
        match self.0.iter().position(|s| *s == section) {
            Some(from) if from == to => {}
            Some(from) => self.0.swap(from, to),
            None => self.0[to] = section,
        }
    }

    /// Show exactly `n` columns, keeping what is already placed.
    ///
    /// Growing appends the sections that are not on screen, in their canonical
    /// order; shrinking drops from the right. Keeping the left-hand columns
    /// still means `1` then `3` returns you to something recognisable rather
    /// than to the default — the overview you were writing in stays where it
    /// was, and only the columns you asked about move.
    pub(crate) fn resize(&mut self, n: usize) {
        let n = n.clamp(1, MAX_COLUMNS);
        self.0.truncate(n);
        while self.0.len() < n {
            match self.missing() {
                Some(s) => self.0.push(s),
                None => break,
            }
        }
    }
}

/// One column per editable section, and no more: a fourth would have nothing
/// to show.
pub(crate) const MAX_COLUMNS: usize = crate::model::PROSE.len();

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

/// The shell an editor pane runs: open a section, and open it again if the
/// menu moved this pane while somebody was inside it.
///
/// A loop rather than one editor, so the context pane can re-point this one
/// without closing anything. The pane writes the section it is opening into
/// its slot, runs the editor, then reads the slot back. Unchanged means the
/// person quit and the loop ends, taking the pane with it; changed means the
/// menu moved it, and the next turn opens what it now says.
///
/// That one difference is the whole vocabulary the context pane needs. To
/// **re-point** a column, write the slot and ask the editor to save and quit.
/// To **remove** a column, ask the editor to save and quit and leave the slot
/// alone. Same keystrokes, opposite outcomes, and neither is a special case of
/// the other in the code that sends them.
///
/// Two things this shape is carrying that are not obvious from reading it:
///
/// - **Nothing here closes the tab.** It used to: a marker file counted
///   editors that had finished and the second one took the tab down. That
///   hard-codes two panes into a shell fragment, and the moment the column
///   count could change, going from three columns to two counted as an editor
///   finishing and closed the tab underneath you. The context pane watches the
///   sibling count instead, which does not care how many columns there are.
/// - **An empty read ends the loop.** The slot can go missing — a temp reaper,
///   a forced close, a second `W` — and an empty section name would otherwise
///   become `wsp edit <id> --`, which parses as no section at all and puts the
///   whole body in front of someone who asked for one part of it.
/// - **The slot is written at the top of each turn, not the bottom.** The menu
///   writes it while the editor is running, so reading it after the editor
///   exits is the only ordering that sees the change.
///
/// Built as a string, by a function of its own, so it can be handed to `sh -n`
/// in a test. Nothing else in this file works without a terminal and a herdr,
/// which is exactly why the one part that is a quoting problem should be
/// checked by something.
pub(crate) fn editor_loop(section: &str, cmd: &str, slot: &str) -> String {
    format!(
        "s={section}; while :; do printf %s \"$s\" > {slot}; \
         {cmd} --\"$s\"; n=$(cat {slot} 2>/dev/null); \
         {{ [ -z \"$n\" ] || [ \"$n\" = \"$s\" ]; }} && break; s=\"$n\"; done; \
         rm -f {slot}"
    )
}

/// The editor panes beside this one, left to right.
///
/// Order comes from herdr's geometry, not from the order it lists panes in.
/// The list order is creation order, and it stops meaning "left to right" the
/// first time a column is closed and another opened — which, with a menu that
/// adds and removes columns, is immediately.
pub(crate) fn editor_panes(me: &str) -> Vec<herdr::Pane> {
    let xs = pane_xs(me);
    let mut panes = edit_tab_siblings(me);
    panes.sort_by_key(|p| xs.get(&p.pane_id).copied().unwrap_or(0));
    panes
}

/// Every pane's x in this tab, for ordering siblings left to right.
pub(crate) fn pane_xs(me: &str) -> std::collections::BTreeMap<String, i64> {
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


/// The sections the editor columns are showing, left to right.
///
/// What the menu draws. Ordered by geometry for the same reason the panes are:
/// "column 2" has to mean the second one across, or `d 2` puts details
/// somewhere the user did not point at.
pub(crate) fn editor_columns() -> Vec<String> {
    let Some(me) = herdr::Env::read().pane_id else { return Vec::new() };
    editor_panes(&me).into_iter().map(|p| p.label).collect()
}

/// Label a pane for its section and start the loop in it.
///
/// Both the panel opening an edit tab and the menu adding a column need this,
/// and they need it identically — a column added later must be
/// indistinguishable from one opened with the tab, or `W`, `q` and the sibling
/// count all start having exceptions.
///
/// The label is half of how you know where you are: herdr shows it, and the
/// editor's own status line shows the buffer's filename, which `wsp edit`
/// names for the section. Between the two there is no way to be typing into a
/// buffer without knowing which section it is.
pub(crate) fn start_editor(pane: &str, section: &str, cmd: &str) {
    let section = section.to_lowercase();
    let _ = herdr::call("pane.rename", json!({ "pane_id": pane, "label": section }));
    let path = slot_path(pane);
    let _ = std::fs::remove_file(&path);
    let slot = crate::util::shell_quote(&path.display().to_string());
    let _ = herdr::call(
        "pane.send_text",
        json!({ "pane_id": pane, "text": format!("{}\n", editor_loop(&section, cmd, &slot)) }),
    );
}

/// The `wsp` invocation an editor pane runs, already quoted.
///
/// Rebuilt from the focus rather than passed down from whoever opened the tab,
/// because the menu can add a column long after the panel that opened it has
/// forgotten the arguments. A task and a project reach `edit_prose` by
/// different subcommands, and that is the only difference between them here.
pub(crate) fn edit_command(focus: &super::Focus) -> Option<String> {
    let exe = std::env::current_exe().ok()?.display().to_string();
    let argv: Vec<String> = match focus {
        super::Focus::Task(id) => vec!["edit".into(), id.clone()],
        super::Focus::Project(id) => vec!["project".into(), "edit".into(), id.clone()],
        super::Focus::Nothing => return None,
    };
    Some(
        std::iter::once(crate::util::shell_quote(&exe))
            .chain(argv.iter().map(|a| crate::util::shell_quote(a)))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(c: &Columns) -> Vec<&str> {
        c.sections().to_vec()
    }

    #[test]
    fn a_tab_opens_on_the_first_n_sections() {
        assert_eq!(secs(&Columns::new(1)), ["Overview"]);
        assert_eq!(secs(&Columns::new(2)), ["Overview", "Details"]);
        assert_eq!(secs(&Columns::new(3)), ["Overview", "Details", "Decisions"]);
    }

    /// `d 2` on the default layout is a no-op, and must not become a swap with
    /// itself or a duplicate.
    #[test]
    fn placing_a_section_where_it_already_is_changes_nothing() {
        let mut c = Columns::new(2);
        c.place("Details", 2);
        assert_eq!(secs(&c), ["Overview", "Details"]);
    }

    /// The case the rule exists for. Overwriting column 1 would leave Details
    /// in both columns and drop Overview, which is two editors on one buffer.
    #[test]
    fn placing_a_section_that_is_already_on_screen_trades_columns() {
        let mut c = Columns::new(2);
        c.place("Details", 1);
        assert_eq!(secs(&c), ["Details", "Overview"]);
    }

    #[test]
    fn placing_a_section_that_is_not_on_screen_replaces_the_occupant() {
        let mut c = Columns::new(2);
        c.place("Decisions", 2);
        assert_eq!(secs(&c), ["Overview", "Decisions"]);
    }

    /// `D 3` from a two-column tab. Refusing and making the user press `3`
    /// first would be pedantry — the instruction is unambiguous.
    #[test]
    fn placing_past_the_end_grows_to_reach_it() {
        let mut c = Columns::new(2);
        c.place("Decisions", 3);
        assert_eq!(secs(&c), ["Overview", "Details", "Decisions"]);
    }

    /// Growing to reach a column has to fill the ones it passes over, and the
    /// filler must not be the section being placed or it lands twice.
    #[test]
    fn growing_to_reach_a_column_fills_the_gap_without_duplicating() {
        let mut c = Columns::new(1);
        c.place("Decisions", 3);
        assert_eq!(secs(&c), ["Overview", "Details", "Decisions"]);
        assert_no_duplicates(&c);
    }

    #[test]
    fn resizing_keeps_the_left_hand_columns() {
        let mut c = Columns::new(3);
        c.place("Decisions", 1);
        assert_eq!(secs(&c), ["Decisions", "Details", "Overview"]);
        c.resize(1);
        assert_eq!(secs(&c), ["Decisions"], "the column you were in survives");
        c.resize(3);
        assert_eq!(secs(&c), ["Decisions", "Overview", "Details"], "and the rest come back");
    }

    #[test]
    fn there_is_no_fourth_column_and_no_zeroth() {
        let mut c = Columns::new(2);
        c.resize(9);
        assert_eq!(c.sections().len(), MAX_COLUMNS);
        c.resize(0);
        assert_eq!(c.sections().len(), 1);
        c.place("Details", 0);
        assert_eq!(secs(&c), ["Details"], "column 0 means the first one");
    }

    /// The invariant everything else rests on, exercised over every reachable
    /// one-step move rather than the handful a person thinks to write down.
    /// Two columns on one section means two editors on one buffer, and the
    /// second to exit wins.
    #[test]
    fn no_sequence_of_moves_puts_a_section_in_two_columns() {
        for start in 1..=MAX_COLUMNS {
            for section in crate::model::PROSE {
                for col in 0..=MAX_COLUMNS + 1 {
                    let mut c = Columns::new(start);
                    c.place(section, col);
                    assert_no_duplicates(&c);
                    for n in 0..=MAX_COLUMNS + 1 {
                        let mut c2 = c.clone();
                        c2.resize(n);
                        assert_no_duplicates(&c2);
                    }
                }
            }
        }
    }

    fn assert_no_duplicates(c: &Columns) {
        let mut seen = c.sections().to_vec();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before, "a section is in two columns: {:?}", c.sections());
    }

    /// The one thing here that is a quoting problem rather than a terminal
    /// problem. `sh -n` parses without running, so a stray brace or an
    /// unbalanced quote fails in a test rather than in a pane at the moment
    /// somebody presses `E`.
    #[test]
    fn the_editor_loop_is_valid_shell() {
        let l = editor_loop("overview", "'/usr/bin/wsp' 'edit' 't-1'", "'/tmp/s'");
        let out = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(&l)
            .output()
            .expect("sh");
        assert!(
            out.status.success(),
            "sh -n rejected the loop: {}\n{l}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A missing slot must end the loop, not feed it an empty section — that
    /// becomes `wsp edit <id> --`, which parses as no section at all and opens
    /// the whole body. Easy to drop when editing the format string, and
    /// nothing else would notice.
    #[test]
    fn an_empty_slot_ends_the_loop_rather_than_reopening_on_nothing() {
        let l = editor_loop("overview", "true", "'/tmp/s'");
        assert!(l.contains("[ -z \"$n\" ]"), "no empty-slot guard: {l}");
        let editor = l.find("--\"$s\"").expect("runs the editor");
        let read = l.find("n=$(cat").expect("reads the slot back");
        assert!(editor < read, "slot is read before the editor runs: {l}");
    }

    /// The loop actually run, driven by a fake editor, rather than a copy of
    /// it in a shell script. Both gestures the context pane makes are exercised
    /// here because they differ only in whether the slot was written first, and
    /// that is exactly the kind of distinction that survives review and fails in
    /// a pane.
    #[test]
    fn the_loop_reopens_on_a_written_slot_and_ends_on_an_untouched_one() {
        let dir = std::env::temp_dir().join(format!("wsp-loop-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let slot = dir.join("slot");
        let log = dir.join("log");
        let _ = std::fs::remove_file(&log);

        // A stand-in for `wsp edit`: records the section it was opened on, and
        // the first time round writes a new one into the slot — which is what
        // the menu does while the editor is up.
        let fake = format!(
            "sh -c 'printf \"%s\\n\" \"$1\" >> {log}; \
             [ \"$(wc -l < {log} | tr -d \" \")\" -eq 1 ] && printf decisions > {slot}; true' --",
            log = log.display(),
            slot = slot.display()
        );
        let script = editor_loop(
            "overview",
            &fake,
            &crate::util::shell_quote(&slot.display().to_string()),
        );
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("sh");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let opened = std::fs::read_to_string(&log).unwrap_or_default();
        let opened: Vec<&str> = opened.lines().collect();
        assert_eq!(
            opened,
            ["--overview", "--decisions"],
            "a written slot reopens the pane on the new section, an untouched one ends it"
        );
        assert!(!slot.exists(), "the loop cleans its slot up on the way out");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The loop must end by itself so the shell exits and herdr reaps the
    /// pane. That is what makes removing a column and re-pointing one the same
    /// operation, and it is what the sibling-count watch depends on.
    #[test]
    fn the_loop_closes_nothing_itself() {
        let l = editor_loop("overview", "true", "'/tmp/s'");
        assert!(!l.contains("tab close"), "the loop must not close the tab: {l}");
        assert!(!l.contains("wc -c"), "the finished-editor marker is gone: {l}");
    }
}
