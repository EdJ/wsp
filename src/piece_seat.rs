//! A piece is a seat: `docs/pieces.md`, *A piece is a seat* and *Batches and
//! trees*, wsp-504. The store side of `wsp spawn <row> --agent --piece <n>` and of
//! the piece's ending; the verb words (`wsp piece done`) are `cmd_piece`'s, so
//! this engine module keeps no edge up to `main`.
//!
//! **A piece claims nothing**, as a leg claims nothing (`leg_seat`): a row has one
//! claim and `claim` refuses a second live holder, so the pieces of one batch
//! could not each hold it. A piece is recorded on its own `## Pieces` entry, its
//! pane is *bound* to the row (`"piece": n` beside `"leg"`, so a command from its
//! pane knows which piece it is), and the log gets two lines a piece:
//!
//! ```text
//! piece 2 started by pane cpd-36 · spawned at sonnet/medium
//! piece 2 ended after 12m · ran sonnet-5-5/medium · 23 turns · … · peak …
//! ```
//!
//! The clauses are the claim and release lines' own (`cmd_agent::spawned_at`,
//! `ran_in_seat`), handed in by the caller that is above this module (the spawn
//! and the verbs), and `cmd_attempts` pairs the two by the piece's number.
//!
//! # What the doc left open (wsp-504)
//!
//! - **The lowest open batch** is the lowest batch with a piece that is `pending`,
//!   `running` or `blocked`. A hand spawn of a piece in a later batch is refused,
//!   naming what is open. A blocked piece holds its batch open (*A piece is a
//!   seat*: the batch cannot close until it is answered).
//! - **The cap is the run's.** A hand spawn writes `running` whatever the row's
//!   `cap:` says, as a hand spawn is not under the machine's cap either.
//! - **A `todo` row becomes `doing`** when its first piece opens, as a leg's does.
//!   After that no piece moves the row's status.
//! - **A wider batch** counts the pieces that are not `dropped` or `promoted`; a
//!   piece that has a tree keeps it when it is opened again.
//! - **The tree is named by the piece id** (`wsp-501-2`), cut from the row's
//!   tip by [`crate::leg_tree::open_named`].
//! - **`review` from a piece's pane** gives the piece its caption from the first
//!   words of the account.

use crate::model::{Policy, Status, Task};
use crate::pieces::{self, Piece, Record, State};
use crate::store::Store;
use crate::util;

/// The most characters of the row's packet prose a piece's brief carries.
const PACKET_TEXT: usize = 4_000;
/// The batch before's notes, read whole up to this many characters in all.
pub(crate) const NOTES_CAP: usize = 6_000;
/// A caption, in characters.
const CAPTION_CAP: usize = 160;

// ---- which piece may open ----------------------------------------------------------

/// The lowest batch with a piece still to settle: `pending`, `running` or `blocked`.
pub(crate) fn lowest_open(rec: &Record) -> Option<usize> {
    rec.pieces
        .iter()
        .filter(|p| matches!(p.state, State::Pending | State::Running | State::Blocked))
        .map(|p| p.batch)
        .min()
}

/// What is open in `batch`, in words: `piece 1 running, piece 2 pending`.
fn open_in(rec: &Record, batch: usize) -> String {
    rec.pieces
        .iter()
        .filter(|p| p.batch == batch && matches!(p.state, State::Pending | State::Running | State::Blocked))
        .map(|p| format!("piece {} {}", p.n, p.state.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `batch` has more than one piece to run: each then gets a tree of its own.
pub(crate) fn wide(rec: &Record, batch: usize) -> bool {
    rec.pieces.iter().filter(|p| p.batch == batch && !matches!(p.state, State::Dropped | State::Promoted)).count() > 1
}

/// Why piece `n` may not open now, or the piece itself.
fn openable(rec: &Record, n: usize) -> Result<&Piece, String> {
    let Some(p) = rec.pieces.iter().find(|p| p.n == n) else {
        return Err(format!("no piece {n} — `wsp piece show` lists them"));
    };
    match p.state {
        State::Running if p.pane.is_none() => {}
        State::Pending => {}
        other => return Err(format!("piece {n} is {}, and only a pending piece can be opened", other.as_str())),
    }
    match lowest_open(rec) {
        Some(low) if low != p.batch => Err(format!(
            "piece {n} is in batch {}, and batch {low} is still open: {}",
            p.batch,
            open_in(rec, low)
        )),
        _ => Ok(p),
    }
}

// ---- opening ---------------------------------------------------------------------

/// Open piece `n` of `id`: `running`, stamped, under the store lock, before any seat
/// exists. Idempotent for a piece already open with no seat.
pub(crate) fn open(store: &Store, id: &str, n: usize) -> Result<Piece, String> {
    store.locked(|| {
        let mut t = store.find_task(id).ok_or_else(|| format!("no task {id}"))?;
        match t.status() {
            Status::Todo | Status::Doing => {}
            other => return Err(format!("{id} is {} — a piece opens on a row at doing", other.as_str())),
        }
        let mut rec = pieces::record(&t);
        if rec.pieces.is_empty() {
            return Err(format!("{id} has no pieces — `wsp piece add` writes them"));
        }
        openable(&rec, n)?;
        if let Some(p) = rec.pieces.iter_mut().find(|p| p.n == n) {
            if p.state == State::Pending {
                p.state = State::Running;
                p.started = Some(util::now_iso());
            }
        }
        if t.status() == Status::Todo {
            t.set_status(Status::Doing);
            t.log("→ doing");
        }
        pieces::write(&mut t, &rec);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        rec.pieces.into_iter().find(|p| p.n == n).ok_or_else(|| format!("no piece {n}"))
    })
}

/// A spawn that never got a seat gives the piece back.
pub(crate) fn unopen(store: &Store, id: &str, n: usize) {
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return };
        let mut rec = pieces::record(&t);
        let Some(p) = rec.pieces.iter_mut().find(|p| p.n == n) else { return };
        if p.state == State::Running && p.pane.is_none() {
            p.state = State::Pending;
            p.started = None;
            // Made for this opening; the next one makes its own.
            p.tree = None;
            pieces::write(&mut t, &rec);
            let _ = store.save_task(&t);
        }
    });
}

