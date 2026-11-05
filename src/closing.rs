//! Closing a leg robustly. `wsp-372`.
//!
//! Seen 2026-11-04 on workspace-port: a leg finished at 17:01Z, and its closing
//! command chained a bare `cat > $TMPDIR/baton.txt` (no input) before `wsp leg
//! done`. The `cat` waited on stdin for 3h46m, `leg done` never ran, and every
//! leg behind it waited. One stuck close stalls a whole list.
//!
//! # What this module is
//!
//! - **A baton at a place wsp knows** ([`baton_path`]): a leg's seat has
//!   `$WSP_BATON` set, `wsp leg done` reads it when `--from` is not given, and
//!   the orders say to write it with a heredoc and to run the closing verb as
//!   its own Bash call ([`how_to_close`]).
//! - **Noticing** ([`tick`]): a leg that is `running` while its seat has sat
//!   idle (or has a baton written and nothing after it) for [`TELL_AFTER`] is
//!   told once, with the exact command. If it is still open [`SECOND_WINDOW`]
//!   later, wsp closes it itself when that is safe, and flags the person when
//!   it is not.
//! - **Closing for the seat** ([`settle`]): safe means the baton is written and
//!   the tree is a clean tree at a commit and the ordinary `leg done` checks
//!   pass. The caption is the baton's first line, the leg is timed to the
//!   baton's write ([`written_at`]), and the line says `closed by wsp: seat
//!   idle Nm, baton written at HH:MMZ`. Nothing else is guessed: no baton, a
//!   dirty tree, a refused close are all a flag to the person, at once.
//! - **A session that ended with its leg open** is settled on the tick that
//!   sees it, with no idle timer.
//!
//! # Not here
//!
//! A member's `wsp review`, a verifier's `wsp verified` and a check's verdict are
//! closes of the same kind. A member that sits idle at `doing` is already told
//! and then ended by [`crate::repair`]'s `gone_member`; a verifier's and a
//! check's verdict file are not given a known path yet.

use crate::cycle::Seats;
use crate::legs::{self, State as LegState};
use crate::model::Status;
use crate::place::State;
use crate::store::Store;
use crate::util;
use serde_json::{json, Value};
use std::path::PathBuf;

/// How long a seat sits idle with its leg open before it is told, in seconds.
pub(crate) const TELL_AFTER: i64 = 3 * 60;

/// How long after the tell the leg may still be open before wsp acts, in seconds.
pub(crate) const SECOND_WINDOW: i64 = 5 * 60;

/// A close this long after the baton was written is called late.
pub(crate) const LATE_AFTER: i64 = 2 * 60;

/// Where leg `leg` of `row` writes its baton, and where `wsp leg done` reads it
/// from when `--from` is not given. Under the state directory, keyed by row and
/// leg, so a seat invents no path of its own.
pub(crate) fn baton_path(store: &Store, row: &str, leg: &str) -> PathBuf {
    store.state.join("batons").join(row).join(format!("{leg}.md"))
}

/// When a file was last written, epoch seconds.
pub(crate) fn written_at(path: &std::path::Path) -> Option<i64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    m.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

/// `HH:MMZ` of an epoch second.
fn clock(secs: i64) -> String {
    util::iso_at(secs).get(11..16).map(|c| format!("{c}Z")).unwrap_or_default()
}

/// The clause a late close adds to a leg's end line and to the timeline.
pub(crate) fn late_clause(baton_at: i64, now: i64, by_wsp: Option<&str>) -> String {
    match by_wsp {
        Some(why) => format!(" · closed by wsp: {why}, baton written at {}", clock(baton_at)),
        None => format!(" · closed late: done at {}, closed at {}", clock(baton_at), clock(now)),
    }
}

/// The line a glance reads, from a baton: its first line with the markdown that
/// leads a heading or a bullet taken off, cut to fit.
pub(crate) fn caption_of(baton: &str) -> String {
    let first = baton.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let first = first.trim_start_matches(['#', '-', '*', '>', ' ']);
    util::truncate(&crate::cmd_task::fold(first), 100)
}

