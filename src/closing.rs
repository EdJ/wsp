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
//! # The other three closing verbs (`wsp-374`)
//!
//! A member's `wsp review`, a verifier's `wsp verified` and a barrier check's
//! `worklist go`/`hold` are closes of the same kind, and go through the same
//! tick, tell, flag and settle path:
//!
//! - **A verdict file at a place wsp knows** ([`verdict_path`], `$WSP_VERDICT` in
//!   the seat), keyed by kind and row. The verb reads it when it is given no
//!   `--from` and no words. A member's file is its account; a verifier's and a
//!   check's first line is the verdict word (`holds`/`blocks`; `go`/`hold`/
//!   `followup`) and the rest is the verdict ([`parse_verdict`]). A file older
//!   than the seat's start is an earlier run's and is ignored ([`fresh_verdict`]).
//! - **A member** is noticed by [`crate::repair`]'s `gone_member`, which tells it
//!   once and ends it a window later; it asks [`close_member`] first, which closes
//!   the row for the seat when the verdict is written and the tree is clean at a
//!   commit, and flags a person otherwise.
//! - **A verifier and a check** are noticed by [`tick_at`] on the legs' windows,
//!   and closed by [`settle_verifier`] and [`settle_check`] when the file parses
//!   to a verdict. `followup` names rows wsp cannot guess, so it is flagged.

use crate::cycle::Seats;
use crate::legs::{self, State as LegState};
use crate::verification::State as PassState;
use crate::model::{Status, Worklist, WorklistStatus};
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

/// When leg `leg` of `row` wrote its baton **in this run of it**: a file older
/// than the leg's start belongs to an earlier run (a `leg reset`, a respawn) and
/// is not read, told about or closed over.
pub(crate) fn fresh_baton(store: &Store, row: &str, leg: &str, started: Option<&str>) -> Option<i64> {
    let at = written_at(&baton_path(store, row, leg))?;
    let start = started.map(util::epoch_of).unwrap_or(0);
    (at >= start).then_some(at)
}

/// A leg starting or starting again begins with no baton: whatever file is at
/// its path is a previous run's.
pub(crate) fn clear_baton(store: &Store, row: &str, leg: &str) {
    let _ = std::fs::remove_file(baton_path(store, row, leg));
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
    util::truncate(&crate::util::fold(first), 100)
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

// ---- verdicts: a place for them, and what they parse to --------------------------

/// Which closing verb a verdict file is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A member's `wsp review`.
    Member,
    /// A verifier's `wsp verified`.
    Verifier,
    /// A barrier check's `worklist go`/`hold`/`followup`.
    Check,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Member => "review",
            Kind::Verifier => "verified",
            Kind::Check => "check",
        }
    }

    /// The words a verdict file may open with.
    fn words(self) -> &'static [&'static str] {
        match self {
            Kind::Member => &[],
            Kind::Verifier => &["holds", "blocks"],
            Kind::Check => &["go", "hold", "followup"],
        }
    }
}

/// Where the verdict for `key` is written, and where its closing verb reads it
/// from when it is given no `--from`. `key` is the row for a member and for a
/// verifier (the member it reads), and the barrier's [`crate::barrier::subject`]
/// for a check. Under the state directory, so a seat invents no path of its own.
pub(crate) fn verdict_path(store: &Store, key: &str, kind: Kind) -> PathBuf {
    store.state.join("verdicts").join(kind.dir()).join(format!("{key}.md"))
}

/// When `key`'s verdict was written **in this run of the seat**: a file older
/// than `start` (epoch seconds, `None` for no start known) is an earlier run's,
/// and is not read, told about or closed over.
pub(crate) fn fresh_verdict(store: &Store, key: &str, kind: Kind, start: Option<i64>) -> Option<i64> {
    let at = written_at(&verdict_path(store, key, kind))?;
    (at >= start.unwrap_or(0)).then_some(at)
}

/// What a verdict file says.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Verdict {
    /// `holds`, `go`, …: the first line, for a kind that has one.
    pub word: Option<String>,
    /// The line a glance reads: the first line of the body, cut to fit.
    pub caption: String,
    /// The verdict itself, without the word.
    pub body: String,
}

/// What a verdict file parses to, or `None`: empty, or (for a verifier and a
/// check) not opening with one of the kind's words. Nothing is guessed.
pub(crate) fn parse_verdict(kind: Kind, text: &str) -> Option<Verdict> {
    if kind.words().is_empty() {
        let body = text.trim().to_string();
        return (!body.is_empty()).then(|| Verdict { word: None, caption: caption_of(&body), body });
    }
    let text = text.trim_start();
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let word = first.trim().trim_start_matches(['#', '*', '-', ' ']);
    let word = word.strip_prefix("verdict:").or_else(|| word.strip_prefix("Verdict:")).unwrap_or(word);
    let word = word.trim().trim_end_matches(['.', ':', '*']).to_lowercase();
    let body = rest.trim().to_string();
    (kind.words().contains(&word.as_str()) && !body.is_empty())
        .then(|| Verdict { word: Some(word), caption: caption_of(&body), body })
}

/// The verdict for `key` if one was written in this run and parses: what it says,
/// the file holding **just its body** (the file the verb is given as `--from`),
/// and when it was written.
pub(crate) fn verdict_source(store: &Store, key: &str, kind: Kind, start: Option<i64>) -> Option<(Verdict, PathBuf, i64)> {
    let at = fresh_verdict(store, key, kind, start)?;
    let path = verdict_path(store, key, kind);
    let v = parse_verdict(kind, &std::fs::read_to_string(&path).ok()?)?;
    let body = path.with_extension("body");
    std::fs::write(&body, &v.body).ok()?;
    Some((v, body, at))
}

/// A seat starting a row again begins with no verdict: whatever file is at its
/// path is a previous run's.
pub(crate) fn clear_verdict(store: &Store, key: &str, kind: Kind) {
    let p = verdict_path(store, key, kind);
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("body"));
}

/// The words the orders about closing carry for a member, a verifier and a check.
pub(crate) fn how_to_close_verdict(kind: Kind, row: &str) -> String {
    let (verb, shape) = match kind {
        Kind::Member => (format!("`wsp review {row}`"), "the account of what you did and left"),
        Kind::Verifier => (
            format!("`wsp verified {row} --caption \"...\"`"),
            "`holds` or `blocks` on the first line, then the verdict",
        ),
        Kind::Check => (
            format!("`wsp worklist go {row}` (or `hold`)"),
            "`go` or `hold` on the first line, then what you decided",
        ),
    };
    format!(
        "Close it in two separate Bash calls, never chained with anything else: first write {shape}, \
         `cat > \"$WSP_VERDICT\" <<'EOF'` ... `EOF` (a heredoc with its text, never a bare `cat`); then, as a call of its own, {verb} \
         (it reads $WSP_VERDICT)."
    )
}