/// A start that failed, or a seat that never came up: the piece goes back to
/// `pending` **with its seat named or not**, which [`unopen`] will not do, and the
/// seat's binding goes (wsp-506; `leg_seat::reset`'s rule, `compound-956`). The
/// seat it had, if any. Nothing for a piece that is neither `running` nor `blocked`
/// (a blocked one is put back by `wsp piece reset`, wsp-509).
pub(crate) fn reset(store: &Store, id: &str, n: usize) -> Option<Option<String>> {
    let seat = store.locked(|| {
        let mut t = store.find_task(id)?;
        let mut rec = pieces::record(&t);
        let p = rec.pieces.iter_mut().find(|p| p.n == n && matches!(p.state, State::Running | State::Blocked))?;
        let seat = p.pane.take();
        p.state = State::Pending;
        p.started = None;
        p.tree = None;
        p.ending = crate::verification::Ending::Standing;
        pieces::write(&mut t, &rec);
        store.save_task(&t).ok()?;
        if let Some(s) = &seat {
            store.clear_binding(s);
        }
        Some(seat)
    });
    if seat.is_some() {
        store.git_commit(&format!("wsp: {id} piece {n} reset"));
    }
    seat
}

/// Where a piece's seat stands: the tree to open in when it is not the row's.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Prepared {
    /// The tree, as `~/…`.
    pub cwd: Option<String>,
    /// The tree's name, for the entry's `tree` clause.
    pub tree: Option<String>,
}

/// What a piece's spawn does before its seat opens: a piece of a wider batch gets a
/// tree of its own, cut from the row's tip. A batch of one stands in the row's tree.
///
/// `row_cwd` is the row's tree, which the spawn has just made or found; `root` the
/// project root the trees are cut in. A piece that cannot open is left to [`open`]
/// to refuse. An `Err` is a tree that could not be made, and the piece must not
/// open in the row's tree instead: that is where a sibling's work is merged.
pub(crate) fn prepare(store: &Store, root: &str, row_cwd: &str, task: &str, n: usize) -> Result<Prepared, String> {
    let Some(t) = store.find_task(task) else { return Ok(Prepared::default()) };
    let rec = pieces::record(&t);
    let Some(p) = rec.pieces.iter().find(|p| p.n == n) else { return Ok(Prepared::default()) };
    if let (State::Running, Some(_), Some(tree)) = (p.state, &p.pane, &p.tree) {
        return Ok(Prepared { cwd: Some(crate::leg_tree::tree_path(row_cwd, tree)), tree: None });
    }
    if openable(&rec, n).is_err() || !wide(&rec, p.batch) {
        return Ok(Prepared::default());
    }
    crate::leg_tree::require_row_tree(root, row_cwd, task)?;
    let o = crate::leg_tree::open_named(root, task, &p.id(task))?;
    Ok(Prepared { cwd: Some(o.path), tree: Some(o.name) })
}

/// Put what [`prepare`] did onto the piece's entry, under the store lock.
pub(crate) fn record_tree(store: &Store, task: &str, n: usize, p: &Prepared) {
    let Some(tree) = &p.tree else { return };
    store.locked(|| {
        let Some(mut t) = store.find_task(task) else { return };
        let mut rec = pieces::record(&t);
        let Some(piece) = rec.pieces.iter_mut().find(|x| x.n == n) else { return };
        piece.tree = Some(tree.clone());
        pieces::write(&mut t, &rec);
        let _ = store.save_task(&t);
    });
}

/// What a seat is bound with, besides the task: the room and tree it stands in
/// and the kind that runs in it.
pub(crate) struct Standing<'a> {
    pub room: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub kind: &'a str,
    /// The start line's tier clause (`cmd_agent::spawned_at`): ` · spawned at sonnet/medium`.
    pub spawned: &'a str,
}

