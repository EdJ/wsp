//! A larger work in defined parts, inside one row: `docs/pieces.md`, wsp-501.
//!
//! A row carries its pieces under `## Pieces`, one entry each, in the order they
//! were added, after a `cap:` line. Modelled on [`crate::legs`], and for the same
//! reason: the record is the entry, and everything that asks about a piece asks
//! the entry.
//!
//! ```text
//! ## Pieces
//! cap: 3
//! - 1 · contract types · batch 1 · done · pane cpd-31 · tree wsp-501-1 · ended
//!   overview: Types and a fixture each for runstate, worklists, taskpage …
//!   done-when: each module's pass3 test green; the fixtures read back
//!   check: tsc (2m): cd spikes/web-chrome && npx tsc --noEmit
//!   checked: green at 52c768d
//!   note: Done: 52c768d types+fixtures. Where: src/contract/*.ts. Left: …
//!   caption: types and fixtures read back
//! - 3 · rail places · batch 2 · claude opus high · pending
//!   why: decides between the rail's two layouts the parity list leaves open
//!   overview: …
//! ```
//!
//! This module is the record: the parse and write, the rules every write is
//! checked against, and `wsp piece add | show | drop | cap`. A piece's seat, its
//! tree, the batch that opens and closes and the run are later rows
//! (pieces-seat, pieces-close, pieces-run); the clauses they write are read and
//! kept here so those rows have nowhere to put a second record.
//!
//! # What the doc left open (wsp-502)
//!
//! - **`by N` on an entry** records which seated piece added it. The doc lets a
//!   seated piece edit "only pieces it added", and nothing else in the entry can
//!   say so. It is written only for a piece added by a piece.
//! - **A batch has opened** once any of its pieces is neither `pending` nor
//!   `dropped`: a dropped piece was a pending one that never ran, so dropping
//!   one does not close a batch to additions.
//! - **`next`** is one new batch after the last, resolved once per call, so a
//!   `--from FILE` of several `batch: next` pieces files them in the one batch.
//! - **A dropped entry's `why:` is the reason it was dropped**, so the tier rule
//!   (a `why:` only on a non-default tier) is not asked of it.
//! - **The planner** is the seat that files rows: a governor or the primary.
//!   There is no other planner seat to name.
//! - **Every write is judged by what it changes**, as legs' are: a record that
//!   was already wrong in the same way before the write does not make an
//!   unrelated write refuse.

use serde_json::{json, Value};

use crate::model::{Policy, Status, Task};
use crate::util;
use crate::verification::Ending;

/// The heading the pieces sit under on the row.
pub(crate) const SECTION: &str = "Pieces";

/// Most pieces a row carries, dropped, promoted and rework counted.
pub(crate) const MAX_PIECES: usize = 20;
/// How many of a row's pieces run at once, when the row says nothing.
pub(crate) const DEFAULT_CAP: usize = 3;
/// A title, in characters.
pub(crate) const TITLE_CAP: usize = 80;
/// An overview.
pub(crate) const OVERVIEW_CAP: usize = 3_000;
/// A done-when.
pub(crate) const DONE_WHEN_CAP: usize = 600;
/// A note.
pub(crate) const NOTE_CAP: usize = 1_500;
/// A `why:`, as a leg's is.
pub(crate) const WHY_CAP: usize = crate::legs::WHY_CAP;

/// Where a piece is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Pending,
    Running,
    Done,
    Blocked,
    Dropped,
    Promoted,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Running => "running",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Dropped => "dropped",
            State::Promoted => "promoted",
        }
    }

    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "pending" => State::Pending,
            "running" => State::Running,
            "done" => State::Done,
            "blocked" => State::Blocked,
            "dropped" => State::Dropped,
            "promoted" => State::Promoted,
            _ => return None,
        })
    }
}

/// One piece of a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Piece {
    /// Its number in the row, from 1, never reused.
    pub n: usize,
    pub title: String,
    pub batch: usize,
    /// What it runs on. Always a policy: an entry that names none reads as
    /// [`crate::legs::default_policy`] and is written without it.
    pub policy: Policy,
    pub state: State,
    /// The seat its agent was started in. Written by pieces-seat.
    pub pane: Option<String>,
    /// The tree it stands in. Written by pieces-seat.
    pub tree: Option<String>,
    /// When it opened, written with `running` and kept: the end line's
    /// "after 12m" reads it, as a leg's does.
    pub started: Option<String>,
    pub ending: Ending,
    /// The piece that added it, when a seated piece did.
    pub by: Option<usize>,
    pub overview: String,
    pub done_when: String,
    /// The `## Checks` line format, label first, one per `check:` line.
    pub checks: Vec<String>,
    pub checked: Option<String>,
    pub note: String,
    pub caption: Option<String>,
    /// Why this tier for a piece that is not on the default, or why it was
    /// dropped.
    pub why: Option<String>,
    /// The row a promoted piece became.
    pub promoted: Option<String>,
}

impl Piece {
    pub(crate) fn pending(n: usize, title: &str, batch: usize) -> Piece {
        Piece {
            n,
            title: title.trim().to_string(),
            batch,
            policy: crate::legs::default_policy(),
            state: State::Pending,
            pane: None,
            tree: None,
            started: None,
            ending: Ending::Standing,
            by: None,
            overview: String::new(),
            done_when: String::new(),
            checks: Vec::new(),
            checked: None,
            note: String::new(),
            caption: None,
            why: None,
            promoted: None,
        }
    }

    /// The id a piece is addressed by once it is one word: `<row>-<n>`.
    pub(crate) fn id(&self, row: &str) -> String {
        format!("{row}-{}", self.n)
    }

    fn head(&self) -> String {
        let mut out = format!("- {} · {} · batch {}", self.n, util::fold(&self.title), self.batch);
        if !crate::legs::is_default(&self.policy) {
            out.push_str(&format!(" · {}", crate::legs::policy_line(&self.policy)));
        }
        out.push_str(&format!(" · {}", self.state.as_str()));
        let mut clause = |k: &str, v: &Option<String>| {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                out.push_str(&format!(" · {k} {v}"));
            }
        };
        clause("pane", &self.pane);
        clause("tree", &self.tree);
        clause("since", &self.started);
        clause("by", &self.by.map(|b| b.to_string()));
        match &self.ending {
            Ending::Standing => {}
            Ending::Ended => out.push_str(" · ended"),
            Ending::Failed(why) => out.push_str(&format!(" · end failed: {}", util::fold(why))),
        }
        out
    }

    /// The entry whole, as it is written.
    pub(crate) fn lines(&self) -> String {
        let mut out = self.head();
        let mut field = |key: &str, text: &str| {
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            let mut lines = text.lines();
            out.push_str(&format!("\n  {key}: {}", lines.next().unwrap_or_default().trim_end()));
            for l in lines {
                // Four spaces, so a line of somebody's text can never read as
                // a key at two.
                out.push('\n');
                if !l.trim().is_empty() {
                    out.push_str(&format!("    {}", l.trim_end()));
                }
            }
        };
        field("overview", &self.overview);
        field("done-when", &self.done_when);
        for c in &self.checks {
            field("check", c);
        }
        field("checked", self.checked.as_deref().unwrap_or_default());
        field("note", &self.note);
        field("caption", self.caption.as_deref().unwrap_or_default());
        field("why", self.why.as_deref().unwrap_or_default());
        field("promoted", self.promoted.as_deref().unwrap_or_default());
        out
    }

    fn parse_head(line: &str) -> Option<Piece> {
        let rest = line.strip_prefix("- ")?;
        let mut clauses = rest.split(" · ").map(str::trim);
        let n: usize = clauses.next()?.parse().ok()?;
        let title = clauses.next().filter(|t| !t.is_empty())?;
        let mut p = Piece::pending(n, title, 0);
        let mut batched = false;
        let mut stated = false;
        for c in clauses {
            if let Some(b) = c.strip_prefix("batch ").and_then(|b| b.trim().parse().ok()) {
                p.batch = b;
                batched = true;
            } else if let Some(s) = State::parse(c) {
                p.state = s;
                stated = true;
            } else if c == "ended" {
                p.ending = Ending::Ended;
            } else if let Some(why) = c.strip_prefix("end failed:") {
                p.ending = Ending::Failed(why.trim().to_string());
            } else if let Some(v) = c.strip_prefix("pane ") {
                p.pane = Some(v.trim().to_string());
            } else if let Some(v) = c.strip_prefix("tree ") {
                p.tree = Some(v.trim().to_string());
            } else if let Some(v) = c.strip_prefix("since ") {
                p.started = Some(v.trim().to_string());
            } else if let Some(v) = c.strip_prefix("by ") {
                p.by = v.trim().parse().ok();
            } else if let Some(policy) = Policy::of_label(c) {
                p.policy = policy;
            }
        }
        (batched && stated).then_some(p)
    }
}

