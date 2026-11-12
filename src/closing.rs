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
//!   its own Bash call ([`how_to_close`]; since `wsp-617` the exit into
//!   `$WSP_EXIT` and `wsp submit`, which is `leg done` for a leg).
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
//!
//! # At the stop, and at a prompt (`wsp-616`, row E5)
//!
//! - **A turn that stops with no exit accepted is told at once** what is
//!   missing — no exit at `$WSP_EXIT`, or the dry run's refusal lines for the
//!   file as it stands — then `wsp submit` ([`exit_tell`]). On the seat's own
//!   `Stop` (one from before its start is not its), when the turn wants
//!   nobody, once per stop; not [`TELL_AFTER`] later. The window to wsp's own
//!   close runs from the first tell, as before. A turn that wants somebody is
//!   `repair`'s to raise (`wsp-606`).
//! - **A verifier or a check at an on-screen prompt is raised at once**,
//!   through [`crate::waiting::route`] with the prompt's text
//!   ([`crate::waiting::raise_prompt`]), and is never told or closed while it
//!   stands there. A member's is raised the same way by `repair`.
//! - **Closing for the seat reads the exit first** ([`submit_exit`]): a file
//!   written as an exit is submitted as `wsp submit` would, when it validates;
//!   one that does not is a flag to the person with its refusal lines. A
//!   prose verdict is closed by its old verb, as above.

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

/// The words every order about closing carries (wsp-372, one line since
/// `wsp-617`): the exit from a heredoc into `$WSP_EXIT` in one call, `wsp
/// submit --dry-run` once, then `wsp submit` in a call of its own, never
/// chained. A leg's submit is its `leg done`; the last leg's ends the row.
/// ASCII, because a leg's order goes out in argv.
pub(crate) fn how_to_close(task: &str, last: bool) -> String {
    let what = match last {
        true => "it ends the row, as `wsp review` did",
        false => "it closes the leg, as `wsp leg done` did, with the file as the baton",
    };
    format!("{} ({what}).", submit_words(task))
}

/// The one line of how a seat closes on `row`: write, dry-run, submit.
pub(crate) fn submit_words(row: &str) -> String {
    format!(
        "Close in three Bash calls, none chained: write it with a heredoc, `cat > \"$WSP_EXIT\" <<'EOF'` ... `EOF` \
         (never a bare `cat`); `wsp submit {row} --dry-run` once, fixing what it refuses; then `wsp submit {row}` alone"
    )
}

// ---- the exit template every packet ends with (`wsp-617`, row E6) --------------

/// The template a packet ends with for a seat of `kind` (docs/exit-packet.md
/// §2): its sections in order, ready to fill, then how to close. `done` is the
/// row's own done-when items ([`crate::cmd_submit::packet_items`]), pre-filled
/// as `- held: <item> — ` for the seat to finish or flip; with none, the line's
/// shape. `row` is what `wsp submit` takes (a check's is its list). Under
/// [`TEMPLATE_MAX`] bytes before the items.
pub(crate) fn exit_template(kind: crate::exit::Kind, row: &str, done: &[String]) -> String {
    use crate::exit::Kind as K;
    let done_lines = |evidence: &str| match done.is_empty() {
        true => format!("- held: <item, as the packet says it> — <{evidence}>\n- not held: <item> — <why>\n"),
        false => done.iter().map(|i| format!("- held: {} — \n", fold_line(i))).collect(),
    };
    let checks = |word: &str, ran: &str| format!("{word}:\n- <label> (<time>): <command>{ran}\n\n");
    let (body, note) = match kind {
        K::Member => (
            format!(
                "summary: <under 100 chars: what is true now>\n\n<the account: what you did and left>\n\n\
                 done-when:\n{}\n{}asks:\n- governor: <a decision for the governor>\n\
                 - person: <a decision only the person can make>\n- note: <it asks nothing>\n\nowed: nothing\n",
                done_lines("a test name, file:line"),
                checks("checks", "")
            ),
            "Finish or flip each `held:`; keep the asks you have; `owed:` and `- <item>` lines if any is left.",
        ),
        K::Leg => (
            format!(
                "summary: <one line: what is true now>\n\n<the account: what the next leg needs>\n\n{}owed: nothing\n",
                checks("checks", "")
            ),
            "`owed:` and `- <item>` lines for what the next leg is left; `done-when:` is optional.",
        ),
        K::Verifier => (
            format!(
                "verdict: holds | blocks\nsummary: <one line>\n\n<the verdict in prose>\n\ndone-when:\n{}\n{}\
                 cause: work | check | tree | env — <one sentence>\n\nowed:\n- <what the member owes>\n",
                done_lines("the evidence you read or ran"),
                checks("checked", " — pass | fail")
            ),
            "On `holds`: no `cause:`, and `owed: nothing`. `cause: env` (you could not verify) blocks nothing.",
        ),
        K::Check => (
            format!(
                "verdict: go | hold | followup\nsummary: <one line>\n\n<the verdict in prose>\n\n{}\
                 cause: work | check | tree | env — <one sentence>\n\nowed: nothing\n",
                checks("checked", " — pass | fail")
            ),
            "`cause:` only on `hold`; a `followup` is a flag to the person.",
        ),
    };
    let close = match kind {
        K::Leg => how_to_close(row, false),
        _ => format!("{}.", submit_words(row)),
    };
    format!("## Your exit\n\nTo `$WSP_EXIT`, in this order, each at a line's start:\n```text\n{body}```\n{note}\n{close}\n")
}