/// What a seat's session served since a time, as the end line's clauses
/// (`cmd_agent::ran_in_seat`): given the seat and the start.
pub(crate) type Ran<'a> = &'a dyn Fn(&str, &str) -> String;

/// The seat onto the entry, the pane bound to the row, and the start line on the
/// log — one section under the lock, so a reader never sees a piece with a seat and
/// no binding. Whether it wrote.
pub(crate) fn attach(store: &Store, id: &str, n: usize, seat: &str, stand: &Standing) -> bool {
    let wrote = store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        let mut rec = pieces::record(&t);
        let Some(p) = rec.pieces.iter_mut().find(|p| p.n == n) else { return false };
        if p.state != State::Running || p.pane.is_some() {
            return false;
        }
        p.pane = Some(seat.to_string());
        let why = match (crate::legs::is_default(&p.policy), p.why.as_deref()) {
            (false, Some(w)) => format!(" · why {}", util::fold(w)),
            _ => String::new(),
        };
        t.log(&format!(
            "piece {n} started by pane {seat}{}{why}",
            stand.spawned
        ));
        let piece_id = p.id(id);
        pieces::write(&mut t, &rec);
        if store.save_task(&t).is_err() {
            return false;
        }
        // `leg` beside `piece`: everything that reads "a leg's seat is not the
        // row's member" (`ending`, `release`) reads a piece's the same way.
        let mut binding = serde_json::json!({
            "task_id": t.id,
            "pane_id": seat,
            "leg": piece_id,
            "piece": n,
            "agent_kind": stand.kind,
            "started_at": util::now_iso(),
        });
        if let Some(r) = stand.room {
            binding["workspace_id"] = serde_json::json!(r);
        }
        if let Some(c) = stand.cwd {
            binding["cwd"] = serde_json::json!(c);
        }
        store.set_binding(seat, binding);
        true
    });
    if wrote {
        // A seat is a start that took: its misses in `starts.json` are forgotten.
        crate::starts::forgive(store, &format!("{id}-{n}"));
        store.git_commit(&format!("wsp: {id} piece {n} started in {seat}"));
    }
    wrote
}

/// The tier a piece spawns at: its entry's kind, model and effort.
pub(crate) fn tier_of(p: &Piece) -> (String, Option<String>, Option<String>) {
    let Policy { kind, model, effort } = p.policy.clone();
    (kind, model, effort)
}

/// The piece this seat is running, if any.
pub(crate) fn actor_of(rec: &Record, pane: Option<&str>) -> Option<usize> {
    let pane = pane?;
    rec.pieces.iter().find(|p| p.state == State::Running && p.pane.as_deref() == Some(pane)).map(|p| p.n)
}

/// The one command that ends a piece, in words.
pub(crate) fn ending_clause(task: &str) -> String {
    format!(
        "End with `wsp piece done --note FILE --caption \"...\"` (or `wsp review {task} \"...\"`, which is the same with the \
         account as the note): it runs your `check:` lines in this tree and is refused while one is red. A piece never moves \
         the row's status; if you cannot go on, `wsp block {task} \"...\"` blocks your piece and not the row."
    )
}

/// The piece's own order text, for a work order that is one line.
pub(crate) fn order_line(task: &str, n: usize, fetch: bool) -> String {
    let fetch = match fetch {
        true => "Please run `wsp brief --session` first — your context is empty. ",
        false => "",
    };
    format!(
        "{fetch}You are piece {n} of {task}: one seat in a row run as pieces. Your brief is already above, \
         with the row's packet, the pieces table, your own entry and the notes of the batch before; work in this tree, \
         commit. {} `wsp ask {task} \"...\"` reaches whoever coordinates this.",
        ending_clause(task)
    )
}

// ---- ending ---------------------------------------------------------------------

/// The end line for a piece that ended now.
fn ended_line(ran: Ran, p: &Piece, pane: &str, extra: &str) -> String {
    let started = p.started.clone().unwrap_or_default();
    let secs = if started.is_empty() { 0 } else { (util::epoch_secs() - util::epoch_of(&started)).max(0) };
    let after = if secs > 0 { format!(" after {}", util::duration_human(secs)) } else { String::new() };
    let ran = if started.is_empty() { String::new() } else { ran(pane, &started) };
    format!("piece {} ended{after}{extra}{ran}", p.n)
}

/// What a finished piece leaves the caller to print.
#[derive(Debug)]
pub(crate) struct Finished {
    pub task: Task,
    pub piece: Piece,
    pub sha: String,
    /// How many `check:` lines ran.
    pub checks: usize,
}

/// A step's way of failing, in a line.
fn red_line(r: &crate::check::Ran) -> String {
    let mut out = format!("check `{}` is red ({})", r.step.name, crate::check::why_failed(r));
    let tail = r.tail.trim();
    if !tail.is_empty() {
        out.push_str(":\n  ");
        out.push_str(&tail.lines().collect::<Vec<_>>().join("\n  "));
    }
    out
}