/// The line a row's log (or the cycle log) gets when a close was not at the moment
/// the verdict was written: by wsp, or late.
pub(crate) fn note_close(store: &Store, row: &str, what: &str, written: i64, by_wsp: Option<&str>) {
    let now = util::epoch_secs();
    if by_wsp.is_none() && now - written < LATE_AFTER {
        return;
    }
    let clause = late_clause(written, now, by_wsp).replace("baton written", "verdict written");
    let line = format!("{what}{clause}");
    crate::store::log_line(store, &format!("{row}: {line}"));
    let id = row.to_string();
    store.locked(|| {
        let Some(mut t) = store.find_task(&id) else { return false };
        crate::log::note(store, &mut t, &line).is_ok()
    });
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

/// One pass over every running leg of a row at `doing`, every verifier whose
/// pass is still reading, and every barrier check still open.
pub(crate) fn tick_at(store: &Store, seats: &dyn Seats, now: i64) {
    let mut seen = read(store);
    let before = seen.clone();
    let mut open = std::collections::BTreeSet::new();
    let mut asks = None;
    for t in store.tasks_shared().iter().filter(|t| t.status() == Status::Doing && legs::staged(t)) {
        for l in legs::legs(t).into_iter().filter(|l| l.state == LegState::Running) {
            let Some(seat) = l.pane.clone() else { continue };
            let baton = fresh_baton(store, &t.id, &l.name, l.started.as_deref());
            let item = Item { key: format!("{}/{}", t.id, l.name), row: &t.id, seat: &seat, written: baton };
            let tell = |idle: i64| tell_to_close(store, &t.id, &l.name, &seat, idle);
            let close = |why: Why| settle(store, &t.id, &l.name, &seat, baton, why, now);
            advance(store, seats, &mut asks, &mut seen, &mut open, &item, now, &tell, &close);
        }
    }
    // A verifier whose pass is still reading: its verdict is the close.
    for t in store.tasks_shared().iter().filter(|t| t.body.contains("## Verification")) {
        for p in crate::verification::passes(t).into_iter().filter(|p| p.state == PassState::Running) {
            let Some(seat) = p.pane.clone().filter(|_| p.standing()) else { continue };
            let start = Some(util::epoch_of(&p.at));
            let written = fresh_verdict(store, &t.id, Kind::Verifier, start);
            let item = Item { key: format!("verify/{}/{seat}", t.id), row: &t.id, seat: &seat, written };
            let tell = |idle: i64| tell_verdict(store, Kind::Verifier, &t.id, &seat, idle, &format!("{} verifier", t.id));
            let close = |why: Why| settle_verifier(store, &t.id, &seat, start, why, now);
            advance(store, seats, &mut asks, &mut seen, &mut open, &item, now, &tell, &close);
        }
    }
    // A barrier check still open: its verdict, `go` or `hold`, is the close.
    for w in store.worklists().into_iter().filter(|w| matches!(w.status(), WorklistStatus::Running | WorklistStatus::Held)) {
        let groups = w.groups();
        for (i, g) in groups.iter().enumerate() {
            let Some(c) = crate::barrier::running(g) else { continue };
            let Some(seat) = c.standing().map(str::to_string) else { continue };
            // A barrier already passed has nothing for this check to decide:
            // the tick ends its seat, and it is not told to close or closed
            // with a `go` (`wsp-385`).
            if crate::barrier::moot(&w, &groups, i) {
                continue;
            }
            let at = i + 1;
            let key = crate::barrier::subject(&w.id, at);
            let start = Some(util::epoch_of(&c.at));
            let written = fresh_verdict(store, &key, Kind::Check, start);
            let item = Item { key: format!("check/{key}"), row: &w.id, seat: &seat, written };
            let tell = |idle: i64| tell_verdict(store, Kind::Check, &key, &seat, idle, &format!("{} barrier {at} check", w.id));
            let close = |why: Why| settle_check(store, &w, at, &seat, start, why, now);
            advance(store, seats, &mut asks, &mut seen, &mut open, &item, now, &tell, &close);
        }
    }
    seen.retain(|k, _| open.contains(k));
    if seen != before {
        write(store, &seen);
    }
}

/// One open item the tick watches: a leg, a verifier or a check.
struct Item<'a> {
    key: String,
    /// The row (or list) it is about, for what the seat is waiting on.
    row: &'a str,
    seat: &'a str,
    /// When its verdict (or baton) was written in this run, if it was.
    written: Option<i64>,
}

/// One step for one item: tell it once when its seat has sat idle (or has
/// written and stopped), act a window later, and settle at once when the session
/// has ended. `tell` is given the idle minutes; `close` is given why, and says
/// whether the item is dealt with (closed or flagged).
#[allow(clippy::too_many_arguments)]
fn advance(
    store: &Store,
    seats: &dyn Seats,
    asks: &mut Option<crate::waiting::Asks>,
    seen: &mut std::collections::BTreeMap<String, Seen>,
    open: &mut std::collections::BTreeSet<String>,
    item: &Item,
    now: i64,
    tell: &dyn Fn(i64),
    close: &dyn Fn(Why) -> bool,
) {
    open.insert(item.key.clone());
    let entry = seen.entry(item.key.clone()).or_default();
    let state = seats.state(item.seat);
    // A session that ended with its item open is not waited on.
    if seats.absent(item.seat) || matches!(state, Some(State::Gone | State::Empty)) {
        if !entry.flagged {
            entry.flagged = close(Why::Ended);
        }
        return;
    }
    let since = match state {
        Some(State::Idle) => {
            // A question it asked, or a prompt, is a wait the item is owed an
            // answer for; the shells it has running are not, because a stuck
            // shell is the very thing this is for.
            let asks = asks.get_or_insert_with(|| crate::waiting::Asks::read(store));
            if crate::waiting::reading(Some(State::Idle), asks, &crate::waiting::Shells::default(), item.seat, item.row).is_some() {
                *entry = Seen::default();
                return;
            }
            Some(*entry.idle_since.get_or_insert(now))
        }
        // A tool is running and has been since the verdict was written: the
        // close is what it is stuck on.
        Some(State::Working) => {
            entry.idle_since = None;
            item.written
        }
        _ => None,
    };
    let Some(since) = since else {
        *entry = Seen::default();
        return;
    };
    if since > now - TELL_AFTER {
        return;
    }
    match entry.told {
        None => {
            entry.told = Some(now);
            tell((now - since) / 60);
        }
        Some(told) if now - told >= SECOND_WINDOW && !entry.flagged => {
            entry.flagged = close(Why::Idle((now - since) / 60));
        }
        Some(_) => {}
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
    crate::store::log_line(store, &format!("leg {leg} of {row}: {seat} idle {idle_min} min with the leg open — told to close it"));
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
            crate::store::log_line(
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
            crate::store::log_line(store, &format!("leg {leg} of {row}: wsp's close for {seat} was refused: {}", util::truncate(&crate::util::fold(&refused), 300)));
            flag(store, row, leg, seat, &format!("{}; wsp tried the close and it was refused: {}", evidence("closing it was tried"), util::truncate(&crate::util::fold(&refused), 300)));
            true
        }
    }
}