/// The words every order about closing carries (wsp-372): its own call, never
/// chained, and the baton from a heredoc into `$WSP_BATON`.
pub(crate) fn how_to_close(task: &str, last: bool) -> String {
    let verb = match last {
        true => format!("`wsp review {task} \"...\"`"),
        false => format!("`wsp leg done {task} --caption \"...\"`"),
    };
    format!(
        "Close it in two separate Bash calls, never chained with anything else: first write the baton, \
         `cat > \"$WSP_BATON\" <<'EOF'` ... `EOF` (a heredoc with its text, never a bare `cat`); then, as a call of its own, {verb} \
         (it reads $WSP_BATON). A close chained after another command waits on that command, and nobody sees it wait."
    )
}

// ---- the record -------------------------------------------------------------------

/// What the tick has seen of one open leg: when its seat was first read idle,
/// when it was told, and whether the person was flagged.
#[derive(Debug, Default, Clone, PartialEq)]
struct Seen {
    idle_since: Option<i64>,
    told: Option<i64>,
    flagged: bool,
}

fn record_path(store: &Store) -> PathBuf {
    store.state.join("closing.json")
}

fn read(store: &Store) -> std::collections::BTreeMap<String, Seen> {
    let v: Value = std::fs::read_to_string(record_path(store)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut out = std::collections::BTreeMap::new();
    for (k, e) in v.as_object().into_iter().flatten() {
        out.insert(
            k.clone(),
            Seen {
                idle_since: e["idle_since"].as_i64(),
                told: e["told"].as_i64(),
                flagged: e["flagged"].as_bool().unwrap_or(false),
            },
        );
    }
    out
}

fn write(store: &Store, seen: &std::collections::BTreeMap<String, Seen>) {
    let v: serde_json::Map<String, Value> = seen
        .iter()
        .map(|(k, s)| (k.clone(), json!({ "idle_since": s.idle_since, "told": s.told, "flagged": s.flagged })))
        .collect();
    let _ = std::fs::create_dir_all(&store.state);
    let _ = std::fs::write(record_path(store), Value::Object(v).to_string());
}

// ---- the tick ---------------------------------------------------------------------

pub(crate) fn tick(store: &Store, seats: &dyn Seats) {
    tick_at(store, seats, util::epoch_secs());
}

/// One pass over every running leg of a row at `doing`.
pub(crate) fn tick_at(store: &Store, seats: &dyn Seats, now: i64) {
    let mut seen = read(store);
    let before = seen.clone();
    let mut open = std::collections::BTreeSet::new();
    let mut asks = None;
    for t in store.tasks_shared().iter().filter(|t| t.status() == Status::Doing && legs::staged(t)) {
        for l in legs::legs(t).into_iter().filter(|l| l.state == LegState::Running) {
            let Some(seat) = l.pane.clone() else { continue };
            let key = format!("{}/{}", t.id, l.name);
            open.insert(key.clone());
            let entry = seen.entry(key).or_default();
            let baton = written_at(&baton_path(store, &t.id, &l.name));
            let state = seats.state(&seat);
            // A session that ended with its leg open is not waited on.
            if seats.absent(&seat) || matches!(state, Some(State::Gone | State::Empty)) {
                if !entry.flagged {
                    let why = Why::Ended;
                    entry.flagged = settle(store, &t.id, &l.name, &seat, baton, why, now);
                }
                continue;
            }
            let since = match state {
                Some(State::Idle) => {
                    // A question it asked, or a prompt, is a wait the leg is
                    // owed an answer for; the shells it has running are not,
                    // because a stuck shell is the very thing this is for.
                    let asks = asks.get_or_insert_with(|| crate::waiting::Asks::read(store));
                    if crate::waiting::reading(Some(State::Idle), asks, &crate::waiting::Shells::default(), &seat, &t.id).is_some() {
                        *entry = Seen::default();
                        continue;
                    }
                    Some(*entry.idle_since.get_or_insert(now))
                }
                // A tool is running and has been since the baton was written:
                // the close is what it is stuck on.
                Some(State::Working) => {
                    entry.idle_since = None;
                    baton
                }
                _ => None,
            };
            let Some(since) = since else {
                *entry = Seen::default();
                continue;
            };
            if since > now - TELL_AFTER {
                continue;
            }
            match entry.told {
                None => {
                    entry.told = Some(now);
                    tell_to_close(store, &t.id, &l.name, &seat, (now - since) / 60);
                }
                Some(told) if now - told >= SECOND_WINDOW && !entry.flagged => {
                    let why = Why::Idle((now - since) / 60);
                    entry.flagged = settle(store, &t.id, &l.name, &seat, baton, why, now);
                }
                Some(_) => {}
            }
        }
    }
    seen.retain(|k, _| open.contains(k));
    if seen != before {
        write(store, &seen);
    }
}

fn tell_to_close(store: &Store, row: &str, leg: &str, seat: &str, idle_min: i64) {
    let baton = baton_path(store, row, leg);
    let text = format!(
        "wsp: leg {leg} of {row} is still open, and this seat has sat {idle_min} min without closing it. \
         If the work is done and committed, close it now: `wsp leg done {row} --caption \"...\"` — run as its own \
         Bash call, not chained after anything. It reads your baton from {} ($WSP_BATON); write that with a heredoc \
         first if you have not. If wsp finds it still open in {} min it will close it for you from the baton, or flag the person.",
        util::contract(&baton),
        SECOND_WINDOW / 60
    );
    crate::cycle::log_line(store, &format!("leg {leg} of {row}: {seat} idle {idle_min} min with the leg open — told to close it"));
    crate::repair::tell_member(store, seat, &text);
}

/// Why wsp is settling a leg whose seat did not.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Why {
    /// The seat sat idle this many minutes.
    Idle(i64),
    /// The session ended.
    Ended,
}