/// End piece `n` of `row`, from the seat `pane` standing in `dir`.
///
/// Refuses a tree with uncommitted tracked changes, a missing caption, a note over
/// its cap, and a `check:` line that is red (naming the step): the piece is not
/// done while its own done-when's checks fail. On green writes `checked: green at
/// <sha>`, the note and caption, marks the piece `done`, writes the end line and
/// pokes the run. Never moves the row's status.
pub(crate) fn finish(
    store: &Store,
    row: &str,
    pane: &str,
    dir: Option<&str>,
    note: &str,
    caption: &str,
    ran: Ran,
) -> Result<Finished, String> {
    let t = store.task_or_why(row)?;
    let rec = pieces::record(&t);
    let Some(n) = actor_of(&rec, Some(pane)) else {
        return Err(format!("{pane} is not the seat of a running piece of {} — only a piece's own seat ends it", t.id));
    };
    let mine = rec.pieces.iter().find(|p| p.n == n).cloned().ok_or_else(|| format!("no piece {n}"))?;
    let caption = util::fold(caption);
    if caption.is_empty() || caption == "true" {
        return Err("a piece's end takes a caption: the line a glance reads, --caption \"…\"".into());
    }
    if caption.chars().count() > CAPTION_CAP {
        return Err(format!("the caption is {} characters and the cap is {CAPTION_CAP}", caption.chars().count()));
    }
    let note = note.trim();
    if note.chars().count() > pieces::NOTE_CAP {
        return Err(format!(
            "the note is {} characters and the cap is {} — pointers, not a plan",
            note.chars().count(),
            pieces::NOTE_CAP
        ));
    }
    if let Some(dir) = dir {
        if let Some(changed) = crate::leg_seat::dirty(dir).filter(|c| !c.is_empty()) {
            let shown: Vec<&str> = changed.iter().take(5).map(String::as_str).collect();
            return Err(format!(
                "{dir} has {} uncommitted change(s) — every piece ends at a commit, so the batch can merge it. \
                 Commit, then end the piece again:\n  {}",
                changed.len(),
                shown.join("\n  ")
            ));
        }
    }
    // The piece's own checks, in its tree, each line the `## Checks` format.
    let mut steps = Vec::new();
    for c in &mine.checks {
        match crate::check::step_of(c, crate::check::Of::Task) {
            Some(s) => steps.push(s),
            None => return Err(format!("piece {n} has a `check:` line that is no command: {c}")),
        }
    }
    let run_dir = dir.map(|d| util::expand(d)).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
    if !steps.is_empty() {
        let scratch = crate::scratch::path(store, &t.id);
        let _ = std::fs::create_dir_all(&scratch);
        let log = scratch.join(format!("piece-{n}-check.log"));
        let runner = crate::check::Runner::chosen(store, crate::check::Whose::Row(&t.id), crate::check::TreeKind::Work, &run_dir, None)?;
        let ran = crate::check::run_all(&run_dir, &steps, &log, None, &runner, |_| {});
        if let Some(red) = ran.iter().find(|r| r.done != crate::check::Done::Passed) {
            return Err(format!("piece {n} is not done: {}", red_line(red)));
        }
    }
    let sha = crate::cmd_verify::git(&run_dir, &["rev-parse", "--short", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default();
    let line = ended_line(ran, &mine, pane, "");
    let at = util::now_iso();
    let checked = format!("green at {}", if sha.is_empty() { "no commit" } else { sha.as_str() });
    let result = store.locked(|| {
        let mut t = store.find_task(&t.id).ok_or_else(|| format!("no task {}", t.id))?;
        let mut rec = pieces::record(&t);
        let p = rec
            .pieces
            .iter_mut()
            .find(|p| p.n == n && p.state == State::Running && p.pane.as_deref() == Some(pane))
            .ok_or_else(|| format!("piece {n} is no longer running in {pane}"))?;
        p.state = State::Done;
        p.checked = Some(checked.clone());
        p.note = note.to_string();
        p.caption = Some(caption.clone());
        let done = p.clone();
        t.log(&line);
        pieces::write(&mut t, &rec);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok::<_, String>((t, done))
    });
    let (t, piece) = result?;
    store.log_event("piece-done", serde_json::json!({ "id": t.id, "piece": n, "at": at }));
    store.git_commit(&format!("wsp: {} piece {n} done", t.id));
    crate::cycle::poke_task(store, &t.id, "piece-done");
    Ok(Finished { task: t, piece, sha, checks: steps.len() })
}

/// Whether `pane` is the seat of a running piece of `needle`'s row.
pub(crate) fn is_seat(store: &Store, needle: &str, pane: Option<&str>) -> bool {
    store.find_task(needle).is_some_and(|t| actor_of(&pieces::record(&t), pane).is_some())
}

/// `review` typed in a piece's seat is `piece done` with the account as the note, and
/// the row stays where it is. `Ok(None)` for a pane that is no piece's: the caller goes
/// on to move the row.
pub(crate) fn on_review(store: &Store, needle: &str, account: &str, pane: Option<&str>, ran: Ran) -> Result<Option<Finished>, String> {
    let Some(t) = store.find_task(needle) else { return Ok(None) };
    let rec = pieces::record(&t);
    let Some(n) = actor_of(&rec, pane) else { return Ok(None) };
    let pane = pane.unwrap_or_default();
    let first = account.split(['.', '\n']).next().unwrap_or_default();
    let caption = util::truncate(&util::fold(first), 80);
    let caption = if caption.is_empty() { format!("piece {n} done") } else { caption };
    let note = if account.trim().chars().count() > pieces::NOTE_CAP {
        util::truncate(account.trim(), pieces::NOTE_CAP)
    } else {
        account.trim().to_string()
    };
    let dir = crate::leg_seat::standing_dir(store, pane);
    finish(store, &t.id, pane, dir.as_deref(), &note, &caption, ran).map(Some)
}

/// `block` typed in a piece's seat blocks the piece, not the row. Returns the piece's
/// id for the caller to tell the row's governor with, or `None` for a pane that is no
/// piece's. The batch cannot close until the piece is answered (`wsp tell`, then
/// `wsp piece reset`), dropped or promoted.
pub(crate) fn on_block(store: &Store, needle: &str, reason: &str, pane: Option<&str>) -> Result<Option<String>, String> {
    let Some(t) = store.find_task(needle) else { return Ok(None) };
    let rec = pieces::record(&t);
    let Some(n) = actor_of(&rec, pane) else { return Ok(None) };
    let id = store.locked(|| {
        let mut t = store.find_task(&t.id).ok_or_else(|| format!("no task {}", t.id))?;
        let mut rec = pieces::record(&t);
        let p = rec.pieces.iter_mut().find(|p| p.n == n).ok_or_else(|| format!("no piece {n}"))?;
        p.state = State::Blocked;
        let id = p.id(&t.id);
        t.log(&format!("piece {n} blocked: {}", util::fold(reason)));
        pieces::write(&mut t, &rec);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok::<_, String>(id)
    })?;
    store.log_event("piece-blocked", serde_json::json!({ "id": t.id, "piece": n }));
    store.git_commit(&format!("wsp: {} piece {n} blocked", t.id));
    crate::cycle::poke_task(store, &t.id, "piece-blocked");
    Ok(Some(id))
}

// ---- the brief ------------------------------------------------------------------

/// The batch before `batch`: the highest lower batch any piece is in.
fn batch_before(rec: &Record, batch: usize) -> Option<usize> {
    rec.pieces.iter().map(|p| p.batch).filter(|b| *b < batch).max()
}

/// The notes of the batch before, whole until [`NOTES_CAP`] characters are spent and as
/// captions after.
fn before_notes(t: &Task, rec: &Record, batch: usize) -> Option<String> {
    let before = batch_before(rec, batch)?;
    let mut spent = 0usize;
    let mut lines = Vec::new();
    for p in rec.pieces.iter().filter(|p| p.batch == before && p.state == State::Done) {
        let caption = p.caption.as_deref().unwrap_or("no caption");
        let whole = p.note.trim();
        let fits = !whole.is_empty() && spent + whole.chars().count() <= NOTES_CAP;
        if fits {
            spent += whole.chars().count();
            lines.push(format!("piece {} ({}) — {}:\n{}", p.n, p.title, caption, whole));
        } else {
            lines.push(format!(
                "piece {} ({}) — {} · `wsp piece show {} {}` reads the note whole",
                p.n, p.title, caption, t.id, p.n
            ));
        }
    }
    (!lines.is_empty()).then(|| format!("notes of batch {before}:\n{}", lines.join("\n")))
}

/// The piece block of a brief: which piece this seat is, the row's packet once, the
/// table, its entry whole, and the notes of the batch before. `None` for a seat that
/// is no running piece's.
pub(crate) fn brief_lines(tasks: &[Task], pane: &str) -> Option<Vec<String>> {
    let (t, rec, n) = tasks.iter().find_map(|t| {
        let rec = pieces::record(t);
        let n = actor_of(&rec, Some(pane))?;
        Some((t, rec, n))
    })?;
    let me = rec.pieces.iter().find(|p| p.n == n)?;
    let count = |s: State| rec.pieces.iter().filter(|p| p.state == s).count();
    let mut out = vec![format!(
        "piece {n} on {} · batch {} · {} running, {} done of {}",
        t.id,
        me.batch,
        count(State::Running),
        count(State::Done),
        rec.pieces.len()
    )];
    if let Some(p) = crate::packet::of(t).filter(|p| !p.text.trim().is_empty()) {
        out.push(format!("the row's packet:\n{}", util::truncate(p.text.trim(), PACKET_TEXT)));
    }
    out.push(format!("the pieces:\n{}", pieces::table(&rec)));
    out.push(format!("your piece:\n{}", me.lines()));
    if let Some(tree) = &me.tree {
        out.push(format!(
            "your tree: {tree} (a branch of that name, cut from the row's tip) — your batch has siblings, so you stand apart; \
             the batch's close merges it"
        ));
    }
    if let Some(notes) = before_notes(t, &rec, me.batch) {
        out.push(notes);
    }
    out.push(format!(
        "finish: commit, then `wsp piece done --note FILE --caption \"…\"` — a note of at most {} characters, pointers not a \
         plan (what is done, where, what is left). It runs your `check:` lines here and is refused while one is red. \
         The closing verb is its own Bash call, never chained after another command. `wsp block {} \"…\"` blocks your \
         piece and not the row",
        pieces::NOTE_CAP,
        t.id
    ));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(spec: &[(usize, usize, State)]) -> Record {
        let mut r = Record::default();
        for (n, batch, state) in spec {
            let mut p = Piece::pending(*n, &format!("piece {n}"), *batch);
            p.state = *state;
            r.pieces.push(p);
        }
        r
    }

    #[test]
    fn the_lowest_open_batch_is_the_lowest_with_a_piece_to_settle() {
        let r = rec(&[(1, 1, State::Done), (2, 1, State::Dropped), (3, 2, State::Pending), (4, 3, State::Pending)]);
        assert_eq!(lowest_open(&r), Some(2));
        let r = rec(&[(1, 1, State::Blocked), (2, 2, State::Pending)]);
        assert_eq!(lowest_open(&r), Some(1), "a blocked piece holds its batch open");
        assert_eq!(lowest_open(&rec(&[(1, 1, State::Done)])), None);
    }

    #[test]
    fn a_later_batch_is_refused_naming_what_is_open() {
        let mut r = rec(&[(1, 1, State::Running), (2, 1, State::Pending), (3, 2, State::Pending)]);
        r.pieces[0].pane = Some("cpd-1".into());
        let why = openable(&r, 3).unwrap_err();
        assert!(why.contains("batch 1 is still open") && why.contains("piece 1 running, piece 2 pending"), "{why}");
        assert!(openable(&r, 2).is_ok());
        assert!(openable(&r, 1).unwrap_err().contains("only a pending piece"), "a running piece with a seat is not reopened");
    }

    #[test]
    fn a_batch_of_one_stands_in_the_rows_tree_and_a_wider_one_does_not() {
        let r = rec(&[(1, 1, State::Pending), (2, 1, State::Pending), (3, 2, State::Pending), (4, 3, State::Pending), (5, 3, State::Dropped)]);
        assert!(wide(&r, 1));
        assert!(!wide(&r, 2));
        assert!(!wide(&r, 3), "a dropped piece is not a sibling");
    }

    #[test]
    fn the_brief_names_the_piece_the_table_and_the_batch_before() {
        let mut t = Task::new("a row", "m-1");
        let mut r = Record::default();
        let mut one = Piece::pending(1, "contract types", 1);
        one.state = State::Done;
        one.caption = Some("types read back".into());
        one.note = "Done: 52c768d types".into();
        let mut two = Piece::pending(2, "board", 2);
        two.state = State::Running;
        two.pane = Some("cpd-9".into());
        two.done_when = "board green".into();
        r.pieces = vec![one, two];
        pieces::write(&mut t, &r);
        let lines = brief_lines(&[t], "cpd-9").expect("a running piece's seat has a block").join("\n");
        assert!(lines.contains("piece 2 on m-1 · batch 2 · 1 running, 1 done of 2"), "{lines}");
        assert!(lines.contains("contract types") && lines.contains("types read back"), "{lines}");
        assert!(lines.contains("done-when: board green"), "{lines}");
        assert!(lines.contains("notes of batch 1") && lines.contains("Done: 52c768d types"), "{lines}");
        assert!(lines.contains("wsp piece done --note FILE"), "{lines}");
        assert!(brief_lines(&[Task::new("a row", "m-1")], "cpd-9").is_none());
    }

    // ---- on a scratch repository ------------------------------------------------------

    use std::path::{Path, PathBuf};
    use wsp_base::calls::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn head(dir: &Path) -> String {
        crate::cmd_verify::git(dir, &["rev-parse", "--short", "HEAD"]).unwrap().trim().to_string()
    }

    /// A repository with project `p`, a row `p-001` at `doing` with its tree, and pieces
    /// 1 and 2 in batch 1 and 3 in batch 2. Piece 1 has the checks given.
    fn scratch(name: &str, checks: &[&str]) -> (util::Isolated, Store, PathBuf, PathBuf) {
        let env = util::isolated(&format!("piece-seat-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        std::fs::write(repo.join(".gitignore"), "target/\n.worktrees/\n").unwrap();
        std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "--quiet", "-m", "first"]);
        let store = Store::open();
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let row = repo.join(".worktrees").join("p-001");
        git(&repo, &["worktree", "add", "--quiet", "-b", "p-001", &row.display().to_string(), "master"]);
        let mut t = Task::new("a row", "p-001");
        t.project = Some("p".into());
        t.set_status(Status::Doing);
        let mut r = Record::default();
        let mut one = Piece::pending(1, "first", 1);
        one.checks = checks.iter().map(|c| c.to_string()).collect();
        r.pieces = vec![one, Piece::pending(2, "second", 1), Piece::pending(3, "third", 2)];
        pieces::write(&mut t, &r);
        store.save_task(&t).unwrap();
        (env, store, repo, row)
    }

    fn state_of(store: &Store, n: usize) -> Piece {
        pieces::record(&store.find_task("p-001").unwrap()).pieces.into_iter().find(|p| p.n == n).unwrap()
    }

    fn none(_: &str, _: &str) -> String {
        String::new()
    }

    /// Open piece `n`, make its tree when its batch is wide, and seat `pane` in it.
    fn seat(store: &Store, repo: &Path, row: &Path, n: usize, pane: &str) -> PathBuf {
        let prepared = prepare(store, &repo.display().to_string(), &row.display().to_string(), "p-001", n).unwrap();
        open(store, "p-001", n).unwrap();
        record_tree(store, "p-001", n, &prepared);
        let cwd = prepared.cwd.clone().unwrap_or_else(|| row.display().to_string());
        let stand = Standing { room: None, cwd: Some(&cwd), kind: "claude", spawned: " · spawned at sonnet/medium" };
        assert!(attach(store, "p-001", n, pane, &stand), "piece {n} was not open to seat");
        util::expand(&cwd)
    }

    #[test]
    fn each_piece_of_a_wider_batch_has_a_tree_off_the_rows_tip_and_a_batch_of_one_stands_in_the_row() {
        let (_env, store, repo, row) = scratch("trees", &[]);
        std::fs::write(row.join("plan.txt"), "the plan\n").unwrap();
        git(&row, &["add", "plan.txt"]);
        git(&row, &["commit", "--quiet", "-m", "plan"]);
        let tip = head(&row);
        let a = seat(&store, &repo, &row, 1, "cpd-1");
        let b = seat(&store, &repo, &row, 2, "cpd-2");
        assert!(a.ends_with(".worktrees/p-001-1") && b.ends_with(".worktrees/p-001-2"), "{a:?} {b:?}");
        for d in [&a, &b] {
            assert_eq!(head(d), tip, "{d:?} did not start at the row's tip");
            assert!(d.join("plan.txt").exists());
        }
        assert_eq!(state_of(&store, 1).tree.as_deref(), Some("p-001-1"));
        assert_eq!(state_of(&store, 2).tree.as_deref(), Some("p-001-2"));
        assert_eq!(head(&row), tip, "the row's own tree is untouched");
        // No claim, and the pane is bound to the row with the piece beside `leg`.
        assert!(store.claims().is_empty());
        let b1 = store.bindings()["cpd-1"].clone();
        assert_eq!((b1["task_id"].as_str(), b1["piece"].as_u64(), b1["leg"].as_str()), (Some("p-001"), Some(1), Some("p-001-1")));
        let log = store.find_task("p-001").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("piece 1 started by pane cpd-1 · spawned at sonnet/medium"), "{log}");
        assert!(log.contains("piece 2 started by pane cpd-2"), "{log}");
        assert!(!log.contains("claimed by pane"), "{log}");
        // Batch 2 has one piece: it stands in the row's tree, once batch 1 is settled.
        assert!(open(&store, "p-001", 3).unwrap_err().contains("batch 1 is still open"));
        let p3 = prepare(&store, &repo.display().to_string(), &row.display().to_string(), "p-001", 3).unwrap();
        assert_eq!(p3, Prepared::default());
    }

    #[test]
    fn a_later_batch_is_refused_while_the_one_before_is_open_and_the_row_moves_from_todo() {
        let (_env, store, _repo, _row) = scratch("order", &[]);
        let mut t = store.find_task("p-001").unwrap();
        t.set_status(Status::Todo);
        store.save_task(&t).unwrap();
        let why = open(&store, "p-001", 3).unwrap_err();
        assert!(why.contains("batch 1 is still open") && why.contains("piece 1 pending, piece 2 pending"), "{why}");
        assert_eq!(state_of(&store, 3).state, State::Pending, "nothing was written");
        assert_eq!(store.find_task("p-001").unwrap().status(), Status::Todo);
        open(&store, "p-001", 1).unwrap();
        assert_eq!(store.find_task("p-001").unwrap().status(), Status::Doing);
        assert!(state_of(&store, 1).started.is_some());
        unopen(&store, "p-001", 1);
        assert_eq!(state_of(&store, 1).state, State::Pending, "a spawn with no seat gives the piece back");
        assert!(state_of(&store, 1).started.is_none());
    }

    #[test]
    fn piece_done_is_refused_on_a_red_check_naming_the_step_and_passes_once_green() {
        let (_env, store, repo, row) = scratch("checks", &["gate (1m): test -f ok.txt", "after (1m): true"]);
        let a = seat(&store, &repo, &row, 1, "cpd-1");
        let end = |note: &str, caption: &str| finish(&store, "p-001", "cpd-1", Some(&a.display().to_string()), note, caption, &none);
        assert!(end("n", "").unwrap_err().contains("caption"));
        // An uncommitted tracked change is refused.
        std::fs::write(a.join("tracked.txt"), "changed\n").unwrap();
        assert!(end("n", "c").unwrap_err().contains("uncommitted"));
        git(&a, &["commit", "--quiet", "-am", "edit"]);
        // Red: the step is named, the piece is still running, and nothing was written.
        let why = end("n", "c").unwrap_err();
        assert!(why.contains("piece 1 is not done") && why.contains("`gate`") && why.contains("red"), "{why}");
        let p = state_of(&store, 1);
        assert_eq!((p.state, p.checked.as_deref()), (State::Running, None));
        // Green.
        std::fs::write(a.join("ok.txt"), "yes\n").unwrap();
        git(&a, &["add", "ok.txt"]);
        git(&a, &["commit", "--quiet", "-m", "ok"]);
        let done = end("Done: ok.txt. Left: nothing.", "gate passes").unwrap();
        assert_eq!((done.checks, done.sha.clone()), (2, head(&a)));
        let p = state_of(&store, 1);
        assert_eq!(p.state, State::Done);
        assert_eq!(p.checked, Some(format!("green at {}", head(&a))));
        assert_eq!((p.note.as_str(), p.caption.as_deref()), ("Done: ok.txt. Left: nothing.", Some("gate passes")));
        let t = store.find_task("p-001").unwrap();
        assert_eq!(t.status(), Status::Doing, "a piece never moves the row");
        assert!(t.section("Log").unwrap().contains("piece 1 ended"), "{}", t.section("Log").unwrap());
        assert!(end("n", "c").is_err(), "a done piece is not ended twice");
        assert_eq!(state_of(&store, 2).state, State::Pending, "a sibling is untouched");
    }

    #[test]
    fn review_from_a_pieces_pane_ends_the_piece_and_leaves_the_row_and_its_sibling() {
        let (_env, store, repo, row) = scratch("review", &[]);
        seat(&store, &repo, &row, 1, "cpd-1");
        seat(&store, &repo, &row, 2, "cpd-2");
        assert!(is_seat(&store, "p-001", Some("cpd-2")) && !is_seat(&store, "p-001", Some("cpd-9")));
        assert!(on_review(&store, "p-001", "the pane", Some("cpd-9"), &none).unwrap().is_none());
        let done = on_review(&store, "p-001", "Wrote the board. Left: the rail.", Some("cpd-2"), &none).unwrap().expect("a piece's seat");
        assert_eq!(done.piece.n, 2);
        let p = state_of(&store, 2);
        assert_eq!((p.state, p.caption.as_deref()), (State::Done, Some("Wrote the board")));
        assert_eq!(p.note, "Wrote the board. Left: the rail.");
        assert_eq!(state_of(&store, 1).state, State::Running);
        assert_eq!(store.find_task("p-001").unwrap().status(), Status::Doing);
    }

    #[test]
    fn block_from_a_pieces_pane_blocks_the_piece_and_not_the_row() {
        let (_env, store, repo, row) = scratch("block", &[]);
        seat(&store, &repo, &row, 1, "cpd-1");
        seat(&store, &repo, &row, 2, "cpd-2");
        assert_eq!(on_block(&store, "p-001", "which layout?", Some("cpd-9")).unwrap(), None);
        assert_eq!(on_block(&store, "p-001", "which layout?", Some("cpd-1")).unwrap().as_deref(), Some("p-001-1"));
        assert_eq!(state_of(&store, 1).state, State::Blocked);
        assert_eq!(state_of(&store, 2).state, State::Running, "its sibling carries on");
        let t = store.find_task("p-001").unwrap();
        assert_eq!(t.status(), Status::Doing, "the row is not blocked");
        assert!(t.section("Log").unwrap().contains("piece 1 blocked: which layout?"));
        // A blocked piece holds its batch open.
        assert_eq!(lowest_open(&pieces::record(&t)), Some(1));
    }

    #[test]
    fn the_notes_of_the_batch_before_are_captions_past_the_cap() {
        let mut t = Task::new("a row", "m-1");
        let mut r = Record::default();
        for n in 1..=5usize {
            let mut p = Piece::pending(n, &format!("p{n}"), 1);
            p.state = State::Done;
            p.caption = Some(format!("cap{n}"));
            p.note = "x".repeat(1_400);
            r.pieces.push(p);
        }
        let mut last = Piece::pending(6, "last", 2);
        last.state = State::Running;
        r.pieces.push(last);
        pieces::write(&mut t, &r);
        let rec = pieces::record(&t);
        let notes = before_notes(&t, &rec, 2).unwrap();
        assert!(notes.contains(&"x".repeat(1_400)), "the first notes are whole");
        assert!(notes.contains("wsp piece show m-1 5"), "the fifth is a caption past 6,000 characters: {}", notes.len());
        assert!(notes.matches("xxxxxxxxxx").count() <= 4 * 140, "{}", notes.len());
    }
}