/// The item needs a person: a note about the row, in the log and to the list's
/// governor.
fn flag(store: &Store, row: &str, leg: &str, seat: &str, evidence: &str) {
    let fix = format!(
        "The close is `wsp leg done {row} --caption \"...\"` from the leg's seat once the tree is committed and the baton is written, \
         or `wsp leg reset {row} {leg}` to run it again."
    );
    flag_about(store, row, &format!("leg {leg}"), seat, evidence, &fix, None, true);
}

/// [`flag`] for any item: `what` is how the log names it (`leg build`, `the
/// verifier`), `fix` the sentence that says how a person closes it, and `list`
/// the list to tell when the row is not a task of one (a check's).
fn flag_about(store: &Store, row: &str, what: &str, seat: &str, evidence: &str, fix: &str, list: Option<&Worklist>, governor: bool) {
    use crate::message::{self, About, Kind, Message, Party};
    crate::store::log_line(store, &format!("{what} of {row}: needs a person — {evidence}"));
    let text = format!("{row}: {what} is open and its seat {seat} is not closing it\n\n{evidence}.\n\n{fix}");
    let about = match list {
        Some(w) => About::Scope(w.id.clone()),
        None => About::Task(row.to_string()),
    };
    let m = Message::new(Party::wsp(), Kind::Note, &text).about(about);
    let _ = message::raise(store, &m);
    let w = match list {
        _ if !governor => None,
        Some(w) => Some(w.clone()),
        None => store.find_task(row).and_then(|t| crate::cycle::list_of(store, &t)),
    };
    if let Some(w) = w {
        crate::cycle::tell(store, &w, &format!("{what} of {row} is open and not closing: {evidence}. The person is flagged."));
    }
}

/// Said once to a verifier's or a check's seat that has sat with its item open.
fn tell_verdict(store: &Store, kind: Kind, key: &str, seat: &str, idle_min: i64, what: &str) {
    let path = verdict_path(store, key, kind);
    let row = match kind {
        Kind::Check => crate::barrier::parse_subject(key).map(|(l, _)| l.to_string()).unwrap_or_else(|| key.to_string()),
        _ => key.to_string(),
    };
    let text = format!(
        "wsp: the {what} is still open, and this seat has sat {idle_min} min without closing it. \
         If your verdict is reached, close it now. {} Your verdict is read from {} ($WSP_VERDICT). \
         If wsp finds it still open in {} min it will close it for you from that file when it parses to a verdict, or flag the person.",
        how_to_close_verdict(kind, &row),
        util::contract(&path),
        SECOND_WINDOW / 60
    );
    crate::store::log_line(store, &format!("{what}: {seat} idle {idle_min} min with it open — told to close it"));
    crate::repair::tell_member(store, seat, &text);
}

/// What a verdict file and the tree say, as a flag's evidence.
fn verdict_evidence(store: &Store, key: &str, kind: Kind, written: Option<i64>, tree: &Tree, why: Why, note: &str) -> String {
    let path = verdict_path(store, key, kind);
    let head = match tree {
        Tree::Clean(h) => format!("HEAD {h} is a clean commit"),
        Tree::Dirty(n) => format!("the tree has {n} uncommitted change(s)"),
        Tree::NotGit => "the tree is not a git work tree".to_string(),
        Tree::Unknown => "the seat is bound to no tree wsp can read".to_string(),
    };
    let file = std::fs::read_to_string(&path).unwrap_or_default();
    let verdict = match written {
        Some(at) if parse_verdict(kind, &file).is_some() => format!("a verdict was written at {} ({})", clock(at), util::contract(&path)),
        Some(at) => format!("a file was written at {} ({}) but does not parse to a verdict", clock(at), util::contract(&path)),
        None => format!("no verdict at {}", util::contract(&path)),
    };
    format!("{head}; {verdict}; {} ({note})", why.words())
}

// ---- a member ---------------------------------------------------------------------

/// Flag a person once per start of the row: the marker is a file of its own, as
/// the tick's record cannot hold a key it does not watch.
fn flag_once(store: &Store, key: &str) -> bool {
    let path = store.state.join("closing-flags.json");
    let mut v: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
    if v.get(key).is_some() {
        return false;
    }
    v[key] = json!(true);
    let _ = std::fs::create_dir_all(&store.state);
    let _ = std::fs::write(&path, v.to_string());
    true
}

/// A member whose seat is idle past the window, or ended, with its row still at
/// `doing` (`gone_member` is the one that notices): close it for the seat when it
/// is safe, which is the verdict written in this run and the tree clean at a
/// commit; else flag the person, once. Returns whether the row was closed, so the
/// caller does not go on to end the seat.
pub(crate) fn close_member(store: &Store, t: &crate::model::Task, seat: &str, why: Why) -> bool {
    let start = crate::repair::started_at(t);
    let written = fresh_verdict(store, &t.id, Kind::Member, start);
    let (_, tree) = tree_of(store, seat);
    let safe = matches!(tree, Tree::Clean(_) | Tree::NotGit);
    let id = &t.id;
    if let (true, Some((args, _, at))) = (safe, member_args_for(store, t)) {
        let code = crate::cmd_task::review(store, &args);
        if code == 0 {
            note_close(store, id, "review", at, Some(&why.words()));
            if !matches!(why, Why::Ended) {
                crate::repair::tell_member(store, seat, &format!("wsp: {id} is in review — wsp ran `wsp review` for you from your verdict. Nothing is owed."));
            }
            return true;
        }
        crate::store::log_line(store, &format!("{id}: wsp's `wsp review` for {seat} was refused (exit {code})"));
        let evidence = verdict_evidence(store, id, Kind::Member, written, &tree, why, "wsp tried the review and it was refused");
        flag_member(store, t, seat, &evidence, start, !matches!(why, Why::Ended));
        return false;
    }
    let evidence = verdict_evidence(store, id, Kind::Member, written, &tree, why, "wsp does not guess, so it is not reviewed");
    flag_member(store, t, seat, &evidence, start, !matches!(why, Why::Ended));
    false
}