/// The most an exit template may cost a packet, before its done-when items.
pub(crate) const TEMPLATE_MAX: usize = 800;

/// A done-when item on one line, so the template keeps one line per item.
fn fold_line(item: &str) -> String {
    item.split_whitespace().collect::<Vec<_>>().join(" ")
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

    /// What the seat says it is in `$WSP_SEAT_KIND` (`wsp-538`): the build shim
    /// holds only a member to its row's own checks.
    pub(crate) fn seat_word(self) -> &'static str {
        match self {
            Kind::Member => "member",
            Kind::Verifier => "verifier",
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
    // Written as an exit (`wsp-614`), its summary is the line a glance reads.
    let caption = match body.lines().find_map(|l| l.strip_prefix("summary:")).map(str::trim).filter(|s| !s.is_empty()) {
        Some(summary) if crate::cmd_submit::looks_like_exit(&body) => util::truncate(&crate::util::fold(summary), 100),
        _ => caption_of(&body),
    };
    (kind.words().contains(&word.as_str()) && !body.is_empty()).then_some(Verdict { word: Some(word), caption, body })
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

// ---- the exit at a seat's stop (`wsp-616`) ------------------------------------

/// How a seat's exit file (`$WSP_EXIT`, the kind's [`verdict_path`]) reads as
/// it stands: what the tell at its stop says.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ExitReads {
    /// Nothing written in this run of the seat.
    Missing,
    /// Written, and `wsp submit --dry-run` refuses it: every refusal line.
    Refused(Vec<String>),
    /// Written, and it validates: only the submit is missing.
    Valid,
}

/// [`ExitReads`] for `key`'s exit of `kind`, written since `start`.
pub(crate) fn exit_reads(store: &Store, kind: Kind, key: &str, start: Option<i64>) -> ExitReads {
    if fresh_verdict(store, key, kind, start).is_none() {
        return ExitReads::Missing;
    }
    let text = std::fs::read_to_string(verdict_path(store, key, kind)).unwrap_or_default();
    if text.trim().is_empty() {
        return ExitReads::Missing;
    }
    match crate::cmd_submit::dry_run(store, kind, key, &text) {
        Ok(()) => ExitReads::Valid,
        Err(lines) => ExitReads::Refused(lines),
    }
}

/// The row `wsp submit` takes for `key`: a check's is its list.
fn submit_row(kind: Kind, key: &str) -> String {
    match kind {
        Kind::Check => crate::barrier::parse_subject(key).map(|(l, _)| l.to_string()).unwrap_or_else(|| key.to_string()),
        _ => key.to_string(),
    }
}

/// What a seat whose turn stopped with no exit accepted is told (`wsp-616`):
/// what is missing — no exit at `$WSP_EXIT`, or the dry run's refusal lines
/// for the file as it stands — then the one command. `what` names the open
/// item, `window` is the minutes until wsp acts on the file itself.
pub(crate) fn exit_tell(store: &Store, kind: Kind, key: &str, start: Option<i64>, what: &str, window: i64) -> String {
    let path = util::contract(&verdict_path(store, key, kind));
    let state = match exit_reads(store, kind, key, start) {
        ExitReads::Missing => format!(
            "There is no exit at {path} ($WSP_EXIT): write it with a heredoc, `cat > \"$WSP_EXIT\" <<'EOF'` … `EOF` \
             (a heredoc with its text, never a bare `cat`)."
        ),
        ExitReads::Refused(lines) => format!(
            "The exit at {path} ($WSP_EXIT) is refused as it stands — fix these lines:\n{}\n",
            lines.iter().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
        ),
        ExitReads::Valid => format!("The exit at {path} ($WSP_EXIT) validates, and it is not submitted."),
    };
    let row = submit_row(kind, key);
    format!(
        "wsp: {what} is open, and this seat's turn has stopped with no exit accepted. {state} \
         Then run, as a Bash call of its own and never chained: `wsp submit {row}` (`--dry-run` checks the file first). \
         If wsp finds it still open in {window} min it submits that file for you when it validates, or flags the person."
    )
}

/// **Closing for the seat reads the exit first** (`wsp-616`, S3): a file
/// written as an exit in this run is submitted for the seat when it validates,
/// exactly as `wsp submit` would; one that does not is the refusal lines, for
/// the flag. `None` where there is no exit-shaped file, and the older verdict
/// close decides.
fn submit_exit(store: &Store, kind: Kind, key: &str, seat: &str, start: Option<i64>, why: Why) -> Option<Result<(), String>> {
    let written = fresh_verdict(store, key, kind, start)?;
    let path = verdict_path(store, key, kind);
    let text = std::fs::read_to_string(&path).ok()?;
    if !crate::cmd_submit::looks_like_exit(&text) {
        return None;
    }
    let row = submit_row(kind, key);
    if let Err(lines) = crate::cmd_submit::dry_run(store, kind, key, &text) {
        return Some(Err(format!("the exit is refused as it stands: {}", lines.join("; "))));
    }
    let args = crate::Args::synth("submit", &[row.as_str()], &[("from", &path.display().to_string())]);
    let code = crate::cmd_submit::submit_from(store, &args, Some(kind.seat_word()), Some(seat.to_string()));
    if code != 0 {
        crate::store::log_line(store, &format!("{row}: wsp's `wsp submit {row}` for {seat} was refused (exit {code})"));
        return Some(Err(format!("wsp ran `wsp submit {row}` from the exit and it was refused (exit {code})")));
    }
    note_close(store, &row, "submit", written, Some(&why.words()));
    Some(Ok(()))
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
    /// The seat's `Stop` it was last told at (`wsp-616`): one tell per stop.
    stop: Option<i64>,
    /// Its on-screen prompt was raised (`wsp-616`): once per spell at it.
    prompted: bool,
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
                stop: e["stop"].as_i64(),
                prompted: e["prompted"].as_bool().unwrap_or(false),
            },
        );
    }
    out
}