/// A row's pieces and how many run at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub cap: usize,
    /// The batches the run has closed, in the order it closed them: written
    /// `closed: 1 2` under `cap:` by the run (wsp-506), so a second advance finds
    /// a batch closed and does not merge it again, and the next batch opens only
    /// behind it.
    pub closed: Vec<usize>,
    pub pieces: Vec<Piece>,
}

impl Default for Record {
    fn default() -> Record {
        Record { cap: DEFAULT_CAP, closed: Vec::new(), pieces: Vec::new() }
    }
}

/// The pieces written on a row, in file order.
pub(crate) fn record(t: &Task) -> Record {
    let mut rec = Record::default();
    let Some(text) = t.section(SECTION) else { return rec };
    const KEYS: [&str; 8] = ["overview", "done-when", "check", "checked", "note", "caption", "why", "promoted"];
    let mut out: Vec<(Piece, Vec<(String, String)>)> = Vec::new();
    for line in text.lines() {
        if line.starts_with("- ") {
            if let Some(p) = Piece::parse_head(line) {
                out.push((p, Vec::new()));
            }
            continue;
        }
        let Some((_, fields)) = out.last_mut() else {
            if let Some(n) = line.trim().strip_prefix("cap:").and_then(|n| n.trim().parse().ok()) {
                rec.cap = n;
            }
            if let Some(ns) = line.trim().strip_prefix("closed:") {
                rec.closed = ns.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            }
            continue;
        };
        let two = line.strip_prefix("  ").filter(|r| !r.starts_with(' '));
        if let Some((k, v)) = two.and_then(|r| r.split_once(':')).filter(|(k, _)| KEYS.contains(k)) {
            fields.push((k.to_string(), v.strip_prefix(' ').unwrap_or(v).to_string()));
        } else if let Some((_, f)) = fields.last_mut() {
            f.push('\n');
            f.push_str(line.strip_prefix("    ").unwrap_or(line.trim_start()));
        }
    }
    rec.pieces = out
        .into_iter()
        .map(|(mut p, f)| {
            let one = |k: &str| f.iter().rev().find(|(fk, _)| fk == k).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty());
            p.overview = one("overview").unwrap_or_default();
            p.done_when = one("done-when").unwrap_or_default();
            p.checks = f.iter().filter(|(k, _)| k == "check").map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
            p.checked = one("checked");
            p.note = one("note").unwrap_or_default();
            p.caption = one("caption");
            p.why = one("why");
            p.promoted = one("promoted");
            p
        })
        .collect();
    rec
}

/// Write the pieces back under the heading, in the order given. Does not touch
/// the row's `updated`, as legs' write does not.
pub(crate) fn write(t: &mut Task, rec: &Record) {
    let mut parts = Vec::new();
    if !rec.pieces.is_empty() || rec.cap != DEFAULT_CAP {
        parts.push(format!("cap: {}", rec.cap));
    }
    if !rec.closed.is_empty() {
        parts.push(format!("closed: {}", rec.closed.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(" ")));
    }
    parts.extend(rec.pieces.iter().map(Piece::lines));
    crate::model::set_section_in(&mut t.body, SECTION, &parts.join("\n"));
}

/// Whether the row carries pieces: it has a `## Pieces` section.
pub(crate) fn staged(t: &Task) -> bool {
    t.section(SECTION).is_some()
}

/// Why `wsp leg add` may not give this row legs, when it has pieces.
pub(crate) fn refuses_legs(t: &Task) -> Option<String> {
    staged(t).then(|| {
        format!(
            "{} has `## {SECTION}`, and a row has pieces or legs, not both — \
             `wsp piece add` stages another piece, or the row is split into legs in a row of its own",
            t.id
        )
    })
}

/// Why `wsp piece add` may not give this row pieces, when it has legs.
pub(crate) fn refuses_pieces(t: &Task) -> Option<String> {
    t.section(crate::legs::SECTION).is_some().then(|| {
        format!(
            "{} has `## {}`, and a row has pieces or legs, not both — \
             the legs are its plan; a larger work in separable parts is a row with pieces and no legs",
            t.id,
            crate::legs::SECTION
        )
    })
}

// ---- the batches --------------------------------------------------------------

/// The last batch any piece is in; 0 for none.
pub(crate) fn last_batch(rec: &Record) -> usize {
    rec.pieces.iter().map(|p| p.batch).max().unwrap_or(0)
}

/// Whether `batch` has opened: any piece of it is neither pending nor dropped.
pub(crate) fn opened(rec: &Record, batch: usize) -> bool {
    rec.pieces.iter().any(|p| p.batch == batch && !matches!(p.state, State::Pending | State::Dropped))
}

/// Whether a piece is settled for its batch's close: done, dropped or promoted.
pub(crate) fn settled(p: &Piece) -> bool {
    matches!(p.state, State::Done | State::Dropped | State::Promoted)
}

/// The batch the run owes a close: the lowest one not yet closed whose pieces are
/// all settled, with every batch below it closed. `None` while the lowest batch
/// not closed still has a piece to end, or when every batch is closed.
pub(crate) fn close_owed(rec: &Record) -> Option<usize> {
    for b in 1..=last_batch(rec) {
        let mut members = rec.pieces.iter().filter(|p| p.batch == b).peekable();
        if members.peek().is_none() || rec.closed.contains(&b) {
            continue;
        }
        return members.all(settled).then_some(b);
    }
    None
}

/// Every batch closed and nothing left to run: what the run types the row's
/// review on. A record with no piece done is not finished; it was dropped whole.
pub(crate) fn finished(rec: &Record) -> bool {
    rec.pieces.iter().any(|p| p.state == State::Done)
        && rec.pieces.iter().all(settled)
        && (1..=last_batch(rec)).all(|b| rec.closed.contains(&b) || !rec.pieces.iter().any(|p| p.batch == b))
}

// ---- the rules ------------------------------------------------------------------