fn flag_member(store: &Store, t: &crate::model::Task, seat: &str, evidence: &str, start: Option<i64>, governor: bool) {
    if !flag_once(store, &format!("member/{}/{}", t.id, start.unwrap_or(0))) {
        return;
    }
    let fix = format!("The close is `wsp review {}` from the member's seat once the tree is committed and the verdict is written, or `wsp reopen {} \"what is owed\"`.", t.id, t.id);
    flag_about(store, &t.id, "the member", seat, evidence, &fix, None, governor);
}

/// `wsp review`'s arguments from the member's verdict file, if there is one.
fn member_args_for(store: &Store, t: &crate::model::Task) -> Option<(crate::Args, PathBuf, i64)> {
    let (_, body, at) = verdict_source(store, &t.id, Kind::Member, crate::repair::started_at(t))?;
    Some((crate::Args::synth("review", &[&t.id, "--from", &body.display().to_string()], &[]), body, at))
}

/// A member's `wsp review <id>` given no account: read it from the verdict file.
pub(crate) fn member_args(store: &Store, args: &crate::Args) -> Option<(crate::Args, i64)> {
    if args.rest.len() != 1 {
        return None;
    }
    let t = store.find_task(&args.rest[0])?;
    member_args_for(store, &t).map(|(a, _, at)| (a, at))
}

// ---- a verifier -------------------------------------------------------------------

/// The newest pass of `member` still reading, and its start: this seat's when it
/// is known, else the newest running one.
fn running_pass(t: &crate::model::Task, me: Option<&str>) -> Option<crate::verification::Pass> {
    let ps: Vec<_> = crate::verification::passes(t).into_iter().filter(|p| p.state == PassState::Running).collect();
    ps.iter().rev().find(|p| me.is_some() && p.pane.as_deref() == me).or(ps.last()).cloned()
}

/// `wsp verified <member>` given no verdict: read it from the verdict file. The
/// word typed (`--holds`/`--blocks`) wins over the file's, and so does a caption.
pub(crate) fn verifier_args(store: &Store, args: &crate::Args, me: Option<&str>) -> Option<(crate::Args, i64)> {
    if args.rest.len() != 1 {
        return None;
    }
    let t = store.find_task(&args.rest[0])?;
    let pass = running_pass(&t, me)?;
    let (v, body, at) = verdict_source(store, &t.id, Kind::Verifier, Some(util::epoch_of(&pass.at)))?;
    let word = match (args.has("holds"), args.has("blocks")) {
        (true, false) => "holds".to_string(),
        (false, true) => "blocks".to_string(),
        _ => v.word.clone()?,
    };
    let caption = args.get("caption").filter(|c| c != "true").unwrap_or(v.caption);
    let rest = [t.id.as_str(), "--from", &body.display().to_string()];
    Some((crate::Args::synth("verified", &rest, &[(word.as_str(), "true"), ("caption", &caption)]), at))
}

/// Close a verifier's pass for its seat from its verdict file, when that file was
/// written in this run and parses; else flag the person. True when dealt with.
fn settle_verifier(store: &Store, member: &str, seat: &str, start: Option<i64>, why: Why, now: i64) -> bool {
    let written = fresh_verdict(store, member, Kind::Verifier, start);
    let (_, tree) = tree_of(store, seat);
    let fix = format!(
        "The close is `wsp verified {member} --caption \"...\"` from the verifier's seat with the verdict in {}, or `wsp verified {member} --holds|--blocks --from FILE --caption \"...\"` by hand.",
        util::contract(&verdict_path(store, member, Kind::Verifier))
    );
    let Some(t) = store.find_task(member) else { return true };
    let Some((args, at)) = verifier_args(store, &crate::Args::synth("verified", &[member], &[]), Some(seat)) else {
        let evidence = verdict_evidence(store, member, Kind::Verifier, written, &tree, why, "wsp does not guess, so it is not recorded");
        flag_about(store, member, "the verifier", seat, &evidence, &fix, None, true);
        return true;
    };
    drop(t);
    let code = crate::verification::verified_as(store, &args, Some(seat.to_string()));
    if code == 0 {
        note_close(store, member, "verified", at, Some(&why.words()));
        if !matches!(why, Why::Ended) {
            crate::repair::tell_member(store, seat, &format!("wsp: your verdict on {member} is recorded — wsp ran `wsp verified` for you from your verdict file. Nothing is owed."));
        }
        return true;
    }
    crate::store::log_line(store, &format!("{member}: wsp's `wsp verified` for {seat} was refused (exit {code}) at {}", clock(now)));
    let evidence = verdict_evidence(store, member, Kind::Verifier, written, &tree, why, "wsp tried the verdict and it was refused");
    flag_about(store, member, "the verifier", seat, &evidence, &fix, None, true);
    true
}

// ---- a barrier check --------------------------------------------------------------

/// `worklist go`/`hold` given no words: read the verdict from the barrier's file,
/// when its first line is the verb's own word.
pub(crate) fn check_args(store: &Store, args: &crate::Args, verb: &str, w: &Worklist) -> Option<(crate::Args, i64)> {
    if args.rest.len() > 2 || args.get("from").is_some() {
        return None;
    }
    let pos = crate::worklist::position(store, w, crate::worklist::Reading::Landed);
    let at = pos.at?;
    let g = w.groups().into_iter().nth(at - 1)?;
    let c = crate::barrier::running(&g)?;
    let key = crate::barrier::subject(&w.id, at);
    let (v, body, written) = verdict_source(store, &key, Kind::Check, Some(util::epoch_of(&c.at)))?;
    if v.word.as_deref() != Some(verb) {
        return None;
    }
    let caption = args.get("caption").filter(|c| c != "true").unwrap_or(v.caption);
    let rest = [verb, w.id.as_str()];
    let synth = crate::Args::synth(&args.cmd, &rest, &[("from", &body.display().to_string()), ("caption", &caption)]);
    Some((synth, written))
}