impl Why {
    fn words(self) -> String {
        match self {
            Why::Idle(m) => format!("seat idle {m}m"),
            Why::Ended => "session ended".to_string(),
        }
    }
}

/// How the tree a leg stood in reads.
enum Tree {
    /// A work tree with nothing uncommitted, at this commit.
    Clean(String),
    Dirty(usize),
    /// Not a git tree (a sandbox row): nothing to commit, so not a reason to wait.
    NotGit,
    /// The seat is bound to no tree wsp can name.
    Unknown,
}

fn tree_of(store: &Store, seat: &str) -> (Option<String>, Tree) {
    let Some(dir) = store
        .bindings()
        .get(seat)
        .and_then(|b| b.get("cwd"))
        .and_then(|c| c.as_str())
        .map(|c| util::expand(c).display().to_string())
    else {
        return (None, Tree::Unknown);
    };
    let tree = match crate::leg_seat::dirty(&dir) {
        None => Tree::NotGit,
        Some(changed) if !changed.is_empty() => Tree::Dirty(changed.len()),
        Some(_) => {
            let head = std::process::Command::new("git")
                .args(["-C", &dir, "rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            Tree::Clean(head)
        }
    };
    (Some(dir), tree)
}

/// Close the leg for a seat that did not, if that is safe, else flag the person.
/// Returns whether the item is dealt with (closed or flagged), which is when it
/// is not looked at again.
fn settle(store: &Store, row: &str, leg: &str, seat: &str, baton_at: Option<i64>, why: Why, now: i64) -> bool {
    let path = baton_path(store, row, leg);
    let baton = std::fs::read_to_string(&path).unwrap_or_default();
    let (_, tree) = tree_of(store, seat);
    let evidence = |note: &str| {
        let head = match &tree {
            Tree::Clean(h) => format!("HEAD {h} is a clean commit"),
            Tree::Dirty(n) => format!("the tree has {n} uncommitted change(s)"),
            Tree::NotGit => "the tree is not a git work tree".to_string(),
            Tree::Unknown => "the seat is bound to no tree wsp can read".to_string(),
        };
        let baton = match baton_at {
            Some(at) if !baton.trim().is_empty() => format!("a baton was written at {} ({})", clock(at), util::contract(&path)),
            _ => format!("no baton at {}", util::contract(&path)),
        };
        format!("{head}; {baton}; {} ({note})", why.words())
    };
    let safe = matches!(tree, Tree::Clean(_) | Tree::NotGit) && baton_at.is_some() && !baton.trim().is_empty();
    if !safe {
        flag(store, row, leg, seat, &evidence("wsp does not guess, so it is not closed"));
        return true;
    }
    let caption = caption_of(&baton);
    let late = crate::leg_seat::Late { baton_at: baton_at.unwrap_or(now), by_wsp: Some(why.words()) };
    match crate::leg_seat::finish(store, row, leg, seat, &baton, &caption, Some(&late)) {
        Ok(_) => {
            crate::cycle::log_line(
                store,
                &format!("leg {leg} of {row}: closed by wsp: {}, baton written at {}", why.words(), clock(baton_at.unwrap_or(now))),
            );
            // The seat, if there is still one to hear it.
            if !matches!(why, Why::Ended) {
                crate::repair::tell_member(store, seat, &format!("wsp: leg {leg} of {row} is closed — wsp ran the close for you from your baton. Nothing is owed."));
            }
            true
        }
        Err(refused) => {
            crate::cycle::log_line(store, &format!("leg {leg} of {row}: wsp's close for {seat} was refused: {}", util::truncate(&crate::cmd_task::fold(&refused), 300)));
            flag(store, row, leg, seat, &format!("{}; wsp tried the close and it was refused: {}", evidence("closing it was tried"), util::truncate(&crate::cmd_task::fold(&refused), 300)));
            true
        }
    }
}

/// The item needs a person: a note about the row, in the log and to the list's
/// governor.
fn flag(store: &Store, row: &str, leg: &str, seat: &str, evidence: &str) {
    use crate::message::{self, About, Kind, Message, Party};
    crate::cycle::log_line(store, &format!("leg {leg} of {row}: needs a person — {evidence}"));
    let text = format!(
        "{row}: leg {leg} is open and its seat {seat} is not closing it\n\n{evidence}.\n\n\
         The close is `wsp leg done {row} --caption \"...\"` from the leg's seat once the tree is committed and the baton is written, \
         or `wsp leg reset {row} {leg}` to run it again."
    );
    let m = Message::new(Party::wsp(), Kind::Note, &text).about(About::Task(row.to_string()));
    let _ = message::raise(store, &m);
    if let Some(t) = store.find_task(row) {
        if let Some(w) = crate::cycle::list_of(store, &t) {
            crate::cycle::tell(store, &w, &format!("leg {leg} of {row} is open and not closing: {evidence}. The person is flagged."));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legs::Leg;
    use crate::model::Task;
    use std::cell::RefCell;

    struct Fake(RefCell<Option<State>>, bool);
    impl Seats for Fake {
        fn state(&self, _seat: &str) -> Option<State> {
            *self.0.borrow()
        }
        fn absent(&self, _seat: &str) -> bool {
            self.1
        }
    }

    /// A `doing` row `m-1` with one running leg `build` seated in `cpd-1`, bound
    /// to a git tree that is clean at a commit.
    fn staged(env: &util::Isolated) -> (Store, PathBuf) {
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut leg = Leg::pending("build", "make it");
        leg.state = LegState::Running;
        leg.pane = Some("cpd-1".into());
        leg.started = Some(util::iso_at(util::epoch_secs() - 3600));
        let mut t = Task::new("a row", "m-1");
        t.set_status(Status::Doing);
        t.body = "## Overview\nthe work\n\n## Log\n- 2026-11-05 started\n".to_string();
        legs::write(&mut t, &[leg]);
        store.save_task(&t).unwrap();
        let tree = env.home().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        for args in [vec!["init", "-q"], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "c"]] {
            let ok = std::process::Command::new("git").arg("-C").arg(&tree).args(&args).status().unwrap();
            assert!(ok.success());
        }
        store.set_binding("cpd-1", json!({ "task_id": "m-1", "pane_id": "cpd-1", "leg": "build", "cwd": tree.display().to_string() }));
        (store, tree)
    }

    fn write_baton(store: &Store, text: &str) -> i64 {
        let p = baton_path(store, "m-1", "build");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        written_at(&p).unwrap()
    }

    fn running(store: &Store) -> bool {
        legs::legs(&store.find_task("m-1").unwrap())[0].state == LegState::Running
    }

    fn told() -> Vec<(String, String)> {
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow().clone())
    }

    #[test]
    fn an_idle_seat_with_its_leg_open_and_a_baton_written_is_told_then_closed_by_wsp_timed_to_the_baton() {
        let env = util::isolated("closing-baton");
        let (store, _tree) = staged(&env);
        let fake = Fake(RefCell::new(Some(State::Idle)), false);
        let wrote = write_baton(&store, "# Built the store layer\nsee src/legs.rs\n");
        let t0 = wrote + 10;
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().clear());
        tick_at(&store, &fake, t0);
        assert!(told().is_empty(), "idle for no time yet");
        tick_at(&store, &fake, t0 + TELL_AFTER);
        assert_eq!(told().len(), 1, "told once: {:?}", told());
        assert!(told()[0].1.contains("wsp leg done m-1") && told()[0].1.contains("its own"), "{:?}", told());
        tick_at(&store, &fake, t0 + TELL_AFTER + 60);
        assert_eq!(told().len(), 1, "not told twice");
        assert!(running(&store), "still within the second window");
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW);
        assert!(!running(&store), "wsp closed it");
        let t = store.find_task("m-1").unwrap();
        let l = legs::legs(&t).remove(0);
        assert_eq!(l.caption.as_deref(), Some("Built the store layer"));
        assert!(l.baton.contains("src/legs.rs"));
        assert_eq!(l.closed.as_deref(), Some(util::iso_at(wrote).as_str()), "timed to the baton's write, not to the close");
        let log = t.section("Log").unwrap();
        assert!(log.contains("closed by wsp: seat idle") && log.contains("baton written at"), "{log}");
        assert!(told().len() == 2 && told()[1].1.contains("is closed"), "the seat hears it: {:?}", told());
    }

    #[test]
    fn an_idle_seat_with_no_baton_is_told_once_then_flagged_and_never_closed() {
        let env = util::isolated("closing-nobaton");
        let (store, _tree) = staged(&env);
        let fake = Fake(RefCell::new(Some(State::Idle)), false);
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().clear());
        let t0 = util::epoch_secs();
        tick_at(&store, &fake, t0);
        tick_at(&store, &fake, t0 + TELL_AFTER);
        tick_at(&store, &fake, t0 + TELL_AFTER + 30);
        assert_eq!(told().len(), 1, "told once");
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW);
        assert!(running(&store), "no baton, no guess");
        let raised = crate::message::raised(&store);
        assert_eq!(raised.len(), 1, "the person is flagged");
        assert!(raised[0].text.contains("no baton"), "{}", raised[0].text);
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW + 600);
        assert_eq!(crate::message::raised(&store).len(), 1, "flagged once");
    }

    #[test]
    fn a_session_that_ended_with_its_leg_open_is_closed_at_once_when_the_baton_is_there() {
        let env = util::isolated("closing-ended");
        let (store, _tree) = staged(&env);
        write_baton(&store, "pointers\n");
        let fake = Fake(RefCell::new(Some(State::Gone)), false);
        tick_at(&store, &fake, util::epoch_secs());
        assert!(!running(&store));
        let log = store.find_task("m-1").unwrap().section("Log").unwrap();
        assert!(log.contains("closed by wsp: session ended"), "{log}");
    }

    #[test]
    fn a_session_that_ended_with_a_dirty_tree_or_no_baton_is_flagged_at_once() {
        let env = util::isolated("closing-ended-dirty");
        let (store, tree) = staged(&env);
        write_baton(&store, "pointers\n");
        std::fs::write(tree.join("f"), "x").unwrap();
        std::process::Command::new("git").arg("-C").arg(&tree).args(["add", "f"]).status().unwrap();
        let fake = Fake(RefCell::new(None), true);
        tick_at(&store, &fake, util::epoch_secs());
        assert!(running(&store), "a dirty tree is not closed over");
        let raised = crate::message::raised(&store);
        assert_eq!(raised.len(), 1);
        assert!(raised[0].text.contains("uncommitted"), "{}", raised[0].text);
    }

    #[test]
    fn a_seat_waiting_on_its_own_question_is_not_hurried() {
        let env = util::isolated("closing-working");
        let (store, _tree) = staged(&env);
        // Working with no baton written: a leg mid-work is never touched.
        let fake = Fake(RefCell::new(Some(State::Working)), false);
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().clear());
        let t0 = util::epoch_secs();
        tick_at(&store, &fake, t0);
        tick_at(&store, &fake, t0 + 3600);
        assert!(told().is_empty() && running(&store));
    }

    #[test]
    fn a_working_seat_stuck_after_writing_its_baton_is_closed_from_the_baton() {
        let env = util::isolated("closing-stuck");
        let (store, _tree) = staged(&env);
        let fake = Fake(RefCell::new(Some(State::Working)), false);
        let wrote = write_baton(&store, "done\n");
        tick_at(&store, &fake, wrote + TELL_AFTER + 1);
        tick_at(&store, &fake, wrote + TELL_AFTER + SECOND_WINDOW + 2);
        assert!(!running(&store));
    }

    #[test]
    fn a_refused_close_says_so_on_the_log_and_leaves_the_leg_open() {
        let env = util::isolated("closing-refused");
        let (store, tree) = staged(&env);
        std::fs::write(tree.join("f"), "x").unwrap();
        std::process::Command::new("git").arg("-C").arg(&tree).args(["add", "f"]).status().unwrap();
        let args = crate::Args::synth("leg", &["done", "m-1"], &[("caption", "done")]);
        let rc = crate::leg_seat::cmd_done(&store, &args, Some("cpd-1"));
        assert_eq!(rc, 1, "a refused close exits non-zero");
        assert!(running(&store));
        let said = crate::cycle::tests::SAID.with(|s| s.borrow().clone());
        assert!(said.iter().any(|l| l.contains("leg done m-1 refused") && l.contains("uncommitted")), "{said:?}");
    }

    #[test]
    fn leg_done_reads_the_known_baton_when_from_is_not_given_and_says_when_it_was_late() {
        let env = util::isolated("closing-known");
        let (store, _tree) = staged(&env);
        let wrote = write_baton(&store, "handed on\n");
        let p = baton_path(&store, "m-1", "build");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 3600);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        let _ = wrote;
        let args = crate::Args::synth("leg", &["done", "m-1"], &[("caption", "built")]);
        assert_eq!(crate::leg_seat::cmd_done(&store, &args, Some("cpd-1")), 0);
        let t = store.find_task("m-1").unwrap();
        assert_eq!(legs::legs(&t)[0].baton, "handed on");
        assert!(t.section("Log").unwrap().contains("closed late: done at"), "{}", t.section("Log").unwrap());
    }

    #[test]
    fn the_caption_is_the_batons_first_line_cut_to_fit() {
        assert_eq!(caption_of("\n## Built it\nmore"), "Built it");
        assert_eq!(caption_of(&"x".repeat(300)).chars().count(), 100);
    }

    #[test]
    fn the_closing_words_say_own_call_and_heredoc() {
        let w = how_to_close("m-1", false);
        assert!(w.contains("separate Bash calls") && w.contains("<<'EOF'") && w.contains("$WSP_BATON") && w.contains("wsp leg done m-1"), "{w}");
        assert!(how_to_close("m-1", true).contains("wsp review m-1"));
    }
}