/// What a record must satisfy, as the first thing that is wrong with it.
pub(crate) fn check(rec: &Record) -> Result<(), String> {
    if rec.pieces.len() > MAX_PIECES {
        return Err(format!(
            "{} pieces is over the {MAX_PIECES} a row carries, dropped, promoted and rework counted — a work wider than that is rows",
            rec.pieces.len()
        ));
    }
    if rec.cap == 0 || rec.cap > MAX_PIECES {
        return Err(format!("cap: {} — how many pieces run at once is 1 to {MAX_PIECES}", rec.cap));
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in &rec.pieces {
        let at = format!("piece {}", p.n);
        if !seen.insert(p.n) {
            return Err(format!("two pieces are numbered {}", p.n));
        }
        if p.n == 0 || p.batch == 0 {
            return Err(format!("{at}: numbers and batches count from 1"));
        }
        if p.title.trim().is_empty() || p.title.contains('\n') || p.title.contains(" · ") {
            return Err(format!("{at}: a title is one line of words and no ` · `"));
        }
        let over = |what: &str, text: &str, cap: usize| {
            let n = text.trim().chars().count();
            (n > cap).then(|| format!("the {what} of {at} is {n} characters, over the {cap} cap"))
        };
        if let Some(e) = over("title", &p.title, TITLE_CAP)
            .or_else(|| over("overview", &p.overview, OVERVIEW_CAP))
            .or_else(|| over("done-when", &p.done_when, DONE_WHEN_CAP))
            .or_else(|| over("note", &p.note, NOTE_CAP))
            .or_else(|| over("why", p.why.as_deref().unwrap_or_default(), WHY_CAP))
        {
            return Err(e);
        }
        for c in &p.checks {
            if !is_check_line(c) {
                return Err(format!("{at}: `{c}` is not a check — `label (time): command`"));
            }
        }
        if p.state == State::Dropped {
            continue;
        }
        match (crate::legs::is_default(&p.policy), p.why.is_some()) {
            (false, false) => {
                return Err(format!(
                    "{at} is on {}, not the default {}, and gives no reason — `--why \"…\"` says what \
                     in that piece's own work needs it, or leave --agent off",
                    crate::legs::policy_line(&p.policy),
                    crate::legs::DEFAULT_AGENT
                ))
            }
            (true, true) => {
                return Err(format!(
                    "{at} is on the default {} and has a `why:`, which would be a reason for nothing — \
                     drop it, or give the piece the tier the reason is for",
                    crate::legs::DEFAULT_AGENT
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether a `check:` line is the `## Checks` line format with its label first:
/// `label (time): command`, the label being what the step is called.
fn is_check_line(c: &str) -> bool {
    let Some(step) = crate::check::step_of(c, crate::check::Of::Task) else { return false };
    // A head that does not parse (`label (never)`) is left in the command whole.
    step.command != c.trim()
        && c.split_once(": ").is_some_and(|(head, _)| head.split_whitespace().next() == Some(step.name.as_str()))
}

/// [`check`] for a write: refused only if the record it leaves is wrong in a way
/// the record it started from was not.
fn judged(old: &Record, new: &Record) -> Result<(), String> {
    match check(new) {
        Ok(()) => Ok(()),
        Err(e) if check(old).err().as_deref() == Some(e.as_str()) => Ok(()),
        Err(e) => Err(e),
    }
}

// ---- who may write -----------------------------------------------------------------

/// Who is writing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Actor {
    /// No seat: a person at a terminal.
    Person,
    /// The primary seat, which files rows.
    Primary,
    /// A governor, which files rows too.
    Governor,
    /// The seat holding the row's claim.
    Claimant,
    /// The seat of this piece of the row (binding `"piece"`).
    Piece(usize),
    /// Any other seat.
    Other,
}

/// What may be written at all: the row must be open for it, and the writer one
/// the doc names.
fn gate(actor: &Actor, status: Status) -> Result<(), String> {
    if !matches!(status, Status::Todo | Status::Doing) {
        return Err(format!("pieces are written on a todo or doing row, and this one is {}", status.as_str()));
    }
    if *actor == Actor::Other {
        return Err("only a person, the primary seat, a governor, the seat holding the row or one of its pieces may write its pieces".into());
    }
    Ok(())
}

/// A seated piece adds only to batches after its own.
fn may_add(old: &Record, actor: &Actor, batch: usize) -> Result<(), String> {
    let Actor::Piece(me) = actor else { return Ok(()) };
    let Some(mine) = old.pieces.iter().find(|p| p.n == *me && p.state == State::Running) else {
        return Err(format!("piece {me} is not running, and only a running piece may add pieces"));
    };
    if batch <= mine.batch {
        return Err(format!("piece {me} is in batch {} and may add only to a later batch, not {batch}", mine.batch));
    }
    Ok(())
}

/// A seated piece changes only the pieces it added; anyone else, any pending one.
fn may_change(actor: &Actor, p: &Piece) -> Result<(), String> {
    if p.state != State::Pending {
        return Err(format!("piece {} is {}, and only a pending piece can be changed", p.n, p.state.as_str()));
    }
    match actor {
        Actor::Piece(me) if p.by != Some(*me) => {
            Err(format!("piece {me} may change only the pieces it added, and piece {} is not one of them", p.n))
        }
        _ => Ok(()),
    }
}

// ---- the writes -----------------------------------------------------------------------

/// Where a new piece goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum At {
    Batch(usize),
    /// A new batch after the last.
    Next,
}

/// A piece to add.
#[derive(Clone, Debug)]
pub(crate) struct New {
    pub title: String,
    pub at: At,
    pub overview: String,
    pub done_when: String,
    pub checks: Vec<String>,
    pub agent: Option<String>,
    pub why: Option<String>,
}

/// The record `new` leaves when added, or why it may not be.
pub(crate) fn add(old: &Record, new: New, actor: &Actor, status: Status) -> Result<Record, String> {
    gate(actor, status)?;
    let policy = match new.agent.as_deref() {
        None => crate::legs::default_policy(),
        Some(a) => Policy::parse(a)
            .ok_or_else(|| format!("`{a}` is not a policy — a kind, then a model and an effort, as `claude opus high`"))?,
    };
    if new.title.trim().is_empty() {
        return Err("a piece needs a title".into());
    }
    if new.overview.trim().is_empty() {
        return Err("a piece needs an overview: what it is to do".into());
    }
    if new.done_when.trim().is_empty() {
        return Err("a piece needs a done-when: how its seat knows it is finished".into());
    }
    let last = last_batch(old);
    let batch = match new.at {
        At::Next => last + 1,
        At::Batch(0) => return Err("batches count from 1".into()),
        At::Batch(b) if b > last + 1 => {
            return Err(format!("batch {b} would leave a gap — the last batch is {last}, and `next` is {}", last + 1))
        }
        At::Batch(b) => b,
    };
    if opened(old, batch) {
        return Err(format!("batch {batch} has opened, and a piece is added only to a batch that has not — `--batch next` is {}", last + 1));
    }
    may_add(old, actor, batch)?;
    let mut rec = old.clone();
    let n = old.pieces.iter().map(|p| p.n).max().unwrap_or(0) + 1;
    let mut p = Piece::pending(n, &new.title, batch);
    p.policy = policy;
    p.why = new.why.map(|w| w.trim().to_string()).filter(|w| !w.is_empty());
    p.overview = new.overview.trim().to_string();
    p.done_when = new.done_when.trim().to_string();
    p.checks = new.checks.into_iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
    p.by = match actor {
        Actor::Piece(me) => Some(*me),
        _ => None,
    };
    rec.pieces.push(p);
    judged(old, &rec)?;
    Ok(rec)
}

/// [`add`] for several, in order, as one write. `next` is resolved once, so the
/// pieces that say it share a batch.
pub(crate) fn add_all(old: &Record, news: Vec<New>, actor: &Actor, status: Status) -> Result<Record, String> {
    let next = last_batch(old) + 1;
    let mut rec = old.clone();
    let many = news.len() > 1;
    for mut n in news {
        if n.at == At::Next {
            n.at = At::Batch(next);
        }
        let title = util::truncate(&util::fold(&n.title), 40);
        rec = add(&rec, n, actor, status).map_err(|e| if many { format!("{title}: {e}") } else { e })?;
    }
    Ok(rec)
}

/// The record with `new` written as a batch of its own right after batch
/// `after`, and the number it was given. **The only write that inserts a
/// batch**: every batch after `after` moves up by one. It is the run's (a closing
/// batch adds its merge or fix piece), so it takes no actor, and it is allowed
/// because nothing after `after` has opened; if something has, it refuses and
/// the record is as it was.
pub(crate) fn insert_batch_after(old: &Record, after: usize, new: New, status: Status) -> Result<(Record, usize), String> {
    gate(&Actor::Person, status)?;
    if !old.pieces.iter().any(|p| p.batch == after) {
        return Err(format!("no piece is in batch {after}, so nothing can be inserted after it"));
    }
    if let Some(b) = (after + 1..=last_batch(old)).find(|b| opened(old, *b)) {
        return Err(format!("batch {b} has opened, and a batch is inserted only before batches that have not"));
    }
    // Adding to the batch after the last is the ordinary write, so it carries
    // the piece's own rules; the batches that follow are then moved up behind it.
    let mut shifted = old.clone();
    for p in shifted.pieces.iter_mut().filter(|p| p.batch > after) {
        p.batch += 1;
    }
    for b in shifted.closed.iter_mut().filter(|b| **b > after) {
        *b += 1;
    }
    let rec = add(&shifted, New { at: At::Batch(after + 1), ..new }, &Actor::Person, status)?;
    let n = rec.pieces.last().map(|p| p.n).unwrap_or(0);
    Ok((rec, n))
}

/// The record with piece `n` dropped, for a reason. A pending piece only.
pub(crate) fn drop_piece(old: &Record, n: usize, why: &str, actor: &Actor, status: Status) -> Result<Record, String> {
    gate(actor, status)?;
    let why = why.trim();
    if why.is_empty() {
        return Err("a drop takes the reason, which stays on the entry".into());
    }
    let mut rec = old.clone();
    let p = rec.pieces.iter_mut().find(|p| p.n == n).ok_or_else(|| format!("no piece {n}"))?;
    may_change(actor, p)?;
    p.state = State::Dropped;
    p.why = Some(why.to_string());
    judged(old, &rec)?;
    Ok(rec)
}

/// A seated piece moves out or back only a piece it added; anyone else, any
/// piece that is `pending` or `blocked` (never one running, done, dropped or
/// already promoted).
fn may_move(actor: &Actor, p: &Piece) -> Result<(), String> {
    if !matches!(p.state, State::Pending | State::Blocked) {
        return Err(format!("piece {} is {}, and only a pending or blocked piece can be promoted", p.n, p.state.as_str()));
    }
    match actor {
        Actor::Piece(me) if p.by != Some(*me) => {
            Err(format!("piece {me} may change only the pieces it added, and piece {} is not one of them", p.n))
        }
        _ => Ok(()),
    }
}

/// The record with piece `n` promoted to the row `new_id`: `promoted: <id>`, and
/// the piece no longer holds its batch (a promoted piece is settled). A pending
/// or blocked piece only. The row itself is the caller's to file, so this takes
/// the id it was given.
pub(crate) fn promote(old: &Record, n: usize, new_id: &str, actor: &Actor, status: Status) -> Result<Record, String> {
    gate(actor, status)?;
    let mut rec = old.clone();
    let p = rec.pieces.iter_mut().find(|p| p.n == n).ok_or_else(|| format!("no piece {n}"))?;
    may_move(actor, p)?;
    p.state = State::Promoted;
    p.promoted = Some(new_id.trim().to_string());
    judged(old, &rec)?;
    Ok(rec)
}

/// The sub-task a piece becomes: its title, its overview with the done-when
/// under it as the row's overview, and its `check:` lines as `## Checks`.
/// `(title, overview, checks)`.
pub(crate) fn as_row(p: &Piece) -> (String, String, String) {
    let mut overview = p.overview.trim().to_string();
    if !p.done_when.trim().is_empty() {
        overview.push_str(&format!("\n\nDone when: {}", p.done_when.trim()));
    }
    (p.title.clone(), overview, p.checks.join("\n"))
}

/// The record with piece `n` back to `pending`, for a reason the caller logs: a
/// `blocked` piece, or a `running` one whose seat is gone (the caller has seen
/// to the seat). The seat, tree and start go from the entry; its note, caption
/// and checks stay for the seat that takes it next. Not a seated piece's to do.
pub(crate) fn reset_piece(old: &Record, n: usize, why: &str, actor: &Actor, status: Status) -> Result<Record, String> {
    gate(actor, status)?;
    if let Actor::Piece(me) = actor {
        return Err(format!("piece {me} may add pieces to later batches, and putting another piece back is not its to do"));
    }
    if why.trim().is_empty() {
        return Err("a reset takes the reason, which goes on the row's log".into());
    }
    let mut rec = old.clone();
    let p = rec.pieces.iter_mut().find(|p| p.n == n).ok_or_else(|| format!("no piece {n}"))?;
    if !matches!(p.state, State::Blocked | State::Running) {
        return Err(format!("piece {n} is {}, and only a blocked or running piece can be reset", p.state.as_str()));
    }
    p.state = State::Pending;
    p.pane = None;
    p.tree = None;
    p.started = None;
    p.ending = Ending::Standing;
    judged(old, &rec)?;
    Ok(rec)
}

/// The record with `cap:` set. Not a seated piece's to change.
pub(crate) fn set_cap(old: &Record, cap: usize, actor: &Actor, status: Status) -> Result<Record, String> {
    gate(actor, status)?;
    if let Actor::Piece(me) = actor {
        return Err(format!("piece {me} may add pieces to later batches, and the cap on how many run is not its to change"));
    }
    let mut rec = old.clone();
    rec.cap = cap;
    judged(old, &rec)?;
    Ok(rec)
}

// ---- the verdict ---------------------------------------------------------------------------

/// The `check:` lines of every done piece as steps, labelled `<n>.<label>`:
/// what `wsp check <row>` runs after the row's own.
pub(crate) fn check_steps(t: &Task) -> Vec<(usize, crate::check::Step)> {
    let mut out = Vec::new();
    for p in record(t).pieces.iter().filter(|p| p.state == State::Done) {
        for c in &p.checks {
            if let Some(mut s) = crate::check::step_of(c, crate::check::Of::Task) {
                s.name = format!("{}.{}", p.n, s.name);
                out.push((p.n, s));
            }
        }
    }
    out
}

/// What opens a rework piece's overview when a verifier sent the row back.
const VERIFIER_HEAD: &str = "The verifier blocked this row. Its verdict, which is what is owed:\n\n";

/// Whether a rework piece is still owed in the last batch, so a verdict
/// recorded twice is one send-back and not two.
fn rework_open(rec: &Record) -> bool {
    let last = last_batch(rec);
    rec.pieces.iter().any(|p| {
        p.batch == last && (p.title == "rework" || p.title.starts_with("rework of ")) && matches!(p.state, State::Pending | State::Running | State::Blocked)
    })
}

/// The pieces a verdict names, by id (`<row>-<n>`): done pieces only, since
/// there is nothing to rework in one that was dropped or never ran.
pub(crate) fn named_in(rec: &Record, row: &str, verdict: &str) -> Vec<usize> {
    rec.pieces.iter().filter(|p| p.state == State::Done && crate::legs::names(verdict, &p.id(row))).map(|p| p.n).collect()
}

/// The record a blocking verdict leaves: one rework piece per piece it names,
/// in a new batch after the last, on the named piece's tier and `why:` prefixed
/// `rework of N:`, with the verdict as its overview and the named piece's
/// done-when and checks. A verdict that names none gets one over the row, on
/// the default tier. `Ok(None)` when a rework is already owed in the last batch.
pub(crate) fn rework_by(old: &Record, row: &str, verdict: &str, head: &str, status: Status) -> Result<Option<Record>, String> {
    if old.pieces.is_empty() || rework_open(old) {
        return Ok(None);
    }
    let verdict = verdict.trim();
    let overview = format!("{head}{}", util::truncate(verdict, OVERVIEW_CAP - head.chars().count()));
    let named = named_in(old, row, verdict);
    let news: Vec<New> = if named.is_empty() {
        vec![New {
            title: "rework".into(),
            at: At::Next,
            overview,
            done_when: "each item the verdict says is owed is done, and the row's own check is green".into(),
            checks: Vec::new(),
            agent: None,
            why: None,
        }]
    } else {
        named
            .iter()
            .filter_map(|n| old.pieces.iter().find(|p| p.n == *n))
            .map(|p| New {
                title: format!("rework of {}", p.n),
                at: At::Next,
                overview: overview.clone(),
                done_when: p.done_when.clone(),
                checks: p.checks.clone(),
                agent: (!crate::legs::is_default(&p.policy)).then(|| crate::legs::policy_line(&p.policy)),
                why: (!crate::legs::is_default(&p.policy))
                    .then(|| util::truncate(&format!("rework of {}: {}", p.n, p.why.as_deref().unwrap_or_default()), WHY_CAP)),
            })
            .collect()
    };
    add_all(old, news, &Actor::Person, status).map(Some)
}

/// What a piece's last finished seat ran as: the piece number, the agent
/// (empty when unknown), the turns. The caller reads it off the attempts, so
/// this module names nothing of the timeline.
pub(crate) struct PieceRan {
    pub n: usize,
    pub ran: String,
    pub turns: usize,
}

/// Fold a landed row's `## Pieces` into its log: one line a piece, and the
/// section goes. `ran` is what each piece's seat ran as, the last one a piece
/// wins. Returns the lines written.
pub(crate) fn fold_at_land(t: &mut Task, ran: &[PieceRan]) -> Vec<String> {
    let rec = record(t);
    if rec.pieces.is_empty() {
        if staged(t) {
            crate::model::set_section_in(&mut t.body, SECTION, "");
        }
        return Vec::new();
    }
    let lines: Vec<String> = rec
        .pieces
        .iter()
        .map(|p| {
            let head = format!("piece {} · {}", p.n, util::fold(&p.title));
            match p.state {
                State::Dropped => format!("{head} · dropped: {}", util::fold(p.why.as_deref().unwrap_or("no reason given"))),
                State::Promoted => format!("{head} · promoted to {}", p.promoted.as_deref().unwrap_or("a row")),
                _ => {
                    let mut line = format!("{head} · {}", p.state.as_str());
                    if let Some(c) = p.caption.as_deref().filter(|c| !c.is_empty()) {
                        line.push_str(&format!(" · {}", util::fold(c)));
                    }
                    if let Some(a) = ran.iter().rev().find(|a| a.n == p.n) {
                        let on = if a.ran.is_empty() { "?" } else { a.ran.as_str() };
                        line.push_str(&format!(" · ran {on} {} turns", a.turns));
                    }
                    line.push_str(match (p.checks.is_empty(), p.checked.as_deref()) {
                        (true, _) => " · no checks",
                        (false, Some(c)) if c.starts_with("green") => " · checks green",
                        (false, _) => " · checks not green",
                    });
                    if !p.done_when.is_empty() {
                        line.push_str(&format!(" · done-when: {}", util::truncate(&util::fold(&p.done_when), 200)));
                    }
                    line
                }
            }
        })
        .collect();
    for l in &lines {
        t.log(l);
    }
    crate::model::set_section_in(&mut t.body, SECTION, "");
    lines
}

/// The folded line of piece `n` in a landed row's log, if it was folded.
pub(crate) fn folded_line(t: &Task, n: usize) -> Option<String> {
    let want = format!("piece {n} · ");
    t.section("Log")?.lines().find_map(|l| {
        let at = l.find(&want)?;
        (l[..at].split_whitespace().count() <= 2)
            .then(|| l[at..].to_string())
    })
}

// ---- seen -------------------------------------------------------------------------------

/// One line a piece, for `wsp show`: the number, the title, the batch, the
/// state, and the caption once there is one.
pub(crate) fn table(rec: &Record) -> String {
    let mut out = vec![format!("cap: {}", rec.cap)];
    for p in &rec.pieces {
        let mut line = format!("- {} · {} · batch {}", p.n, util::truncate(&util::fold(&p.title), TITLE_CAP), p.batch);
        if !crate::legs::is_default(&p.policy) {
            line.push_str(&format!(" · {}", crate::legs::policy_line(&p.policy)));
        }
        line.push_str(&format!(" · {}", p.state.as_str()));
        if let Some(c) = p.caption.as_deref().or(p.promoted.as_deref()).or(if p.state == State::Dropped { p.why.as_deref() } else { None }) {
            line.push_str(&format!(" · {}", util::truncate(&util::fold(c), 80)));
        }
        out.push(line);
    }
    out.join("\n")
}

/// The entries whole, for `wsp show --pieces`.
pub(crate) fn whole(rec: &Record) -> String {
    let mut out = vec![format!("cap: {}", rec.cap)];
    out.extend(rec.pieces.iter().map(Piece::lines));
    out.join("\n")
}

/// `wsp show`'s `## Pieces`: the table, or with `whole` every entry in full.
pub(crate) fn section_for_show(t: &Task, whole_entries: bool) -> Option<String> {
    let rec = record(t);
    (!rec.pieces.is_empty()).then(|| if whole_entries { whole(&rec) } else { table(&rec) })
}

/// A row's pieces for `--json`: `[{n, id, title, batch, state, seat, tree,
/// caption}]`.
pub(crate) fn json(t: &Task) -> Value {
    let rec = record(t);
    Value::Array(
        rec.pieces
            .iter()
            .map(|p| {
                json!({
                    "n": p.n, "id": p.id(&t.id), "title": p.title, "batch": p.batch,
                    "state": p.state.as_str(), "seat": p.pane, "tree": p.tree, "caption": p.caption,
                })
            })
            .collect(),
    )
}


pub(crate) fn at_of(word: &str) -> Result<At, String> {
    match word.trim() {
        "next" => Ok(At::Next),
        w => w.parse().map(At::Batch).map_err(|_| format!("batch `{w}` is a number from 1, or `next`")),
    }
}

/// The pieces a `--from` file holds: one `### title` block each, with `batch:`,
/// `done-when:`, `check:` (and optionally `agent:` and `why:`) lines, and what
/// else is under the heading as the overview.
pub(crate) fn parse_blocks(text: &str) -> Result<Vec<New>, String> {
    let mut out: Vec<New> = Vec::new();
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("### ") {
            out.push(New {
                title: title.trim().to_string(),
                at: At::Next,
                overview: String::new(),
                done_when: String::new(),
                checks: Vec::new(),
                agent: None,
                why: None,
            });
            continue;
        }
        let Some(cur) = out.last_mut() else {
            if line.trim().is_empty() {
                continue;
            }
            return Err(format!("`{}` comes before the first `### title`", util::truncate(line.trim(), 40)));
        };
        let keyed = |k: &str| line.strip_prefix(k).and_then(|r| r.strip_prefix(':')).map(|v| v.trim().to_string());
        if let Some(v) = keyed("batch") {
            cur.at = at_of(&v).map_err(|e| format!("{}: {e}", cur.title))?;
        } else if let Some(v) = keyed("done-when") {
            cur.done_when = v;
        } else if let Some(v) = keyed("check") {
            cur.checks.push(v);
        } else if let Some(v) = keyed("agent") {
            cur.agent = Some(v);
        } else if let Some(v) = keyed("why") {
            cur.why = Some(v);
        } else {
            cur.overview.push_str(line);
            cur.overview.push('\n');
        }
    }
    if out.is_empty() {
        return Err("the file holds no `### title` block".into());
    }
    Ok(out)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn new(title: &str, at: At) -> New {
        New {
            title: title.into(),
            at,
            overview: format!("do {title}"),
            done_when: format!("{title} is done"),
            checks: Vec::new(),
            agent: None,
            why: None,
        }
    }

    const TODO: Status = Status::Todo;

    fn put(rec: &Record, n: New) -> Record {
        add(rec, n, &Actor::Person, TODO).expect("the add is accepted")
    }

    fn with_state(mut rec: Record, n: usize, state: State) -> Record {
        rec.pieces.iter_mut().find(|p| p.n == n).unwrap().state = state;
        rec
    }

    fn row(body: &str) -> Task {
        let mut t = Task::new("a row", "m-1");
        t.body = format!("## Overview\nthe work\n\n{body}\n## Log\n- 2026-10-18 claimed\n");
        t
    }

    /// wsp-506: the batches the run closed read back, a batch is owed its close
    /// only once its pieces are settled and the ones below it are closed, and the
    /// row is finished once every batch is closed.
    #[test]
    fn closed_batches_read_back_and_say_which_close_is_owed_and_when_the_row_is_finished() {
        let piece = |n: usize, batch: usize, state: State| Piece { state, ..Piece::pending(n, &format!("p{n}"), batch) };
        let mut rec = Record {
            cap: 2,
            closed: Vec::new(),
            pieces: vec![piece(1, 1, State::Done), piece(2, 1, State::Running), piece(3, 2, State::Pending)],
        };
        assert_eq!(close_owed(&rec), None, "piece 2 still runs");
        rec.pieces[1].state = State::Dropped;
        assert_eq!(close_owed(&rec), Some(1));
        rec.closed.push(1);
        let mut t = row("");
        write(&mut t, &rec);
        assert!(t.body.contains("closed: 1\n"), "{}", t.body);
        assert_eq!(record(&t), rec, "it reads back as written");
        assert_eq!(close_owed(&rec), None, "batch 2 has its piece to run");
        assert!(!finished(&rec));
        rec.pieces[2].state = State::Done;
        assert_eq!(close_owed(&rec), Some(2));
        assert!(!finished(&rec), "not until batch 2 is closed");
        rec.closed.push(2);
        assert!(finished(&rec));
        let before = Record { cap: 2, closed: vec![1], pieces: vec![piece(1, 1, State::Done), piece(2, 2, State::Pending)] };
        let (moved, _) = insert_batch_after(&before, 1, new("fix", At::Next), Status::Doing).unwrap();
        assert_eq!(moved.closed, [1], "a batch closed before the insert keeps its number");
    }

    #[test]
    fn a_batch_is_inserted_right_after_one_and_the_later_batches_move_up_unless_one_has_opened() {
        let rec = put(&put(&put(&Record::default(), new("a", At::Next)), new("b", At::Next)), new("c", At::Next));
        let rec = with_state(rec, 1, State::Done);
        let (got, n) = insert_batch_after(&rec, 1, new("merge batch 1", At::Next), TODO).unwrap();
        assert_eq!(n, 4);
        let batches: Vec<(usize, usize)> = got.pieces.iter().map(|p| (p.n, p.batch)).collect();
        assert_eq!(batches, [(1, 1), (2, 3), (3, 4), (4, 2)]);
        assert!(check(&got).is_ok());

        // Batch 2 running: nothing is inserted before it, and the record is as it was.
        let open = with_state(rec.clone(), 2, State::Running);
        let err = insert_batch_after(&open, 1, new("merge batch 1", At::Next), TODO).unwrap_err();
        assert!(err.contains("batch 2 has opened"), "{err}");
        assert!(insert_batch_after(&rec, 9, new("x", At::Next), TODO).is_err());
        assert!(insert_batch_after(&rec, 1, new("x", At::Next), Status::Done).is_err());
    }

    #[test]
    fn an_entry_reads_back_as_it_was_written() {
        let mut rec = Record { cap: 2, ..Record::default() };
        let mut p = Piece::pending(1, "contract types", 1);
        p.state = State::Done;
        p.pane = Some("cpd-31".into());
        p.tree = Some("m-1-1".into());
        p.ending = Ending::Ended;
        p.overview = "Types and a fixture each.\n\nSecond paragraph.\n- not an entry\n  overview: not a key".into();
        p.done_when = "each module's test green".into();
        p.checks = vec!["tsc (2m): cd web && npx tsc --noEmit".into(), "contract (3m): cd web && npx vitest run".into()];
        p.checked = Some("green at 52c768d".into());
        p.note = "Done: 52c768d.\nLeft: nothing".into();
        p.caption = Some("types and fixtures read back".into());
        rec.pieces.push(p);
        let mut q = Piece::pending(2, "rail places", 2);
        q.policy = Policy::parse("claude opus high").unwrap();
        q.why = Some("decides between two layouts".into());
        q.by = Some(1);
        q.overview = "pick".into();
        q.done_when = "picked".into();
        rec.pieces.push(q);
        let mut r = Piece::pending(3, "old", 2);
        r.state = State::Promoted;
        r.promoted = Some("m-9".into());
        rec.pieces.push(r);
        let mut t = row("");
        write(&mut t, &rec);
        assert_eq!(record(&t), rec);
        assert!(t.body.contains("## Pieces\ncap: 2\n- 1 · contract types · batch 1 · done · pane cpd-31 · tree m-1-1 · ended\n"), "{}", t.body);
        assert!(t.body.contains("- 2 · rail places · batch 2 · claude opus high · pending · by 1\n"), "{}", t.body);
        let again = record(&t);
        write(&mut t, &again);
        assert_eq!(record(&t), rec, "and again");
    }

    #[test]
    fn a_row_with_no_pieces_reads_as_an_empty_record_with_the_default_cap() {
        let rec = record(&row(""));
        assert_eq!((rec.cap, rec.pieces.len()), (3, 0));
        assert!(!staged(&row("")));
    }

    #[test]
    fn numbers_are_never_reused_after_a_drop() {
        let rec = put(&Record::default(), new("one", At::Next));
        let rec = put(&rec, new("two", At::Batch(1)));
        let rec = put(&rec, new("three", At::Batch(1)));
        let rec = drop_piece(&rec, 3, "not needed", &Actor::Person, TODO).unwrap();
        let rec = put(&rec, new("four", At::Batch(1)));
        let ns: Vec<usize> = rec.pieces.iter().map(|p| p.n).collect();
        assert_eq!(ns, vec![1, 2, 3, 4]);
        assert_eq!(rec.pieces[2].state, State::Dropped);
        assert_eq!(rec.pieces[2].why.as_deref(), Some("not needed"));
        let rec = drop_piece(&rec, 4, "no", &Actor::Person, TODO).unwrap();
        let rec = put(&rec, new("five", At::Batch(1)));
        assert_eq!(rec.pieces.last().unwrap().n, 5, "the highest number was dropped and is still not reused");
    }

    #[test]
    fn a_title_is_capped_at_eighty_and_is_one_line() {
        let ok = "t".repeat(80);
        assert!(add(&Record::default(), new(&ok, At::Next), &Actor::Person, TODO).is_ok());
        let e = add(&Record::default(), new(&"t".repeat(81), At::Next), &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("title") && e.contains("81") && e.contains("80"), "{e}");
        let e = add(&Record::default(), new("a · b", At::Next), &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("title"), "{e}");
    }

    #[test]
    fn an_overview_is_capped_at_three_thousand_and_never_truncated() {
        let mut n = new("x", At::Next);
        n.overview = "o".repeat(3_000);
        assert!(add(&Record::default(), n.clone(), &Actor::Person, TODO).is_ok());
        n.overview = "o".repeat(3_001);
        let e = add(&Record::default(), n, &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("overview") && e.contains("3001") && e.contains("3000"), "{e}");
    }

    #[test]
    fn a_done_when_is_capped_at_six_hundred() {
        let mut n = new("x", At::Next);
        n.done_when = "d".repeat(600);
        assert!(add(&Record::default(), n.clone(), &Actor::Person, TODO).is_ok());
        n.done_when = "d".repeat(601);
        let e = add(&Record::default(), n, &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("done-when") && e.contains("601") && e.contains("600"), "{e}");
    }

    #[test]
    fn a_note_is_capped_at_fifteen_hundred() {
        let rec = put(&Record::default(), new("x", At::Next));
        let mut ok = rec.clone();
        ok.pieces[0].note = "n".repeat(1_500);
        assert!(check(&ok).is_ok());
        let mut over = rec;
        over.pieces[0].note = "n".repeat(1_501);
        let e = check(&over).unwrap_err();
        assert!(e.contains("note") && e.contains("1501") && e.contains("1500"), "{e}");
    }

    #[test]
    fn a_twenty_first_piece_is_refused_and_a_twentieth_is_not_dropped_ones_counted() {
        let mut rec = Record::default();
        for i in 0..20 {
            rec = put(&rec, new(&format!("p{i}"), At::Batch(1)));
        }
        let e = add(&rec, new("one more", At::Batch(1)), &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("21") && e.contains("20"), "{e}");
        let rec = drop_piece(&rec, 20, "no", &Actor::Person, TODO).unwrap();
        let rec = with_state(rec, 19, State::Promoted);
        let e = add(&rec, new("still more", At::Next), &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("dropped, promoted and rework counted"), "a dropped and a promoted piece still count: {e}");
    }

    #[test]
    fn a_check_line_that_is_not_a_check_is_refused() {
        let mut n = new("x", At::Next);
        n.checks = vec!["tsc (2m): npx tsc --noEmit".into(), "pieces: cargo test pieces::".into()];
        let rec = add(&Record::default(), n, &Actor::Person, TODO).unwrap();
        assert_eq!(rec.pieces[0].checks.len(), 2);
        let mut bad = rec.clone();
        bad.pieces[0].checks.push("   ".into());
        bad.pieces[0].checks.push("label (never): cmd".into());
        let e = check(&bad).unwrap_err();
        assert!(e.contains("not a check"), "{e}");
    }

    #[test]
    fn a_piece_with_no_policy_is_sonnet_medium() {
        let mut t = row("");
        let rec = put(&Record::default(), new("x", At::Next));
        assert!(crate::legs::is_default(&rec.pieces[0].policy));
        write(&mut t, &rec);
        let back = record(&t);
        assert_eq!(crate::legs::policy_line(&back.pieces[0].policy), crate::legs::DEFAULT_AGENT);
        assert!(!t.body.contains("claude sonnet medium"), "the default is not written: {}", t.body);
    }

    #[test]
    fn another_tier_needs_a_why_and_a_why_on_the_default_is_refused() {
        let mut n = new("x", At::Next);
        n.agent = Some("claude opus high".into());
        let e = add(&Record::default(), n.clone(), &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("gives no reason"), "{e}");
        n.why = Some("decides between two layouts".into());
        let rec = add(&Record::default(), n, &Actor::Person, TODO).unwrap();
        assert_eq!(crate::legs::policy_line(&rec.pieces[0].policy), "claude opus high");
        // And the other way: the default tier with a why.
        let mut d = new("y", At::Next);
        d.why = Some("a reason for nothing".into());
        let e = add(&Record::default(), d.clone(), &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("reason for nothing"), "{e}");
        d.agent = Some(crate::legs::DEFAULT_AGENT.into());
        assert!(add(&Record::default(), d, &Actor::Person, TODO).is_err(), "naming the default is still the default");
        // `claude sonnet` is another policy than the default.
        let mut s = new("z", At::Next);
        s.agent = Some("claude sonnet".into());
        assert!(add(&Record::default(), s, &Actor::Person, TODO).unwrap_err().contains("gives no reason"));
    }

    #[test]
    fn a_dropped_piece_keeps_its_reason_and_the_tier_rule_is_not_asked_of_it() {
        let mut n = new("x", At::Next);
        n.agent = Some("claude opus high".into());
        n.why = Some("hard".into());
        let rec = add(&Record::default(), n, &Actor::Person, TODO).unwrap();
        let rec = drop_piece(&rec, 1, "the plan changed", &Actor::Person, TODO).unwrap();
        assert_eq!(rec.pieces[0].why.as_deref(), Some("the plan changed"));
        let mut d = new("d", At::Next);
        d.why = Some("x".into());
        let rec = put(&Record::default(), new("d", At::Next));
        let rec = drop_piece(&rec, 1, "no", &Actor::Person, TODO).unwrap();
        assert!(check(&rec).is_ok(), "a default dropped piece carries a why: line");
        assert!(drop_piece(&rec, 1, "again", &Actor::Person, TODO).is_err(), "only a pending piece is dropped");
    }

    #[test]
    fn a_piece_goes_to_a_batch_that_has_not_opened_or_to_next_and_never_to_one_that_has() {
        let rec = put(&Record::default(), new("a", At::Next));
        let rec = put(&rec, new("b", At::Batch(1)));
        let rec = put(&rec, new("c", At::Batch(2)));
        assert_eq!(last_batch(&rec), 2);
        assert_eq!(put(&rec, new("d", At::Next)).pieces.last().unwrap().batch, 3);
        // A gap is refused.
        assert!(add(&rec, new("e", At::Batch(4)), &Actor::Person, TODO).unwrap_err().contains("gap"));
        // Batch 1 opens once one of its pieces is not pending.
        let opened_by = |s: State| {
            let rec = with_state(rec.clone(), 1, s);
            add(&rec, new("late", At::Batch(1)), &Actor::Person, TODO)
        };
        for s in [State::Running, State::Done, State::Blocked, State::Promoted] {
            let e = opened_by(s).unwrap_err();
            assert!(e.contains("batch 1 has opened"), "{s:?}: {e}");
        }
        assert!(opened_by(State::Dropped).is_ok(), "a dropped piece never ran, so it opened nothing");
        // Batch 2 is still open, and `next` is still 3.
        let started = with_state(rec.clone(), 1, State::Running);
        assert!(add(&started, new("in two", At::Batch(2)), &Actor::Person, TODO).is_ok());
        assert_eq!(put(&started, new("n", At::Next)).pieces.last().unwrap().batch, 3);
        assert!(add(&rec, new("zero", At::Batch(0)), &Actor::Person, TODO).is_err());
    }

    // ---- who may write ----

    fn two_batches() -> Record {
        let rec = put(&Record::default(), new("a", At::Next));
        let rec = put(&rec, new("b", At::Next));
        with_state(rec, 1, State::Running)
    }

    #[test]
    fn a_person_the_primary_a_governor_and_the_seat_holding_the_row_may_write_a_todo_or_doing_row() {
        for actor in [Actor::Person, Actor::Primary, Actor::Governor, Actor::Claimant] {
            for status in [Status::Todo, Status::Doing] {
                let rec = add(&two_batches(), new("c", At::Batch(2)), &actor, status).unwrap_or_else(|e| panic!("{actor:?} {status:?}: {e}"));
                assert_eq!(rec.pieces.len(), 3);
                let rec = drop_piece(&rec, 2, "no", &actor, status).unwrap();
                assert_eq!(rec.pieces[1].state, State::Dropped);
                assert!(set_cap(&rec, 5, &actor, status).is_ok());
            }
            for status in [Status::Inbox, Status::Blocked, Status::Parked, Status::Review, Status::Done] {
                let e = add(&two_batches(), new("c", At::Batch(2)), &actor, status).unwrap_err();
                assert!(e.contains("todo or doing") && e.contains(status.as_str()), "{actor:?} {status:?}: {e}");
                assert!(drop_piece(&two_batches(), 2, "no", &actor, status).is_err());
                assert!(set_cap(&two_batches(), 5, &actor, status).is_err());
            }
        }
    }

    #[test]
    fn any_other_seat_may_not_write() {
        let e = add(&two_batches(), new("c", At::Batch(2)), &Actor::Other, TODO).unwrap_err();
        assert!(e.contains("may write its pieces"), "{e}");
        assert!(drop_piece(&two_batches(), 2, "no", &Actor::Other, TODO).is_err());
        assert!(set_cap(&two_batches(), 5, &Actor::Other, TODO).is_err());
    }

    #[test]
    fn a_seated_piece_adds_only_to_later_batches_and_changes_only_what_it_added() {
        let me = Actor::Piece(1);
        let rec = two_batches();
        // Its own batch, and an earlier one, are refused; a later one is not.
        assert!(add(&rec, new("same", At::Batch(1)), &me, TODO).is_err());
        let e = add(&rec, new("same", At::Batch(2)), &me, TODO);
        assert!(e.is_ok(), "{e:?}");
        let rec = e.unwrap();
        let added = rec.pieces.last().unwrap();
        assert_eq!(added.by, Some(1), "it is recorded who added it");
        // `next` is later than its own batch.
        let rec = add(&rec, new("after", At::Next), &me, TODO).unwrap();
        // It drops what it added, and not what it did not.
        assert!(drop_piece(&rec, 3, "wrong", &me, TODO).is_ok());
        let e = drop_piece(&rec, 2, "wrong", &me, TODO).unwrap_err();
        assert!(e.contains("only the pieces it added"), "{e}");
        // Nor the cap.
        assert!(set_cap(&rec, 5, &me, TODO).unwrap_err().contains("cap"));
        // A piece that is not running is no seat.
        let idle = with_state(rec.clone(), 1, State::Done);
        assert!(add(&idle, new("late", At::Next), &me, TODO).unwrap_err().contains("not running"));
        // Another piece's number it is not.
        assert!(add(&rec, new("x", At::Next), &Actor::Piece(9), TODO).is_err());
    }

    #[test]
    fn nobody_changes_a_running_or_done_piece() {
        let rec = two_batches();
        let e = drop_piece(&rec, 1, "no", &Actor::Person, TODO).unwrap_err();
        assert!(e.contains("only a pending piece"), "{e}");
        assert!(drop_piece(&rec, 7, "no", &Actor::Person, TODO).unwrap_err().contains("no piece 7"));
        assert!(drop_piece(&rec, 2, "  ", &Actor::Person, TODO).is_err());
    }

    #[test]
    fn a_write_is_judged_by_what_it_changes() {
        // A hand edit left a note over its cap; an unrelated add still goes in.
        let mut rec = put(&Record::default(), new("a", At::Next));
        rec.pieces[0].note = "n".repeat(1_600);
        assert!(check(&rec).is_err());
        assert!(add(&rec, new("b", At::Batch(1)), &Actor::Person, TODO).is_ok());
    }

    #[test]
    fn the_cap_is_one_to_twenty() {
        let rec = Record::default();
        assert_eq!(set_cap(&rec, 5, &Actor::Person, TODO).unwrap().cap, 5);
        assert!(set_cap(&rec, 0, &Actor::Person, TODO).is_err());
        assert!(set_cap(&rec, 21, &Actor::Person, TODO).is_err());
        let mut t = row("");
        write(&mut t, &Record { cap: 5, closed: Vec::new(), pieces: Vec::new() });
        assert_eq!(record(&t).cap, 5, "a cap with no pieces is kept");
    }

    #[test]
    fn show_renders_the_table_folded_and_the_pieces_whole_and_the_json_carries_them() {
        let mut rec = Record::default();
        let mut p = put(&rec, new("contract types", At::Next));
        p.pieces[0].state = State::Done;
        p.pieces[0].pane = Some("cpd-31".into());
        p.pieces[0].caption = Some("types and fixtures read back".into());
        p.pieces[0].tree = Some("m-1-1".into());
        rec = put(&p, new("board", At::Batch(2)));
        let t = {
            let mut t = row("");
            write(&mut t, &rec);
            t
        };
        let folded = section_for_show(&t, false).unwrap();
        assert_eq!(
            folded,
            "cap: 3\n- 1 · contract types · batch 1 · done · types and fixtures read back\n- 2 · board · batch 2 · pending"
        );
        let whole = section_for_show(&t, true).unwrap();
        assert!(whole.contains("  overview: do contract types") && whole.contains("  done-when: board is done"), "{whole}");
        assert_eq!(section_for_show(&row(""), false), None);
        assert_eq!(
            json(&t),
            json!([
                { "n": 1, "id": "m-1-1", "title": "contract types", "batch": 1, "state": "done", "seat": "cpd-31", "tree": "m-1-1", "caption": "types and fixtures read back" },
                { "n": 2, "id": "m-1-2", "title": "board", "batch": 2, "state": "pending", "seat": null, "tree": null, "caption": null },
            ])
        );
        assert_eq!(json(&row("")), json!([]));
    }

    /// Three done pieces in two batches; piece 2 on opus with a reason.
    fn three_done() -> Record {
        let mut rec = Record::default();
        for (title, at) in [("types", At::Next), ("board", At::Next), ("rail", At::Batch(2))] {
            let mut n = new(title, at);
            n.checks = vec![format!("{title} (1m): true {title}")];
            if title == "board" {
                n.agent = Some("claude opus high".into());
                n.why = Some("the layout is subtle".into());
            }
            rec = put(&rec, n);
        }
        for p in rec.pieces.iter_mut() {
            p.state = State::Done;
            p.caption = Some(format!("{} read back", p.title));
            p.checked = Some("green at abc".into());
        }
        rec
    }

    #[test]
    fn a_done_pieces_check_lines_are_steps_labelled_by_number_and_a_pending_ones_are_not() {
        let mut rec = three_done();
        rec.pieces[2].state = State::Pending;
        let mut t = row("");
        write(&mut t, &rec);
        let names: Vec<String> = check_steps(&t).into_iter().map(|(_, s)| s.name).collect();
        assert_eq!(names, ["1.types", "2.board"]);
    }

    #[test]
    fn a_blocking_verdict_naming_a_piece_adds_one_rework_piece_in_a_new_batch_on_its_tier() {
        let rec = three_done();
        let after = rework_by(&rec, "m-1", "m-1-2 does not hold: the rail overlaps.", "Blocked:\n\n", Status::Doing).unwrap().unwrap();
        assert_eq!(after.pieces.len(), 4, "exactly one");
        let r = after.pieces.last().unwrap();
        assert_eq!((r.title.as_str(), r.batch, r.state), ("rework of 2", 3, State::Pending));
        assert_eq!(r.policy, rec.pieces[1].policy);
        assert_eq!(r.why.as_deref(), Some("rework of 2: the layout is subtle"));
        assert!(r.overview.starts_with("Blocked:") && r.overview.contains("the rail overlaps"), "{}", r.overview);
        assert_eq!(r.done_when, rec.pieces[1].done_when);
        assert_eq!(r.checks, rec.pieces[1].checks);
        // M-1-20 is not m-1-2.
        let after = rework_by(&rec, "m-1", "m-1-20 is wrong", "", Status::Doing).unwrap().unwrap();
        assert_eq!(after.pieces.last().unwrap().title, "rework");
        // A second verdict recorded is not a second send-back.
        assert_eq!(rework_by(&after, "m-1", "m-1-2 is wrong", "", Status::Doing).unwrap(), None);
    }

    #[test]
    fn a_verdict_naming_no_piece_adds_one_over_the_row_and_two_named_add_two() {
        let rec = three_done();
        let after = rework_by(&rec, "m-1", "the whole does not add up", "", Status::Doing).unwrap().unwrap();
        assert_eq!(after.pieces.len(), 4);
        let r = after.pieces.last().unwrap();
        assert_eq!((r.title.as_str(), r.batch), ("rework", 3));
        assert!(crate::legs::is_default(&r.policy) && r.why.is_none());
        let after = rework_by(&rec, "m-1", "m-1-1 and m-1-3 fail", "", Status::Doing).unwrap().unwrap();
        let new: Vec<_> = after.pieces[3..].iter().map(|p| (p.title.as_str(), p.batch)).collect();
        assert_eq!(new, [("rework of 1", 3), ("rework of 3", 3)], "one batch for both");
    }

    #[test]
    fn landing_folds_each_piece_into_a_log_line_and_the_section_goes() {
        let mut rec = three_done();
        rec.pieces[0].done_when = "x".repeat(300);
        let mut t = row("");
        write(&mut t, &rec);
        let lines = fold_at_land(&mut t, &[]);
        assert_eq!(lines.len(), 3);
        assert!(!staged(&t) && t.section(SECTION).is_none(), "the section is gone");
        let log = t.section("Log").unwrap();
        assert_eq!(log.matches("piece ").count(), 3, "{log}");
        assert!(lines[0].starts_with("piece 1 · types · done · types read back · checks green · done-when: xxx"), "{}", lines[0]);
        assert!(lines[0].contains(&"x".repeat(150)) && !lines[0].contains(&"x".repeat(250)) && lines[0].ends_with('…'), "{}", lines[0]);
        assert_eq!(folded_line(&t, 2).as_deref(), Some(lines[1].as_str()));
        assert_eq!(folded_line(&t, 4), None);
    }

    #[test]
    fn a_folded_piece_line_shows_what_it_ran_as_and_its_turns() {
        let mut t = row("");
        write(&mut t, &three_done());
        let ran = [
            PieceRan { n: 1, ran: "sonnet/high".into(), turns: 12 },
            PieceRan { n: 2, ran: String::new(), turns: 3 },
        ];
        let lines = fold_at_land(&mut t, &ran);
        assert!(lines[0].contains(" · ran sonnet/high 12 turns · checks green"), "{}", lines[0]);
        assert!(lines[1].contains(" · ran ? 3 turns"), "{}", lines[1]);
        assert!(!lines[2].contains(" ran "), "{}", lines[2]);
    }

    #[test]
    fn a_dropped_piece_folds_with_its_reason_and_a_promoted_one_with_its_row() {
        let mut rec = three_done();
        rec.pieces[0].state = State::Dropped;
        rec.pieces[0].why = Some("not needed".into());
        rec.pieces[1].state = State::Promoted;
        rec.pieces[1].promoted = Some("m-9".into());
        let mut t = row("");
        write(&mut t, &rec);
        let lines = fold_at_land(&mut t, &[]);
        assert_eq!(lines[0], "piece 1 · types · dropped: not needed");
        assert_eq!(lines[1], "piece 2 · board · promoted to m-9");
    }
}