/// Close a check for its seat from the barrier's verdict file: `go` and `hold`
/// when it parses, a `followup` flagged (it names rows wsp cannot guess).
fn settle_check(store: &Store, w: &Worklist, at: usize, seat: &str, start: Option<i64>, why: Why, now: i64) -> bool {
    let key = crate::barrier::subject(&w.id, at);
    let written = fresh_verdict(store, &key, Kind::Check, start);
    let (_, tree) = tree_of(store, seat);
    let fix = format!(
        "The close is `wsp worklist go {0} \"...\"` or `wsp worklist hold {0} \"why\"` (or `followup`) from the check's seat, or by hand; the verdict file is {1}.",
        w.id,
        util::contract(&verdict_path(store, &key, Kind::Check))
    );
    let what = format!("the barrier {at} check");
    let parsed = verdict_source(store, &key, Kind::Check, start);
    let Some((v, body, at_written)) = parsed else {
        let evidence = verdict_evidence(store, &key, Kind::Check, written, &tree, why, "wsp does not guess, so it is not passed or held");
        flag_about(store, &w.id, &what, seat, &evidence, &fix, Some(w), true);
        return true;
    };
    let word = v.word.clone().unwrap_or_default();
    if word == "followup" {
        let evidence = verdict_evidence(store, &key, Kind::Check, written, &tree, why, "a follow-up names rows, which wsp does not guess");
        flag_about(store, &w.id, &what, seat, &evidence, &fix, Some(w), true);
        return true;
    }
    let from = body.display().to_string();
    let a = crate::Args::synth("worklist", &[word.as_str(), &w.id], &[("from", &from), ("caption", &v.caption)]);
    let code = match word.as_str() {
        "go" => crate::cmd_worklist::go(store, &a),
        _ => crate::cmd_worklist::hold(store, &a),
    };
    if code == 0 {
        note_close(store, &w.id, &format!("barrier {at} {word}"), at_written, Some(&why.words()));
        if !matches!(why, Why::Ended) {
            crate::repair::tell_member(store, seat, &format!("wsp: your verdict on {} barrier {at} is recorded — wsp ran `{word}` for you from your verdict file. Nothing is owed.", w.id));
        }
        return true;
    }
    crate::store::log_line(store, &format!("{}: wsp's `{word}` for {seat} was refused (exit {code}) at {}", w.id, clock(now)));
    let evidence = verdict_evidence(store, &key, Kind::Check, written, &tree, why, &format!("wsp tried `{word}` and it was refused"));
    flag_about(store, &w.id, &what, seat, &evidence, &fix, Some(w), true);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legs::Leg;
    use crate::model::Task;
    use std::cell::RefCell;
    use crate::model::WorklistStatus;

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
        leg.started = Some(util::iso_at(util::epoch_secs() - 6 * 3600));
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
    fn a_session_that_ended_with_a_dirty_tree_is_flagged_at_once() {
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

    /// Re-run a leg: a baton hours older than the new run's start.
    fn rerun_with_an_old_baton(store: &Store) {
        let p = baton_path(store, "m-1", "build");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "# last run's baton\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 3600);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
    }

    #[test]
    fn a_baton_older_than_the_legs_start_is_never_told_about_or_closed_over_working_or_idle() {
        for state in [State::Working, State::Idle] {
            let env = util::isolated(&format!("closing-stale-{state:?}"));
            let (store, _tree) = staged(&env);
            rerun_with_an_old_baton(&store);
            assert!(fresh_baton(&store, "m-1", "build", Some(&util::iso_at(util::epoch_secs() - 3600))).is_none());
            let fake = Fake(RefCell::new(Some(state)), false);
            crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().clear());
            let t0 = util::epoch_secs();
            tick_at(&store, &fake, t0);
            let late = t0 + TELL_AFTER + SECOND_WINDOW;
            if state == State::Working {
                tick_at(&store, &fake, late);
                assert!(told().is_empty(), "a working seat with only an old baton is working: {:?}", told());
            } else {
                // Idle is told (it is idle), but its old baton is not closed over.
                tick_at(&store, &fake, t0 + TELL_AFTER);
                tick_at(&store, &fake, late);
                assert!(!told().iter().any(|(_, t)| t.contains("is closed")), "{:?}", told());
                assert!(crate::message::raised(&store)[0].text.contains("no baton"), "flagged, not closed");
            }
            assert!(running(&store), "{state:?}: the new run's work is not cut off");
        }
    }

    #[test]
    fn opening_or_resetting_a_leg_removes_its_old_baton_and_leg_done_does_not_read_a_stale_one() {
        let env = util::isolated("closing-clear");
        let (store, _tree) = staged(&env);
        rerun_with_an_old_baton(&store);
        let args = crate::Args::synth("leg", &["done", "m-1"], &[("caption", "c")]);
        assert_eq!(crate::leg_seat::cmd_done(&store, &args, Some("cpd-1")), 0);
        let l = legs::legs(&store.find_task("m-1").unwrap()).remove(0);
        assert_eq!(l.baton, "", "the stale file was not read");
        assert!(l.closed.as_deref().map(util::epoch_of).unwrap() >= util::epoch_of(l.started.as_deref().unwrap()), "no end before the start");
    }

    #[test]
    fn resetting_a_running_leg_removes_its_old_baton() {
        let env = util::isolated("closing-reset");
        let (store, _tree) = staged(&env);
        rerun_with_an_old_baton(&store);
        assert!(crate::leg_seat::reset(&store, "m-1", "build").is_some());
        assert!(!baton_path(&store, "m-1", "build").exists());
    }

    #[test]
    fn a_session_that_ended_with_no_baton_is_flagged_at_once_and_the_leg_stays_open() {
        let env = util::isolated("closing-ended-nobaton");
        let (store, _tree) = staged(&env);
        let fake = Fake(RefCell::new(Some(State::Gone)), false);
        tick_at(&store, &fake, util::epoch_secs());
        assert!(running(&store));
        let raised = crate::message::raised(&store);
        assert_eq!(raised.len(), 1);
        assert!(raised[0].text.contains("no baton") && raised[0].text.contains("session ended"), "{}", raised[0].text);
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

    // ---- wsp-374: a member, a verifier and a check ----------------------------------

    fn git_tree(env: &util::Isolated, name: &str) -> PathBuf {
        let tree = env.home().join(name);
        std::fs::create_dir_all(&tree).unwrap();
        for args in [vec!["init", "-q"], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "c"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&tree).args(&args).status().unwrap().success());
        }
        tree
    }

    fn put(store: &Store, key: &str, kind: Kind, text: &str) -> i64 {
        let p = verdict_path(store, key, kind);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        written_at(&p).unwrap()
    }

    fn raised_texts(store: &Store) -> Vec<String> {
        crate::message::raised(store).into_iter().map(|m| m.text).collect()
    }

    fn governor_told() -> Vec<String> {
        crate::cycle::tests::TOLD.with(|t| t.borrow().clone())
    }

    fn clear_told() {
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().clear());
        crate::cycle::tests::TOLD.with(|t| t.borrow_mut().clear());
    }

    // -- a member

    /// A run of one member `m-2` at `doing` for an hour, claimed in `cpd-2`, which
    /// is bound to a git tree clean at a commit.
    fn staged_member(env: &util::Isolated, started_ago: i64) -> (Store, PathBuf) {
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a member", "m-2");
        t.set_status(Status::Doing);
        t.caption = Some("Did the thing".into());
        t.body = format!("## Overview\nthe work\n\n## Log\n\n- {} {} group 1\n", util::iso_at(util::epoch_secs() - started_ago), crate::cycle::STARTED_BY);
        store.save_task(&t).unwrap();
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["m-2".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();
        let tree = git_tree(env, "mtree");
        store.set_claim("m-2", json!({ "workspace": "cpd-2" }));
        store.set_binding("cpd-2", json!({ "task_id": "m-2", "pane_id": "cpd-2", "cwd": tree.display().to_string() }));
        (store, tree)
    }

    fn member_tick(store: &Store, state: Option<State>) {
        crate::repair::tick(store, &Fake(RefCell::new(state), false), &mut crate::repair::Pass::new());
    }

    /// Move the notice the tick wrote to long ago: the second window has passed.
    fn age_notice(store: &Store, id: &str) {
        let mut t = store.find_task(id).unwrap();
        let log = t.section("Log").unwrap_or_default();
        let aged: Vec<String> = log
            .lines()
            .map(|l| if l.contains("wsp: this member's seat reads") { format!("- 2026-01-01T00:00:00Z{}", &l[l.find(' ').map(|i| i + 21).unwrap_or(0)..]) } else { l.to_string() })
            .collect();
        crate::model::set_section_in(&mut t.body, "Log", &format!("## Log\n\n{}\n", aged.join("\n")));
        store.save_task(&t).unwrap();
    }

    fn member_status(store: &Store) -> Status {
        store.find_task("m-2").unwrap().status()
    }

    #[test]
    fn a_member_seat_idle_with_its_verdict_written_is_closed_by_wsp_timed_to_the_write() {
        let env = util::isolated("closing-m-idle");
        let (store, _tree) = staged_member(&env, 3600);
        let wrote = put(&store, "m-2", Kind::Member, "Built the thing; nothing left\n");
        clear_told();
        member_tick(&store, Some(State::Idle));
        assert_eq!(member_status(&store), Status::Doing, "told first, not closed");
        assert!(told().iter().any(|(_, t)| t.contains("wsp review m-2") && t.contains("$WSP_VERDICT")), "the exact command and the file: {:?}", told());
        age_notice(&store, "m-2");
        member_tick(&store, Some(State::Idle));
        assert_eq!(member_status(&store), Status::Review, "wsp ran the review for the seat");
        let log = store.find_task("m-2").unwrap().section("Log").unwrap();
        assert!(log.contains("Built the thing"), "the account is the file: {log}");
        assert!(log.contains("closed by wsp: seat idle") && log.contains(&format!("verdict written at {}", clock(wrote))), "timed to the write: {log}");
        assert!(raised_texts(&store).is_empty(), "nothing to flag");
    }

    #[test]
    fn a_member_seat_idle_with_no_verdict_is_told_once_then_escalated_with_evidence() {
        let env = util::isolated("closing-m-noverdict");
        let (store, _tree) = staged_member(&env, 3600);
        clear_told();
        member_tick(&store, Some(State::Idle));
        member_tick(&store, Some(State::Idle));
        let tells = told().into_iter().filter(|(id, _)| id == "m-2").count();
        assert_eq!(tells, 1, "told once: {:?}", told());
        assert!(raised_texts(&store).is_empty(), "not flagged inside the first window");
        age_notice(&store, "m-2");
        member_tick(&store, Some(State::Idle));
        assert_ne!(member_status(&store), Status::Review, "no verdict, no guess");
        let raised = raised_texts(&store);
        assert_eq!(raised.len(), 1, "the person is flagged: {raised:?}");
        assert!(raised[0].contains("no verdict at") && raised[0].contains("clean commit"), "{}", raised[0]);
        assert!(governor_told().iter().any(|t| t.contains("not closing") && t.contains("no verdict")), "the governor has the evidence: {:?}", governor_told());
    }

    #[test]
    fn a_member_session_that_ended_is_closed_at_once_when_the_verdict_is_there_and_flagged_when_not() {
        {
            let env = util::isolated("closing-m-ended");
            let (store, _tree) = staged_member(&env, 3600);
            put(&store, "m-2", Kind::Member, "Done\n");
            member_tick(&store, Some(State::Gone));
            assert_eq!(member_status(&store), Status::Review);
            assert!(store.find_task("m-2").unwrap().section("Log").unwrap().contains("closed by wsp: session ended"));
        }
        let env = util::isolated("closing-m-ended-dirty");
        let (store, tree2) = staged_member(&env, 3600);
        put(&store, "m-2", Kind::Member, "Done\n");
        std::fs::write(tree2.join("f"), "x").unwrap();
        std::process::Command::new("git").arg("-C").arg(&tree2).args(["add", "f"]).status().unwrap();
        member_tick(&store, Some(State::Gone));
        assert_ne!(member_status(&store), Status::Review, "a dirty tree is not reviewed over");
        let raised = raised_texts(&store);
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert!(raised[0].contains("uncommitted") && raised[0].contains("session ended"), "{}", raised[0]);
        member_tick(&store, Some(State::Gone));
        assert_eq!(raised_texts(&store).len(), 1, "flagged once");
    }

    #[test]
    fn a_member_verdict_from_an_earlier_run_is_ignored() {
        let env = util::isolated("closing-m-stale");
        // The row's run started a minute from now: the file was written before it.
        let (store, _tree) = staged_member(&env, -60);
        put(&store, "m-2", Kind::Member, "Done by the last run\n");
        assert!(fresh_verdict(&store, "m-2", Kind::Member, crate::repair::started_at(&store.find_task("m-2").unwrap())).is_none());
        let args = crate::Args::synth("review", &["m-2"], &[]);
        assert!(member_args(&store, &args).is_none(), "the verb does not read it");
        assert_eq!(crate::cmd_task::review(&store, &args), 2, "and so asks for the account");
        member_tick(&store, Some(State::Gone));
        assert_ne!(member_status(&store), Status::Review);
        assert!(raised_texts(&store).iter().any(|t| t.contains("no verdict at")), "{:?}", raised_texts(&store));
    }

    #[test]
    fn review_reads_the_known_verdict_when_given_no_account() {
        let env = util::isolated("closing-m-verb");
        let (store, _tree) = staged_member(&env, 3600);
        put(&store, "m-2", Kind::Member, "Read from the file\n");
        assert_eq!(crate::cmd_task::review(&store, &crate::Args::synth("review", &["m-2"], &[])), 0);
        assert_eq!(member_status(&store), Status::Review);
        assert!(store.find_task("m-2").unwrap().section("Log").unwrap().contains("Read from the file"));
    }

    // -- a verifier

    /// A member `v-1` at review with one pass reading in `cpd-3`, opened an hour ago.
    fn staged_verifier(env: &util::Isolated, opened_ago: i64) -> Store {
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a member", "v-1");
        t.set_status(Status::Review);
        t.caption = Some("Did the thing".into());
        t.body = "## Overview\nthe work\n\n## Log\n\n- 2026-11-05 review: done\n".to_string();
        let mut pass = crate::verification::Pass::opened(None, None);
        pass.at = util::iso_at(util::epoch_secs() - opened_ago);
        pass.pane = Some("cpd-3".into());
        crate::verification::write(&mut t, &[pass]);
        store.save_task(&t).unwrap();
        store.set_binding("cpd-3", json!({ "pane_id": "cpd-3" }));
        store
    }

    fn pass_state(store: &Store) -> PassState {
        crate::verification::passes(&store.find_task("v-1").unwrap()).remove(0).state
    }

    #[test]
    fn a_verifier_seat_idle_with_a_verdict_written_is_closed_by_wsp_timed_to_the_write() {
        let env = util::isolated("closing-v-idle");
        let store = staged_verifier(&env, 3600);
        let fake = Fake(RefCell::new(Some(State::Idle)), false);
        let wrote = put(&store, "v-1", Kind::Verifier, "holds\nThe wiring is tested now.\nIt was read in the tree.\n");
        clear_told();
        let t0 = wrote + 10;
        tick_at(&store, &fake, t0);
        tick_at(&store, &fake, t0 + TELL_AFTER);
        assert_eq!(told().len(), 1, "told once: {:?}", told());
        assert!(told()[0].1.contains("wsp verified v-1") && told()[0].1.contains("$WSP_VERDICT"), "{:?}", told());
        assert_eq!(pass_state(&store), PassState::Running);
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW);
        assert_eq!(pass_state(&store), PassState::Holds, "wsp recorded the verdict");
        let t = store.find_task("v-1").unwrap();
        let p = crate::verification::passes(&t).remove(0);
        assert!(p.text.contains("wiring is tested") && !p.text.starts_with("holds"), "the body, not the word: {}", p.text);
        assert_eq!(p.caption.as_deref(), Some("The wiring is tested now."));
        let log = t.section("Log").unwrap();
        assert!(log.contains("closed by wsp: seat idle") && log.contains(&format!("verdict written at {}", clock(wrote))), "{log}");
    }

    #[test]
    fn a_verifier_seat_idle_with_no_verdict_is_told_once_then_escalated_and_never_recorded() {
        let env = util::isolated("closing-v-noverdict");
        let store = staged_verifier(&env, 3600);
        let fake = Fake(RefCell::new(Some(State::Idle)), false);
        clear_told();
        let t0 = util::epoch_secs();
        tick_at(&store, &fake, t0);
        tick_at(&store, &fake, t0 + TELL_AFTER);
        tick_at(&store, &fake, t0 + TELL_AFTER + 30);
        assert_eq!(told().len(), 1, "told once");
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW);
        assert_eq!(pass_state(&store), PassState::Running, "no guess");
        let raised = raised_texts(&store);
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert!(raised[0].contains("no verdict at"), "{}", raised[0]);
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW + 600);
        assert_eq!(raised_texts(&store).len(), 1, "flagged once");
    }

    #[test]
    fn a_verifier_session_that_ended_is_closed_at_once_when_the_verdict_parses_and_flagged_when_it_does_not() {
        {
            let env = util::isolated("closing-v-ended");
            let store = staged_verifier(&env, 3600);
            put(&store, "v-1", Kind::Verifier, "holds\nIt holds.\n");
            tick_at(&store, &Fake(RefCell::new(Some(State::Gone)), false), util::epoch_secs());
            assert_eq!(pass_state(&store), PassState::Holds);
            assert!(store.find_task("v-1").unwrap().section("Log").unwrap().contains("closed by wsp: session ended"));
        }
        let env = util::isolated("closing-v-ended-bad");
        let store = staged_verifier(&env, 3600);
        put(&store, "v-1", Kind::Verifier, "It probably holds, I think.\n");
        tick_at(&store, &Fake(RefCell::new(None), true), util::epoch_secs());
        assert_eq!(pass_state(&store), PassState::Running, "a file that is not a verdict is not guessed at");
        let raised = raised_texts(&store);
        assert!(raised.len() == 1 && raised[0].contains("does not parse") && raised[0].contains("session ended"), "{raised:?}");
    }

    #[test]
    fn a_verifier_verdict_from_an_earlier_pass_is_ignored() {
        let env = util::isolated("closing-v-stale");
        // This pass opened a minute from now: the file was written before it.
        let store = staged_verifier(&env, -60);
        put(&store, "v-1", Kind::Verifier, "holds\nThe last pass's.\n");
        assert!(verifier_args(&store, &crate::Args::synth("verified", &["v-1"], &[]), Some("cpd-3")).is_none());
        tick_at(&store, &Fake(RefCell::new(Some(State::Gone)), false), util::epoch_secs());
        assert_eq!(pass_state(&store), PassState::Running, "an earlier run's verdict is not this one's");
        assert!(raised_texts(&store)[0].contains("no verdict at"));
    }

    #[test]
    fn verified_reads_the_known_verdict_when_given_none_and_a_typed_word_wins() {
        let env = util::isolated("closing-v-verb");
        let store = staged_verifier(&env, 3600);
        put(&store, "v-1", Kind::Verifier, "holds\nBut I found a gap.\n");
        let args = crate::Args::synth("verified", &["v-1"], &[("blocks", "true"), ("caption", "a gap")]);
        let (a, _) = verifier_args(&store, &args, Some("cpd-3")).unwrap();
        assert!(a.has("blocks") && !a.has("holds"), "the word typed wins");
        assert_eq!(a.get("caption").as_deref(), Some("a gap"));
    }

    // -- a barrier check

    /// A running list `run` whose group 1 has a check open in `cpd-4`.
    fn staged_check(env: &util::Isolated) -> Store {
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["c-1".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();
        let mut t = Task::new("c", "c-1");
        t.set_status(Status::Done);
        store.save_task(&t).unwrap();
        let w = store.worklist("run").unwrap();
        assert!(crate::barrier::open_within(&store, &w, 1, None, None, crate::barrier::Mechanical::No, &|_: &str| false, &|| true).is_some());
        assert!(crate::barrier::name_seat(&store, "run", 1, "cpd-4"));
        store.set_binding("cpd-4", json!({ "pane_id": "cpd-4" }));
        store
    }

    fn check_key() -> String {
        crate::barrier::subject("run", 1)
    }

    fn held(store: &Store) -> bool {
        store.worklist("run").unwrap().status() == WorklistStatus::Held
    }

    #[test]
    fn a_check_seat_idle_with_a_verdict_written_is_closed_by_wsp_timed_to_the_write() {
        let env = util::isolated("closing-c-idle");
        let store = staged_check(&env);
        let fake = Fake(RefCell::new(Some(State::Idle)), false);
        let wrote = put(&store, &check_key(), Kind::Check, "hold\nThe replay is short.\nThe replay covers one of three cases.\n");
        clear_told();
        let t0 = wrote + 10;
        tick_at(&store, &fake, t0);
        tick_at(&store, &fake, t0 + TELL_AFTER);
        assert_eq!(told().len(), 1, "told once: {:?}", told());
        assert!(told()[0].1.contains("wsp worklist go run") && told()[0].1.contains("$WSP_VERDICT"), "{:?}", told());
        assert!(!held(&store));
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW);
        assert!(held(&store), "wsp ran the hold for the seat");
        let log = store.worklist("run").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("The replay covers one of three cases"), "{log}");
        let cycle = std::fs::read_to_string(store.state.join("cycle.log")).unwrap_or_default();
        assert!(cycle.contains("closed by wsp: seat idle") && cycle.contains(&format!("verdict written at {}", clock(wrote))), "{cycle}");
    }

    #[test]
    fn a_check_seat_idle_with_no_verdict_is_told_once_then_escalated_and_nothing_is_passed_or_held() {
        let env = util::isolated("closing-c-noverdict");
        let store = staged_check(&env);
        let fake = Fake(RefCell::new(Some(State::Idle)), false);
        clear_told();
        let t0 = util::epoch_secs();
        tick_at(&store, &fake, t0);
        tick_at(&store, &fake, t0 + TELL_AFTER);
        tick_at(&store, &fake, t0 + TELL_AFTER + 30);
        assert_eq!(told().len(), 1, "told once");
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW);
        assert!(!held(&store));
        let raised = raised_texts(&store);
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert!(raised[0].contains("no verdict at"), "{}", raised[0]);
        assert!(governor_told().iter().any(|t| t.contains("not closing")), "{:?}", governor_told());
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW + 600);
        assert_eq!(raised_texts(&store).len(), 1, "flagged once");
    }

    #[test]
    fn a_check_session_that_ended_is_closed_at_once_when_the_verdict_parses_and_flagged_when_it_does_not() {
        {
            let env = util::isolated("closing-c-ended");
            let store = staged_check(&env);
            put(&store, &check_key(), Kind::Check, "hold\nWhy not.\n");
            tick_at(&store, &Fake(RefCell::new(Some(State::Gone)), false), util::epoch_secs());
            assert!(held(&store));
        }
        // A follow-up names rows wsp cannot guess; it is flagged, not closed.
        let env = util::isolated("closing-c-ended-followup");
        let store = staged_check(&env);
        put(&store, &check_key(), Kind::Check, "followup\nPass it, with these owed.\n");
        tick_at(&store, &Fake(RefCell::new(None), true), util::epoch_secs());
        assert!(!held(&store));
        let raised = raised_texts(&store);
        assert!(raised.len() == 1 && raised[0].contains("names rows"), "{raised:?}");
    }

    #[test]
    fn a_check_verdict_from_an_earlier_check_is_ignored() {
        let env = util::isolated("closing-c-stale");
        let store = staged_check(&env);
        put(&store, &check_key(), Kind::Check, "hold\nThe last check's.\n");
        // Open the entry a minute from now: the file was written before it.
        let mut w = store.worklist("run").unwrap();
        let mut groups = w.groups();
        groups[0].checks.last_mut().unwrap().at = util::iso_at(util::epoch_secs() + 60);
        w.set_groups(&groups);
        store.save_worklist(&w).unwrap();
        tick_at(&store, &Fake(RefCell::new(Some(State::Gone)), false), util::epoch_secs());
        assert!(!held(&store), "an earlier run's verdict is not this one's");
        assert!(raised_texts(&store)[0].contains("no verdict at"));
    }

    #[test]
    fn go_and_hold_read_the_known_verdict_only_for_their_own_word() {
        let env = util::isolated("closing-c-verb");
        let store = staged_check(&env);
        put(&store, &check_key(), Kind::Check, "hold\nNot yet.\n");
        let w = store.worklist("run").unwrap();
        let args = crate::Args::synth("worklist", &["go", "run"], &[]);
        assert!(check_args(&store, &args, "go", &w).is_none(), "a hold is not a go");
        assert!(check_args(&store, &crate::Args::synth("worklist", &["hold", "run"], &[]), "hold", &w).is_some());
    }

    #[test]
    fn a_verdict_parses_to_a_word_and_a_body_and_nothing_else_does() {
        let v = parse_verdict(Kind::Verifier, "Verdict: Holds.\n\nIt does.\nmore").unwrap();
        assert_eq!((v.word.as_deref(), v.caption.as_str(), v.body.as_str()), (Some("holds"), "It does.", "It does.\nmore"));
        assert!(parse_verdict(Kind::Verifier, "holds").is_none(), "a word with nothing behind it");
        assert!(parse_verdict(Kind::Verifier, "go\nx").is_none(), "another kind's word");
        assert!(parse_verdict(Kind::Check, "followup\nx").is_some());
        assert!(parse_verdict(Kind::Member, "  \n").is_none());
        assert_eq!(parse_verdict(Kind::Member, "# Did it\nmore").unwrap().caption, "Did it");
    }
}