fn write(store: &Store, seen: &std::collections::BTreeMap<String, Seen>) {
    let v: serde_json::Map<String, Value> = seen
        .iter()
        .map(|(k, s)| (k.clone(), json!({ "idle_since": s.idle_since, "told": s.told, "flagged": s.flagged, "stop": s.stop, "prompted": s.prompted })))
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
            let start = l.started.as_deref().map(util::epoch_of);
            let item = Item { key: format!("{}/{}", t.id, l.name), row: &t.id, seat: &seat, written: baton, start, raise: None };
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
            let what = format!("the {} verifier's pass", t.id);
            let raise = Raise { about: crate::message::About::Task(t.id.clone()), what: what.clone(), list: crate::cycle::list_of(store, t) };
            let item = Item { key: format!("verify/{}/{seat}", t.id), row: &t.id, seat: &seat, written, start, raise: Some(raise) };
            let tell = |idle: i64| tell_verdict(store, Kind::Verifier, &t.id, &seat, idle, &what, start);
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
            let what = format!("the {} barrier {at} check", w.id);
            let raise = Raise { about: crate::message::About::Scope(w.id.clone()), what: what.clone(), list: Some(w.clone()) };
            let item = Item { key: format!("check/{key}"), row: &w.id, seat: &seat, written, start, raise: Some(raise) };
            let tell = |idle: i64| tell_verdict(store, Kind::Check, &key, &seat, idle, &what, start);
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
    /// When its seat started on it: a `Stop` before this is an earlier seat's.
    start: Option<i64>,
    /// How its seat's on-screen prompt is raised: a verifier's and a check's
    /// (`wsp-616`). `None` for a leg, which is left as it was.
    raise: Option<Raise>,
}

/// What a run seat at an on-screen prompt is raised about.
struct Raise {
    about: crate::message::About,
    what: String,
    /// The run the seat is in.
    list: Option<Worklist>,
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
    // **A run seat at an on-screen prompt is raised at once** (`wsp-616`,
    // compounding-449): an AskUserQuestion or a permission dialog is read by
    // nobody on a screen nobody watches. Through [`crate::waiting::route`],
    // with the prompt's text, once per spell at it — and never told or closed
    // while it stands there.
    if let (Some(State::Blocked), Some(raise)) = (state, item.raise.as_ref()) {
        if !entry.prompted {
            let screen = seats.screen(item.seat);
            crate::waiting::raise_prompt(store, item.seat, raise.about.clone(), &raise.what, screen.as_deref(), raise.list.as_ref());
            *entry = Seen { prompted: true, ..Seen::default() };
        }
        return;
    }
    entry.prompted = false;
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
            // **Told at its stop, not a window after** (`wsp-616`): a turn
            // that ended in this seat's run wanting nobody, with the item
            // still open, has stopped short of its exit. Once per stop; the
            // window to wsp's own close runs from the first tell, unchanged.
            let stop = seats.turn_end(item.seat).filter(|e| !e.wants() && item.start.is_none_or(|s| e.at >= s));
            if let Some(stop) = stop.filter(|e| entry.stop != Some(e.at)) {
                entry.stop = Some(stop.at);
                let since = *entry.idle_since.get_or_insert(stop.at.min(now));
                if entry.told.is_none() {
                    entry.told = Some(now);
                }
                tell((now - since) / 60);
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
    // An exit-shaped baton that would be refused says so now, with its lines,
    // as `exit_tell` does for the other seats (`wsp-616`).
    let refused = match leg_exit(store, row, &std::fs::read_to_string(&baton).unwrap_or_default()) {
        Err(lines) => format!(
            " The baton is written as an exit and is refused as it stands — fix these lines first:\n{}\n",
            lines.iter().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
        ),
        Ok(_) => String::new(),
    };
    let text = format!(
        "wsp: leg {leg} of {row} is still open, and this seat has sat {idle_min} min without closing it.{refused} \
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
            let head = wsp_base::calls::Command::new("git")
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

/// A leg's baton read as its exit: `Ok(None)` for a baton in prose, the
/// exit's summary when it validates as a leg's, else every refusal line
/// ([`crate::cmd_submit::leg_baton`], the rules `wsp leg done` runs).
fn leg_exit(store: &Store, row: &str, baton: &str) -> Result<Option<String>, Vec<String>> {
    let Some(t) = store.find_task(row) else { return Ok(None) };
    crate::cmd_submit::leg_baton(store, &t, baton).map_err(|why| {
        let lines: Vec<String> = why.lines().skip(1).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        if lines.is_empty() { vec![why] } else { lines }
    })
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
    // A baton written as an exit runs the leg's exit validator first, as
    // `wsp leg done` and `wsp submit` do (`wsp-616`, S3): refused, it is a flag
    // with its refusal lines and the leg stays open; valid, its summary is the caption.
    let caption = match leg_exit(store, row, &baton) {
        Err(lines) => {
            let said = format!("the baton is an exit and it is refused as it stands: {}", lines.join("; "));
            crate::store::log_line(store, &format!("leg {leg} of {row}: wsp's close for {seat} not run — {said}"));
            flag(store, row, leg, seat, &evidence(&format!("wsp does not close over a refused exit, so it is not closed — {said}")));
            return true;
        }
        Ok(Some(summary)) => util::truncate(&crate::util::fold(&summary), 100),
        Ok(None) => caption_of(&baton),
    };
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
fn tell_verdict(store: &Store, kind: Kind, key: &str, seat: &str, idle_min: i64, what: &str, start: Option<i64>) {
    let text = exit_tell(store, kind, key, start, what, SECOND_WINDOW / 60);
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
    match submit_exit(store, Kind::Member, id, seat, start, why) {
        Some(Ok(())) => return true,
        Some(Err(refused)) => {
            let evidence = verdict_evidence(store, id, Kind::Member, written, &tree, why, &refused);
            flag_member(store, t, seat, &evidence, start, !matches!(why, Why::Ended));
            return false;
        }
        None => {}
    }
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
    match submit_exit(store, Kind::Verifier, member, seat, start, why) {
        Some(Ok(())) => return true,
        Some(Err(refused)) => {
            let evidence = verdict_evidence(store, member, Kind::Verifier, written, &tree, why, &refused);
            flag_about(store, member, "the verifier", seat, &evidence, &fix, None, true);
            return true;
        }
        None => {}
    }
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
    match submit_exit(store, Kind::Check, &key, seat, start, why) {
        Some(Ok(())) => return true,
        Some(Err(refused)) => {
            let evidence = verdict_evidence(store, &key, Kind::Check, written, &tree, why, &refused);
            flag_about(store, &w.id, &what, seat, &evidence, &fix, Some(w), true);
            return true;
        }
        None => {}
    }
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
            let ok = wsp_base::calls::Command::new("git").arg("-C").arg(&tree).args(&args).status().unwrap();
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
        wsp_base::calls::Command::new("git").arg("-C").arg(&tree).args(["add", "f"]).status().unwrap();
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
        wsp_base::calls::Command::new("git").arg("-C").arg(&tree).args(["add", "f"]).status().unwrap();
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

    /// `staged`'s row with a second leg `finish` waiting on `build`, so
    /// `build` is not the row's last.
    fn staged_two(env: &util::Isolated) -> (Store, PathBuf) {
        let (store, tree) = staged(env);
        let mut t = store.find_task("m-1").unwrap();
        let mut ls = legs::legs(&t);
        ls.push(Leg::pending("finish", "finish it"));
        legs::write(&mut t, &ls);
        store.save_task(&t).unwrap();
        (store, tree)
    }

    fn submit_leg(env: &util::Isolated, store: &Store, text: &str) -> i32 {
        let file = env.path("exit.md");
        std::fs::write(&file, text).unwrap();
        let from = file.display().to_string();
        let args = crate::Args::synth("submit", &["m-1"], &[("from", &from)]);
        crate::cmd_submit::submit_from(store, &args, Some("leg"), Some("cpd-1".into()))
    }

    const LEG_EXIT: &str = "summary: the store half is built\n\nThe store reads and writes; the panel is the next leg's.\n\nchecks:\n- unit (2m): true\n\nowed:\n- the panel\n";

    #[test]
    fn a_leg_seats_submit_closes_the_leg_as_leg_done_does_with_the_file_as_its_baton() {
        let env = util::isolated("closing-submit-leg");
        let (store, _tree) = staged_two(&env);
        assert_eq!(submit_leg(&env, &store, LEG_EXIT), 0);
        let t = store.find_task("m-1").unwrap();
        let ls = legs::legs(&t);
        assert_eq!(ls[0].state, LegState::Done, "the leg is closed");
        assert_eq!(ls[0].caption.as_deref(), Some("the store half is built"), "the summary is its caption");
        assert!(ls[0].baton.contains("The store reads and writes") && ls[0].baton.contains("- the panel"), "the file is the baton: {}", ls[0].baton);
        assert!(t.section("Log").unwrap().contains("leg build ended"), "{}", t.section("Log").unwrap());
        assert_eq!(ls[1].state, LegState::Pending, "the next leg is the run's to start");
    }

    #[test]
    fn a_leg_exit_that_does_not_validate_leaves_the_leg_open_and_leg_done_runs_the_same_rules() {
        let env = util::isolated("closing-submit-leg-bad");
        let (store, _tree) = staged_two(&env);
        let no_owed = "summary: the store half is built\n\nThe store reads and writes.\n";
        assert_eq!(submit_leg(&env, &store, no_owed), 1);
        assert!(running(&store), "nothing moved");
        // `wsp leg done` with the same file as its baton: refused the same way.
        write_baton(&store, no_owed);
        let args = crate::Args::synth("leg", &["done", "m-1"], &[]);
        assert_eq!(crate::leg_seat::cmd_done(&store, &args, Some("cpd-1")), 1);
        assert!(running(&store));
        // A baton in prose is taken as it always was.
        write_baton(&store, "handed on\n");
        let args = crate::Args::synth("leg", &["done", "m-1"], &[("caption", "built")]);
        assert_eq!(crate::leg_seat::cmd_done(&store, &args, Some("cpd-1")), 0);
    }

    #[test]
    fn wsps_close_of_a_leg_runs_the_leg_exit_validator_a_refused_exit_is_flagged_with_its_lines_and_stays_open() {
        let env = util::isolated("closing-settle-leg-refused");
        let (store, _tree) = staged_two(&env);
        write_baton(&store, "summary: the store half is built\n\nThe store reads and writes.\n");
        // Idle: the tell already names the refusal lines.
        let idle = Fake(RefCell::new(Some(State::Idle)), false);
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().clear());
        let t0 = util::epoch_secs();
        tick_at(&store, &idle, t0);
        tick_at(&store, &idle, t0 + TELL_AFTER);
        assert_eq!(told().len(), 1, "{:?}", told());
        assert!(told()[0].1.contains("refused as it stands") && told()[0].1.contains("owed"), "{:?}", told());
        // Past the window wsp does not close over it: a flag, with the lines.
        tick_at(&store, &idle, t0 + TELL_AFTER + SECOND_WINDOW);
        assert!(running(&store), "a refused exit is never closed over");
        let raised = crate::message::raised(&store);
        assert_eq!(raised.len(), 1, "the person is flagged");
        assert!(raised[0].text.contains("refused as it stands") && raised[0].text.contains("owed"), "{}", raised[0].text);
    }

    #[test]
    fn wsps_close_of_a_leg_with_a_valid_exit_closes_it_with_the_exits_summary_as_caption() {
        let env = util::isolated("closing-settle-leg-valid");
        let (store, _tree) = staged_two(&env);
        write_baton(&store, LEG_EXIT);
        let fake = Fake(RefCell::new(Some(State::Gone)), false);
        tick_at(&store, &fake, util::epoch_secs());
        let t = store.find_task("m-1").unwrap();
        let ls = legs::legs(&t);
        assert_eq!(ls[0].state, LegState::Done, "closed by wsp");
        assert_eq!(ls[0].caption.as_deref(), Some("the store half is built"), "the exit's summary, not its first line");
        assert!(t.section("Log").unwrap().contains("closed by wsp: session ended"), "{}", t.section("Log").unwrap());
        assert!(crate::message::raised(&store).is_empty());
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
        // One line since `wsp-617`: the exit by heredoc, a dry run, the submit alone.
        let w = how_to_close("m-1", false);
        assert!(w.contains("none chained") && w.contains("<<'EOF'") && w.contains("$WSP_EXIT") && w.contains("never a bare `cat`"), "{w}");
        assert!(w.contains("`wsp submit m-1 --dry-run` once") && w.contains("then `wsp submit m-1` alone") && w.contains("leg done"), "{w}");
        assert!(how_to_close("m-1", true).contains("ends the row"));
        for last in [false, true] {
            let w = how_to_close("m-1", last);
            assert!(!w.contains('\n') && w.is_ascii(), "one ASCII line, for an order in argv: {w:?}");
        }
    }

    /// `wsp-617`: every seat's template has its sections in the exit's order,
    /// stays under [`TEMPLATE_MAX`] for a long row id, and ends with the close.
    #[test]
    fn each_seats_exit_template_has_its_sections_in_order_and_fits() {
        use crate::exit::Kind as K;
        let row = "compounding-456";
        for (kind, heads) in [
            (K::Member, &["summary:", "done-when:", "checks:", "asks:", "owed:"][..]),
            (K::Leg, &["summary:", "checks:", "owed:"][..]),
            (K::Verifier, &["verdict: holds | blocks", "summary:", "done-when:", "checked:", "cause:", "owed:"][..]),
            (K::Check, &["verdict: go | hold | followup", "summary:", "checked:", "cause:", "owed:"][..]),
        ] {
            let t = exit_template(kind, row, &[]);
            assert!(t.len() < TEMPLATE_MAX, "{kind:?} is {} bytes: {t}", t.len());
            let at: Vec<usize> = heads.iter().map(|h| t.find(&format!("\n{h}")).unwrap_or_else(|| panic!("{kind:?} has no {h}: {t}"))).collect();
            assert!(at.windows(2).all(|w| w[0] < w[1]), "{kind:?}'s sections are out of order: {t}");
            assert!(t.contains("$WSP_EXIT") && t.trim_end().ends_with(&format!("`wsp submit {row}` alone.")) || kind == K::Leg, "{t}");
            assert!(t.contains(&format!("`wsp submit {row} --dry-run` once")), "{t}");
        }
        assert!(!exit_template(K::Leg, row, &[]).contains("\ndone-when:\n"), "a leg's done-when is its order's");
        assert!(!exit_template(K::Check, row, &[]).contains("\ndone-when:\n"));
    }

    /// The row's done-when bullets come pre-filled, one line each, for the
    /// seat to finish or flip; with none, the line's shape is shown.
    #[test]
    fn a_templates_done_when_is_pre_filled_from_the_rows_items() {
        use crate::exit::Kind as K;
        let items = vec!["the parser reads it".to_string(), "a  long\nitem".to_string()];
        let t = exit_template(K::Member, "m-1", &items);
        assert!(t.contains("done-when:\n- held: the parser reads it — \n- held: a long item — \n\n"), "{t}");
        assert!(!t.contains("<item, as the packet says it>"), "{t}");
        let v = exit_template(K::Verifier, "m-1", &items);
        assert!(v.contains("- held: the parser reads it — \n"), "{v}");
        let bare = exit_template(K::Member, "m-1", &[]);
        assert!(bare.contains("- held: <item, as the packet says it> — <a test name, file:line>\n- not held: <item> — <why>"), "{bare}");
    }

    // ---- wsp-374: a member, a verifier and a check ----------------------------------

    fn git_tree(env: &util::Isolated, name: &str) -> PathBuf {
        let tree = env.home().join(name);
        std::fs::create_dir_all(&tree).unwrap();
        for args in [vec!["init", "-q"], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "c"]] {
            assert!(wsp_base::calls::Command::new("git").arg("-C").arg(&tree).args(&args).status().unwrap().success());
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
        assert!(told().iter().any(|(_, t)| t.contains("`wsp submit m-2`") && t.contains("$WSP_EXIT")), "the exact command and the file: {:?}", told());
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
        wsp_base::calls::Command::new("git").arg("-C").arg(&tree2).args(["add", "f"]).status().unwrap();
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
        assert!(told()[0].1.contains("`wsp submit v-1`") && told()[0].1.contains("$WSP_EXIT"), "{:?}", told());
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

    // -- the exit at a seat's stop, and a seat at a prompt (`wsp-616`)

    /// A seat reading `state`, whose last turn stopped at the epoch `stop`
    /// wanting nobody, with `screen` on it.
    struct AtStop(State, Option<i64>, Option<&'static str>);
    impl Seats for AtStop {
        fn state(&self, _seat: &str) -> Option<State> {
            Some(self.0)
        }
        fn turn_end(&self, _seat: &str) -> Option<crate::waiting::TurnEnd> {
            self.1.map(|at| crate::waiting::TurnEnd { at, words: crate::waiting::last_words("⏺ Read the tree."), denied: false })
        }
        fn screen(&self, _seat: &str) -> Option<String> {
            self.2.map(str::to_string)
        }
    }

    #[test]
    fn a_verifier_whose_turn_stops_with_no_exit_is_told_at_once_once_per_stop_and_flagged_a_window_after_the_first_tell() {
        let env = util::isolated("closing-v-stop");
        let store = staged_verifier(&env, 3600);
        clear_told();
        let t0 = util::epoch_secs();
        tick_at(&store, &AtStop(State::Idle, Some(t0 - 5), None), t0);
        assert_eq!(told().len(), 1, "told on the tick that reads the stop, not {TELL_AFTER}s later: {:?}", told());
        assert!(told()[0].1.contains("no exit at") && told()[0].1.contains("$WSP_EXIT") && told()[0].1.contains("`wsp submit v-1`"), "{:?}", told());
        tick_at(&store, &AtStop(State::Idle, Some(t0 - 5), None), t0 + 30);
        assert_eq!(told().len(), 1, "one stop is told once");
        tick_at(&store, &AtStop(State::Idle, Some(t0 + 40), None), t0 + 60);
        assert_eq!(told().len(), 2, "a new stop is told again: {:?}", told());
        assert!(raised_texts(&store).is_empty(), "nothing flagged inside the window");
        tick_at(&store, &AtStop(State::Idle, Some(t0 + 40), None), t0 + SECOND_WINDOW);
        assert_eq!(raised_texts(&store).len(), 1, "flagged a window after the first tell, as before: {:?}", raised_texts(&store));
        assert_eq!(pass_state(&store), PassState::Running, "no verdict is guessed");
    }

    /// compounding-449, as a test: a verifier stopped on an AskUserQuestion is
    /// raised within one tick, through its route, with the prompt's text — and
    /// never told or closed while it stands there.
    #[test]
    fn a_verifier_at_an_ask_user_question_prompt_is_raised_through_its_route_within_one_tick() {
        let env = util::isolated("closing-v-prompt");
        let store = staged_verifier(&env, 3600);
        clear_told();
        let screen = "⏺ The tree is read.\n\n╭──────────────────────────────╮\n│ Which check gates the barrier? │\n│ ❯ 1. cargo test                │\n│   2. npm run check             │\n╰──────────────────────────────╯\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";
        let t0 = util::epoch_secs();
        let at = AtStop(State::Blocked, None, Some(screen));
        tick_at(&store, &at, t0);
        let raised = raised_texts(&store);
        assert_eq!(raised.len(), 1, "raised on the first tick: {raised:?}");
        assert!(raised[0].contains("the v-1 verifier's pass (cpd-3) is stopped on a prompt"), "{}", raised[0]);
        assert!(raised[0].contains("Which check gates the barrier?") && raised[0].contains("1. cargo test"), "with the prompt's text: {}", raised[0]);
        assert!(!raised[0].contains("Enter to select"), "the footer is left out: {}", raised[0]);
        let m = crate::message::raised(&store).remove(0);
        assert_eq!(crate::waiting::route(&store, &m), crate::waiting::On::Person, "no governor answers for v-1, so its route is the person");
        assert!(m.for_person, "and it is the person's");
        tick_at(&store, &at, t0 + TELL_AFTER + SECOND_WINDOW);
        assert_eq!(raised_texts(&store).len(), 1, "once per spell at the prompt");
        assert!(told().is_empty(), "a sentence typed at a dialog selects an answer nobody chose");
        assert_eq!(pass_state(&store), PassState::Running);
    }

    #[test]
    fn a_verifier_seat_idle_past_the_window_with_a_valid_exit_is_submitted_by_wsp() {
        let env = util::isolated("closing-v-exit");
        let store = staged_verifier(&env, 3600);
        let exit = "verdict: holds\nsummary: the wiring is tested now\n\nRead in the tree; the test drives it.\n\nchecked:\n- unit (2m): cargo test wiring — pass\n";
        assert_eq!(crate::cmd_submit::dry_run(&store, Kind::Verifier, "v-1", exit), Ok(()), "the fixture validates");
        let wrote = put(&store, "v-1", Kind::Verifier, exit);
        clear_told();
        let fake = Fake(RefCell::new(Some(State::Idle)), false);
        let t0 = wrote + 10;
        tick_at(&store, &fake, t0);
        tick_at(&store, &fake, t0 + TELL_AFTER);
        assert!(told().len() == 1 && told()[0].1.contains("validates, and it is not submitted"), "{:?}", told());
        tick_at(&store, &fake, t0 + TELL_AFTER + SECOND_WINDOW);
        assert_eq!(pass_state(&store), PassState::Holds, "wsp submitted the exit for the seat");
        let log = store.find_task("v-1").unwrap().section("Log").unwrap();
        assert!(log.contains("submit · closed by wsp: seat idle"), "{log}");
        assert!(raised_texts(&store).is_empty(), "nothing to flag");
    }

    #[test]
    fn a_member_seat_idle_past_the_window_with_a_valid_exit_is_submitted_and_a_refused_one_is_flagged_with_its_lines() {
        let exit = "summary: did the thing\n\nBuilt it and committed it.\n\ndone-when:\n- held: the work — the commit\n\nowed: nothing\n";
        {
            let env = util::isolated("closing-m-exit");
            let (store, _tree) = staged_member(&env, 3600);
            assert_eq!(crate::cmd_submit::dry_run(&store, Kind::Member, "m-2", exit), Ok(()), "the fixture validates");
            put(&store, "m-2", Kind::Member, exit);
            clear_told();
            member_tick(&store, Some(State::Idle));
            assert!(told().iter().any(|(_, t)| t.contains("validates, and it is not submitted")), "{:?}", told());
            age_notice(&store, "m-2");
            member_tick(&store, Some(State::Idle));
            assert_eq!(member_status(&store), Status::Review, "wsp submitted the exit for the seat");
            assert!(raised_texts(&store).is_empty(), "nothing to flag: {:?}", raised_texts(&store));
        }
        {
            let env = util::isolated("closing-m-exit-refused");
            let (store, _tree) = staged_member(&env, 3600);
            let bad = "summary: did the thing\n\nBuilt it and committed it.\n";
            let lines = crate::cmd_submit::dry_run(&store, Kind::Member, "m-2", bad).unwrap_err();
            put(&store, "m-2", Kind::Member, bad);
            clear_told();
            member_tick(&store, Some(State::Idle));
            age_notice(&store, "m-2");
            member_tick(&store, Some(State::Idle));
            assert_ne!(member_status(&store), Status::Review, "a refused exit is not submitted, and no review is guessed from it");
            let raised = raised_texts(&store);
            assert_eq!(raised.len(), 1, "the person is flagged: {raised:?}");
            assert!(raised[0].contains("refused as it stands") && lines.iter().all(|l| raised[0].contains(l.as_str())), "with its refusal lines {lines:?}: {}", raised[0]);
        }
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

    fn submit_check(env: &util::Isolated, store: &Store, text: &str) -> i32 {
        let file = env.path("exit.md");
        std::fs::write(&file, text).unwrap();
        let from = file.display().to_string();
        let args = crate::Args::synth("submit", &["run"], &[("from", &from)]);
        crate::cmd_submit::submit_from(store, &args, Some("check"), Some("cpd-4".into()))
    }

    #[test]
    fn a_checks_exit_holds_only_with_a_cause_and_a_followup_stays_a_flag_to_the_person() {
        let env = util::isolated("closing-submit-check");
        let store = staged_check(&env);
        let hold = "verdict: hold\nsummary: the barrier's suite is red\n\nTwo tests fail after the group landed.\n\nchecked:\n- suite (20m): cargo nextest run --workspace — fail\n";
        assert_eq!(submit_check(&env, &store, hold), 1, "a hold with no cause is refused");
        assert!(!held(&store));
        let followup = "verdict: followup\nsummary: two rows to file\n\nThe docs and the help drifted.\n";
        assert_eq!(submit_check(&env, &store, followup), 0);
        assert!(!held(&store), "a follow-up moves nothing");
        assert!(raised_texts(&store).iter().any(|t| t.contains("asks for a follow-up")), "{:?}", raised_texts(&store));
        let caused = format!("{hold}\ncause: check — the suite fails at two tests\n");
        assert_eq!(submit_check(&env, &store, &caused), 0);
        assert!(held(&store), "the hold is `worklist hold`");
        // A check seat's `worklist hold` in prose runs the same rule.
        assert!(crate::cmd_submit::check_words_as(Some("check"), "hold", "it is red").is_err());
        assert!(crate::cmd_submit::check_words_as(Some("check"), "hold", "it is red\ncause: check — two tests fail").is_ok());
        assert!(crate::cmd_submit::check_words_as(None, "hold", "it is red").is_ok(), "a person's hold is unchanged");
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
        assert!(told()[0].1.contains("`wsp submit run`") && told()[0].1.contains("$WSP_EXIT"), "{:?}", told());
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
