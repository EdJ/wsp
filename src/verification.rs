//! A verification is a pass on the member's own row: `wsp-188`.
//!
//! Every verification used to be filed as a row of its own — a child of the
//! member, tagged `verify`, whose overview was the verifier's work order. Ed,
//! 2026-10-06: that doubled the task count for no reason. wsp-148 alone had
//! five Verify rows and compound-289 had four, and every one of them was read
//! for exactly one fact: what the newest verdict on its parent said.
//!
//! So the verdict lives where it is read. A member carries a
//! `## Verification` section, one entry per pass, oldest first:
//!
//! ```text
//! - 2026-10-06T10:00:00Z holds · read 55c4839abcde · pane cpd-12 · on opus/high · ended
//!   what was checked, what held, what did not — indented under the entry
//! ```
//!
//! The word after the instant is the state ([`State`]); the clauses after it
//! are keyed by their first word, so a clause nobody wrote is simply absent
//! and a reader of an old entry never mistakes one clause for another.
//!
//! # The entry is the record, and the only one
//!
//! A verifier has no row and holds no claim. The `running` entry is written
//! under the store lock *before* its agent is spawned — the idempotence key the
//! verifier row used to be — and `wsp spawn --verify` writes the seat onto it.
//! Everything that asks about a verifier asks the entry: the barrier's gate
//! ([`verified`]), the tick that ends a verifier once ([`Ending`]), `wsp wip`
//! naming the seat against its member ([`seats`]), and the brief that seat is
//! given. A binding or a claim would have been a second record of the same
//! fact, and the store has paid for that shape before: two readers of one
//! record disagreeing is how wsp-148 got five rows.
//!
//! # Bookkeeping does not touch the member
//!
//! Opening an entry, naming its seat and marking it ended are written without
//! moving `updated`. A member with no landing to key on is re-verified when it
//! is touched ([`crate::cycle`]'s `moved_since`), and a re-verify bought by the
//! verifier's own bookkeeping would be one bought by nobody. A verdict that
//! blocks moves the member to `doing`, which is a real change and does touch it.
//!
//! # The rows that came before
//!
//! A Verify row wsp filed before this is read as a pass ([`legacy`]) until
//! [`migrate`] has moved it onto its parent and archived it — so a binary that
//! is installed before the migration runs reads the same verdicts the rows
//! held, and starts nothing because a row it could not see looked like a
//! member never verified.

use std::collections::BTreeMap;

use crate::model::{Status, Task};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// The heading the passes sit under on the member's row.
pub(crate) const SECTION: &str = "Verification";

/// Where a pass is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Opened, and no verdict yet. Its seat may not exist yet either.
    Running,
    Holds,
    Blocks,
    /// A newer landing came in while this pass was reading. It was ended and
    /// its verdict, if one arrives, is refused.
    Superseded,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Holds => "holds",
            State::Blocks => "blocks",
            State::Superseded => "superseded",
        }
    }

    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "running" => State::Running,
            "holds" => State::Holds,
            "blocks" => State::Blocks,
            "superseded" => State::Superseded,
            _ => return None,
        })
    }
}

/// Whether the pass's seat has been ended, and how that went.
///
/// **Failed is terminal.** wsp-176 item 1: a despawn that failed was retried
/// every tick, each one a line in `cycle.log` and none of them a different
/// answer. It is tried once, the failure is written here and told, and a
/// person ends the seat if it is still standing.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) enum Ending {
    #[default]
    Standing,
    Ended,
    Failed(String),
}

/// One verification of a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pass {
    /// When it was opened, as `util::now_iso` writes it.
    pub at: String,
    pub state: State,
    /// The commit it was asked to read: the member's landing when the pass was
    /// opened. `None` for work with no repository behind it.
    pub read: Option<String>,
    /// The seat its agent was started in.
    pub pane: Option<String>,
    /// What it ran on: the kind, or the kind and its tier.
    pub on: Option<String>,
    /// The Verify row this pass was migrated from, or is being read from.
    pub was: Option<String>,
    pub ending: Ending,
    /// When the verdict was recorded, as `util::now_iso` writes it. `wsp-193`
    /// measures "somebody typed to the member since" from here: a turn after
    /// `at` and before the verdict is the member answering its own verifier.
    pub decided: Option<String>,
    /// The verdict, in the verifier's words.
    pub text: String,
}

impl Pass {
    /// A pass opened now, on the commit the member's work is on.
    pub(crate) fn opened(read: Option<String>, on: Option<String>) -> Pass {
        Pass {
            at: util::now_iso(),
            state: State::Running,
            read: read.map(|s| short(&s)),
            pane: None,
            on,
            was: None,
            ending: Ending::Standing,
            decided: None,
            text: String::new(),
        }
    }

    /// Whether this pass still has an agent wsp has not ended.
    pub(crate) fn standing(&self) -> bool {
        self.pane.is_some() && self.ending == Ending::Standing
    }

    fn line(&self) -> String {
        let mut out = format!("- {} {}", self.at, self.state.as_str());
        let mut clause = |k: &str, v: &Option<String>| {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                out.push_str(&format!(" · {k} {v}"));
            }
        };
        clause("read", &self.read);
        clause("pane", &self.pane);
        clause("on", &self.on);
        clause("was", &self.was);
        clause("decided", &self.decided);
        match &self.ending {
            Ending::Standing => {}
            Ending::Ended => out.push_str(" · ended"),
            // One line: a reason from stderr can run to several, and a line
            // break inside a clause would end the entry.
            Ending::Failed(why) => out.push_str(&format!(" · end failed: {}", crate::cmd_task::fold(why))),
        }
        for l in self.text.trim().lines() {
            out.push_str("\n  ");
            out.push_str(l.trim_end());
        }
        out
    }

    fn parse(line: &str) -> Option<Pass> {
        let rest = line.strip_prefix("- ")?;
        let (at, rest) = rest.split_once(' ')?;
        let mut clauses = rest.split(" · ");
        let state = State::parse(clauses.next()?.trim())?;
        let mut p = Pass {
            at: at.to_string(),
            state,
            read: None,
            pane: None,
            on: None,
            was: None,
            ending: Ending::Standing,
            decided: None,
            text: String::new(),
        };
        for c in clauses {
            let c = c.trim();
            if c == "ended" {
                p.ending = Ending::Ended;
            } else if let Some(why) = c.strip_prefix("end failed:") {
                p.ending = Ending::Failed(why.trim().to_string());
            } else if let Some((k, v)) = c.split_once(' ') {
                let v = Some(v.trim().to_string());
                match k {
                    "read" => p.read = v,
                    "pane" => p.pane = v,
                    "on" => p.on = v,
                    "was" => p.was = v,
                    "decided" => p.decided = v,
                    _ => {}
                }
            }
        }
        Some(p)
    }
}

/// A commit as an entry writes it: twelve characters, which is unambiguous in
/// any repository wsp works in and short enough to read in `wsp show`.
fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// Whether two spellings of a commit name the same one. An entry writes twelve
/// characters and a landing records forty, so the test is a prefix either way
/// round — never shorter than git's own seven.
pub(crate) fn same_commit(a: &str, b: &str) -> bool {
    let n = a.len().min(b.len());
    n >= 7 && a[..n] == b[..n]
}

/// The passes written on a member's own row, oldest first.
pub(crate) fn passes(t: &Task) -> Vec<Pass> {
    let Some(text) = t.section(SECTION) else { return Vec::new() };
    let mut out: Vec<Pass> = Vec::new();
    for line in text.lines() {
        if let Some(p) = Pass::parse(line) {
            out.push(p);
        } else if let (Some(last), Some(more)) = (out.last_mut(), line.strip_prefix("  ")) {
            if !last.text.is_empty() {
                last.text.push('\n');
            }
            last.text.push_str(more);
        }
    }
    out
}

/// Write the passes back under the heading, in time order.
///
/// Does not touch the member — see the module docs for why bookkeeping must
/// not move `updated`.
pub(crate) fn write(t: &mut Task, passes: &[Pass]) {
    let mut sorted = passes.to_vec();
    sorted.sort_by(|a, b| a.at.cmp(&b.at));
    let text = sorted.iter().map(Pass::line).collect::<Vec<_>>().join("\n");
    crate::model::set_section_in(&mut t.body, SECTION, &text);
}

/// The tag a Verify row carries. Kept for [`legacy`] and [`migrate`]: nothing
/// files one any more.
pub(crate) const VERIFY_TAG: &str = "verify";

/// The phrase every work order wsp wrote onto a Verify row carries, and the
/// thing that tells a row wsp filed from one a person wrote and tagged.
const FILED_BY_WSP: &str = "wsp spawned you when";

/// Whether a row is a Verify row wsp filed: the tag, a parent, and wsp's own
/// work order in the overview. Not the title — `wsp-137` is titled
/// "Re-verify" and is somebody's own review, and must be left alone.
pub(crate) fn filed_by_wsp(t: &Task) -> bool {
    t.tags.iter().any(|g| g == VERIFY_TAG)
        && t.parent.is_some()
        && t.section("Overview").is_some_and(|o| o.contains(FILED_BY_WSP))
}

/// A Verify row read as the pass it recorded.
pub(crate) fn legacy(row: &Task) -> Pass {
    let state = match row.status() {
        Status::Review | Status::Done => State::Holds,
        Status::Blocked => State::Blocks,
        _ => State::Running,
    };
    let attempt = crate::cmd_attempts::attempts_of(row).into_iter().last();
    let on = attempt.as_ref().and_then(|a| {
        let ran = crate::cmd_attempts::Row(a.clone()).tier();
        (!ran.is_empty()).then_some(ran)
    });
    Pass {
        at: row.created.clone(),
        state,
        read: crate::repair::landed(row).map(|s| short(&s)),
        pane: attempt.map(|a| a.pane).filter(|p| !p.is_empty()),
        on,
        was: Some(row.id.clone()),
        ending: Ending::Ended,
        // The row's last touch: for a row at its verdict, the verdict.
        decided: (state != State::Running).then(|| row.updated.clone()),
        text: verdict_of(row),
    }
}

/// The verdict a Verify row's own log recorded: its last `review:` or
/// `blocked:` line, which is what `wsp review <id> -` and `wsp block` write.
fn verdict_of(row: &Task) -> String {
    let Some(log) = row.section("Log") else { return String::new() };
    log.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("- ")?;
            let (_, said) = rest.split_once(' ')?;
            said.strip_prefix("review: ").or_else(|| said.strip_prefix("blocked: "))
        })
        .last()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Every pass on a member, oldest first: the entries on its row, and any Verify
/// row under it that has not been migrated yet.
pub(crate) fn all(tasks: &[Task], member: &Task) -> Vec<Pass> {
    let mut out = passes(member);
    out.extend(
        tasks
            .iter()
            .filter(|t| t.parent.as_deref() == Some(member.id.as_str()) && filed_by_wsp(t))
            .map(legacy),
    );
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.was.cmp(&b.was)));
    out
}

/// The newest pass on a member, by id.
pub(crate) fn latest(tasks: &[Task], member: &str) -> Option<Pass> {
    let m = tasks.iter().find(|t| t.id == member)?;
    all(tasks, m).pop()
}

/// Whether a member's newest pass holds — the predicate the barrier is gated on.
pub(crate) fn verified(tasks: &[Task], member: &str) -> bool {
    latest(tasks, member).is_some_and(|p| p.state == State::Holds)
}

/// The member's newest pass, when it blocked.
pub(crate) fn blocked(tasks: &[Task], member: &str) -> Option<Pass> {
    latest(tasks, member).filter(|p| p.state == State::Blocks)
}

/// Every seat a pass still stands in, to the member it is verifying — what
/// `wsp wip` and `wsp brief` name a verifier's seat by.
pub(crate) fn seats(tasks: &[Task]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for t in tasks.iter().filter(|t| t.body.contains("## Verification")) {
        for p in passes(t).into_iter().filter(Pass::standing) {
            if let Some(pane) = p.pane {
                out.insert(pane, t.id.clone());
            }
        }
    }
    out
}

/// What a verifier's seat is called, wherever a seat is named.
pub(crate) fn seat_label(member: &str) -> String {
    format!("{member} · verifying")
}

/// Write the seat onto the newest pass that has none. `wsp spawn --verify`'s
/// half of the record, written the moment the seat exists and before the agent
/// in it reads its brief.
pub(crate) fn name_seat(store: &Store, member: &str, seat: &str) -> bool {
    let wrote = store.locked(|| {
        let Some(mut t) = store.find_task(member) else { return false };
        let mut ps = passes(&t);
        let Some(p) = ps.iter_mut().rev().find(|p| p.state == State::Running && p.pane.is_none()) else {
            return false;
        };
        p.pane = Some(seat.to_string());
        write(&mut t, &ps);
        store.save_task(&t).is_ok()
    });
    if wrote {
        store.git_commit(&format!("wsp: {member} is verified in {seat}"));
    }
    wrote
}

/// `wsp verified <member> --holds|--blocks --from FILE` — a verifier's verdict,
/// recorded on the member it read.
///
/// **Onto the caller's own pass**: the newest `running` entry whose seat is
/// this pane, or the newest `running` entry at all for a caller with no seat.
/// A governor who answers a block by hand — the finding was wrong, the work
/// stands — has no pass open, and gets a new one in their own name.
///
/// **A superseded pass is refused**, because a newer landing has already
/// started a fresh verifier on the work this one was reading, and a verdict
/// about the old commit landing after it would read as the newest.
///
/// Holds leaves the member where it is. Blocks is `wsp reopen` with the verdict
/// as what is owed — the member goes back to `doing`, and its pane is told —
/// and the run tells its governor once, as the decision it is.
pub fn cmd_verified(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp verified <member> --holds|--blocks --from FILE   (or `-` for stdin)";
    let state = match (args.has("holds"), args.has("blocks")) {
        (true, false) => State::Holds,
        (false, true) => State::Blocks,
        _ => {
            eprintln!("usage: {USAGE}");
            eprintln!("       one of --holds or --blocks, and the verdict to go with it");
            return 2;
        }
    };
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let text = match crate::cmd_task::prose_kept(args, USAGE) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let member = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let me = crate::cmd_agent::my_pane();
    let recorded = record(store, &member.id, state, &text, me.as_deref());
    let pass = match recorded {
        Ok(p) => p,
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    };
    store.log_event(
        "task-verified",
        serde_json::json!({ "id": member.id, "verdict": state.as_str(), "read": pass.read, "pane": pass.pane }),
    );
    store.git_commit(&format!("wsp: verified {} {} — {}", member.id, state.as_str(), member.title));
    if state == State::Blocks {
        crate::cmd_task::deliver_reason(store, &member.id, text.trim());
    }
    crate::cycle::poke_task(store, &member.id, "verified");

    let p = Paint::new();
    let mark = match state {
        State::Holds => p.green("✓"),
        _ => p.red("■"),
    };
    let read = pass.read.as_deref().map(|r| format!(" at {r}")).unwrap_or_default();
    println!("{mark} {}  {}{read}", p.bold(&member.id), state.as_str());
    if state == State::Blocks {
        println!("  {}", p.dim("sent back to doing with your verdict as what is owed; its governor is told"));
    }
    0
}

/// The store half of [`verified`]: the pass written, and the member moved when
/// the verdict blocks. Separate so the choice of pass can be tested without a
/// process environment to read a pane out of.
pub(crate) fn record(store: &Store, member: &str, state: State, text: &str, me: Option<&str>) -> Result<Pass, String> {
    store.locked(|| {
        let Some(mut t) = store.find_task(member) else { return Err(format!("no task {member}")) };
        let mut ps = passes(&t);
        let mine = |p: &Pass| me.is_some() && p.pane.as_deref() == me;
        if ps.iter().any(|p| p.state == State::Superseded && mine(p)) && !ps.iter().any(|p| p.state == State::Running && mine(p)) {
            return Err(format!(
                "your pass on {member} was superseded by a newer landing, and a fresh verifier is reading that — nothing recorded"
            ));
        }
        let at = ps
            .iter()
            .rposition(|p| p.state == State::Running && mine(p))
            .or_else(|| me.is_none().then(|| ps.iter().rposition(|p| p.state == State::Running)).flatten());
        let at = match at {
            Some(i) => i,
            None => {
                let mut p = Pass::opened(crate::repair::landed(&t), None);
                p.pane = me.map(str::to_string);
                // Nothing to end: this pass was written by somebody already
                // standing somewhere, not by an agent wsp started for it.
                p.ending = Ending::Ended;
                ps.push(p);
                ps.len() - 1
            }
        };
        // One instant for the verdict and for the block's own move of the
        // member: a re-verify of work with no landing asks whether the member
        // moved *after* the verdict, and two clock reads a second apart would
        // count the block itself as the member moving.
        let now = util::now_iso();
        ps[at].state = state;
        ps[at].decided = Some(now.clone());
        ps[at].text = text.trim().to_string();
        let pass = ps[at].clone();
        write(&mut t, &ps);
        if state == State::Blocks {
            t.set_status(Status::Doing);
            t.updated = now;
            t.log(&format!(
                "{} the verifier blocked{}: {} — the whole verdict is under ## Verification",
                crate::cycle::SENT_BACK,
                pass.read.as_deref().map(|r| format!(" at {r}")).unwrap_or_default(),
                util::truncate(&crate::cmd_task::fold(text), 300)
            ));
        }
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok(pass)
    })
}

/// `wsp migrate --verify-rows [-n]` — every Verify row wsp filed, moved onto
/// its parent's `## Verification` and archived. A one-off: once the store has
/// none, it says so and does nothing.
///
/// **Identified by what wsp wrote, never by title** ([`filed_by_wsp`]). Rows a
/// person made to review something — `compound-291`, `wsp-137` — are listed
/// separately and left exactly as they are.
///
/// **A row still holding a claim is left** until its verifier finishes: moving
/// it would take the record out from under an agent that is about to write to
/// it.
///
/// **Old ids keep resolving.** The row is archived, not deleted, and `wsp show`
/// reads the archive — so a log or a handover that cites `wsp-174` still finds
/// it, and the entry on the parent names it with `was`.
pub(crate) fn migrate(store: &Store, args: &Args) -> i32 {
    let dry = args.has("n") || args.has("dry-run");
    let p = Paint::new();
    let plan = plan(store);
    for (parent, rows) in &plan.moves {
        println!("{}  {} pass(es) from {}", p.bold(parent), rows.len(), rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(" "));
    }
    if !plan.running.is_empty() {
        println!("{}", p.yellow(&format!("left running: {} — still holding a claim", plan.running.join(" "))));
    }
    if !plan.orphans.is_empty() {
        println!("{}", p.yellow(&format!("left alone: {} — the parent is not a live row", plan.orphans.join(" "))));
    }
    if !plan.by_hand.is_empty() {
        println!("{}", p.dim(&format!("made by hand, left alone: {}", plan.by_hand.join(" "))));
    }
    let n: usize = plan.moves.values().map(Vec::len).sum();
    println!("\n{n} Verify row(s) onto {} member(s)", plan.moves.len());
    if n == 0 {
        return 0;
    }
    if dry {
        println!("{}", p.dim("nothing written — drop -n to apply"));
        return 0;
    }
    match apply(store, &plan) {
        Ok(moved) => {
            store.git_commit(&format!("wsp: {moved} Verify row(s) moved onto their members' ## Verification"));
            println!("moved {moved}");
            0
        }
        Err(e) => {
            eprintln!("wsp: {e}");
            1
        }
    }
}

/// What [`migrate`] would do.
pub(crate) struct Plan {
    /// parent id -> the rows whose passes go onto it, oldest first.
    pub moves: BTreeMap<String, Vec<Task>>,
    pub running: Vec<String>,
    pub orphans: Vec<String>,
    pub by_hand: Vec<String>,
}

pub(crate) fn plan(store: &Store) -> Plan {
    let tasks = store.tasks();
    let claims = store.claims();
    let mut plan = Plan { moves: BTreeMap::new(), running: Vec::new(), orphans: Vec::new(), by_hand: Vec::new() };
    for t in &tasks {
        if !filed_by_wsp(t) {
            if made_by_hand(t) {
                plan.by_hand.push(t.id.clone());
            }
            continue;
        }
        if claims.contains_key(&t.id) {
            plan.running.push(t.id.clone());
            continue;
        }
        let parent = t.parent.clone().unwrap_or_default();
        if !tasks.iter().any(|x| x.id == parent) {
            plan.orphans.push(t.id.clone());
            continue;
        }
        plan.moves.entry(parent).or_default().push(t.clone());
    }
    for rows in plan.moves.values_mut() {
        rows.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    }
    plan
}

/// A row somebody wrote to review or verify something, which the migration
/// names so a reader can see it was considered and left. The title's opening
/// words and not a word anywhere in it: a row *about* verification — this one,
/// `wsp-193` — is not a review of anything.
fn made_by_hand(t: &Task) -> bool {
    let title = t.title.to_ascii_lowercase();
    let barrier = title.starts_with("barrier") && (title.contains("review") || title.contains("re-check"));
    t.tags.iter().any(|g| g == VERIFY_TAG) || title.starts_with("verify ") || title.starts_with("re-verify ") || barrier
}

fn apply(store: &Store, plan: &Plan) -> Result<usize, String> {
    let mut moved = 0;
    for (parent, rows) in &plan.moves {
        let wrote = store.locked(|| -> Result<(), String> {
            let mut m = store.find_task(parent).ok_or_else(|| format!("{parent} went away"))?;
            let mut ps = passes(&m);
            for row in rows {
                if ps.iter().any(|p| p.was.as_deref() == Some(row.id.as_str())) {
                    continue;
                }
                ps.push(legacy(row));
            }
            write(&mut m, &ps);
            store.save_task(&m).map_err(|e| format!("{parent}: {e}"))
        });
        wrote?;
        for row in rows {
            store.archive_task(row).map_err(|e| format!("{}: {e}", row.id))?;
            moved += 1;
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("verification-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        (env, store)
    }

    /// An entry is read back as it was written — every clause, the ending, and
    /// a verdict that keeps its lines — and an entry with a clause nobody wrote
    /// reads that clause as absent rather than as its neighbour.
    #[test]
    fn a_pass_reads_back_as_it_was_written_whatever_clauses_it_carries() {
        let mut t = Task::new("a member", "m-1");
        t.body = "## Overview\nthe work\n\n## Log\n- 2026-10-06 claimed\n".into();
        let full = Pass {
            at: "2026-10-06T10:00:00Z".into(),
            state: State::Blocks,
            read: Some("55c4839abcde".into()),
            pane: Some("cpd-12".into()),
            on: Some("claude opus/high".into()),
            was: Some("wsp-174".into()),
            ending: Ending::Failed("cpd-12 is still standing".into()),
            decided: Some("2026-10-06T10:20:00Z".into()),
            text: "the second half does not hold\n1. spool first\n2. then deliver".into(),
        };
        let bare = Pass { at: "2026-10-06T11:00:00Z".into(), ..Pass::opened(None, None) };
        write(&mut t, &[bare.clone(), full.clone()]);
        assert_eq!(passes(&t), vec![full, bare], "oldest first, whichever order they were handed in");
        assert!(t.body.find("## Verification").unwrap() < t.body.find("## Log").unwrap(), "and the log stays last");
        assert!(t.body.contains("- 2026-10-06T10:00:00Z blocks · read 55c4839abcde · pane cpd-12"), "{}", t.body);
    }

    /// A landing records forty characters and an entry twelve: the same commit
    /// either way round, and never on fewer characters than git's own seven.
    #[test]
    fn a_commit_is_the_same_commit_at_either_length_and_never_on_a_stub() {
        assert!(same_commit("55c4839abcde", "55c4839abcdef0123456789"));
        assert!(same_commit("55c4839abcdef0123456789", "55c4839"));
        assert!(!same_commit("55c4839abcde", "55c4839abcdf"));
        assert!(!same_commit("55c", "55c4839"), "three characters name nothing");
    }

    /// A Verify row filed before `wsp-188`, the way wsp filed them.
    fn row(store: &Store, id: &str, parent: &str, created: &str, status: Status, verdict: &str) -> Task {
        let mut t = Task::new(&format!("Verify {parent}: the work"), id);
        t.parent = Some(parent.into());
        t.tags = vec![VERIFY_TAG.into()];
        t.created = created.into();
        t.status_raw = status.as_str().into();
        t.body = format!(
            "## Overview\nVerify **{parent}** (the work), a member of group 4 of the `run` worklist. \
             wsp spawned you when {parent} reached review and landed; nobody else is going to read this work.\n\n\
             ## Log\n- 2026-10-05 wsp: reading cf90cf59556955ce7bc774eb1bbe3c30f0895003\n\
             - 2026-10-05T11:40:00Z claimed by pane cpd-{n} · spawned at opus/high\n\
             - 2026-10-05 wsp: started by this run run group 4\n\
             - 2026-10-05T12:00:00Z {verdict}\n",
            n = &id[id.len() - 3..],
        );
        store.save_task(&t).unwrap();
        t
    }

    /// Ed, 2026-10-06: every Verify row wsp filed moves onto its parent's
    /// `## Verification`, oldest first, and is archived. Shaped on wsp-148,
    /// which had five — four blocks and a holds. Identified by what wsp wrote:
    /// a review somebody made by hand, tagged or titled like one, is named and
    /// left; a verifier still holding a claim is left until it finishes. And
    /// the old ids go on resolving, because logs and handovers cite them.
    #[test]
    fn the_migration_moves_every_row_wsp_filed_onto_its_parent_and_leaves_the_rest() {
        let (_env, store) = scratch("migrate");
        let mut m = Task::new("An empty or dead governor seat is filled", "wsp-148");
        m.status_raw = Status::Review.as_str().into();
        m.body = "## Overview\nthe member\n\n## Log\n- 2026-10-05 claimed\n".into();
        store.save_task(&m).unwrap();
        for (id, at, status, verdict) in [
            ("wsp-174", "2026-10-05T11:38:52Z", Status::Blocked, "blocked: wake::say must spool first"),
            ("wsp-175", "2026-10-05T15:00:00Z", Status::Blocked, "blocked: the reseat races the vacate"),
            ("wsp-178", "2026-10-05T18:00:00Z", Status::Blocked, "blocked: unseated reads as nothing"),
            ("wsp-179", "2026-10-05T21:00:00Z", Status::Blocked, "blocked: one seat filled twice"),
            ("wsp-183", "2026-10-06T07:00:00Z", Status::Review, "review: wsp-148 holds at 55c4839"),
        ] {
            row(&store, id, "wsp-148", at, status, verdict);
        }
        // Made by hand: a re-verify a person filed under the member, untagged,
        // and a barrier re-check tagged `verify` with no parent.
        let mut by_hand = Task::new("Re-verify wsp-148 at 7a0cf7d: the fixes, against the code", "wsp-137");
        by_hand.parent = Some("wsp-148".into());
        store.save_task(&by_hand).unwrap();
        let mut recheck = Task::new("Barrier re-check: round-2 group 3, after the hold", "compound-330");
        recheck.tags = vec![VERIFY_TAG.into()];
        store.save_task(&recheck).unwrap();
        // And a row *about* verification, which reviews nothing.
        store.save_task(&Task::new("A verification is a pass on the member's own row", "wsp-188")).unwrap();
        // Still running: claimed, its verdict not in.
        row(&store, "wsp-200", "wsp-148", "2026-10-06T09:00:00Z", Status::Doing, "note: reading");
        store.set_claim("wsp-200", serde_json::json!({ "workspace": "w" }));

        let planned = plan(&store);
        let named: Vec<&str> = planned.moves["wsp-148"].iter().map(|t| t.id.as_str()).collect();
        assert_eq!(named, ["wsp-174", "wsp-175", "wsp-178", "wsp-179", "wsp-183"], "every row wsp filed, oldest first");
        assert_eq!(planned.moves.len(), 1);
        assert_eq!(planned.running, ["wsp-200"], "the one still holding a claim is left until it finishes");
        let mut hand = planned.by_hand.clone();
        hand.sort();
        assert_eq!(hand, ["compound-330", "wsp-137"], "made by hand: named, and not among the moves");

        // The dry run writes nothing.
        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("verify-rows", "true"), ("n", "true")])), 0);
        assert!(store.find_task("wsp-174").is_some() && passes(&store.find_task("wsp-148").unwrap()).is_empty());

        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("verify-rows", "true")])), 0);
        let ps = passes(&store.find_task("wsp-148").unwrap());
        assert_eq!(
            ps.iter().map(|p| (p.was.as_deref().unwrap(), p.state)).collect::<Vec<_>>(),
            [
                ("wsp-174", State::Blocks),
                ("wsp-175", State::Blocks),
                ("wsp-178", State::Blocks),
                ("wsp-179", State::Blocks),
                ("wsp-183", State::Holds)
            ],
            "all five passes on wsp-148, oldest first"
        );
        assert_eq!(ps[0].read.as_deref(), Some("cf90cf595569"), "the commit it read");
        assert_eq!(ps[0].pane.as_deref(), Some("cpd-174"), "the pane");
        assert_eq!(ps[0].on.as_deref(), Some("opus/high"), "the model");
        assert_eq!(ps[0].text, "wake::say must spool first", "its block text");
        assert_eq!(ps[4].text, "wsp-148 holds at 55c4839", "and its review text");
        let newest = latest(&store.tasks(), "wsp-148").unwrap();
        assert_eq!(
            (newest.was.as_deref(), newest.state),
            (Some("wsp-200"), State::Running),
            "the row still running is still the newest pass, read off the row it is on"
        );

        // Old ids resolve from the archive, and `wsp show` prints them.
        assert!(store.find_task("wsp-174").is_none(), "archived out of the live store");
        assert!(matches!(store.resolve_task("wsp-174"), crate::store::Found::Archived(..)));
        assert_eq!(crate::cmd_task::show(&store, &Args::synth("show", &["wsp-174"], &[])), 0);
        // The rows left alone are where they were.
        for id in ["wsp-137", "compound-330", "wsp-200"] {
            assert!(store.find_task(id).is_some(), "{id} was not wsp's to move");
        }
        // And running it again moves nothing and duplicates nothing.
        store.clear_claim("wsp-200");
        assert_eq!(plan(&store).moves["wsp-148"].len(), 1, "only the one that has since finished");
        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("verify-rows", "true")])), 0);
        assert_eq!(passes(&store.find_task("wsp-148").unwrap()).len(), 6);
    }

    /// Until the migration has run, a Verify row is read as the pass it
    /// recorded — so a binary installed first reads the barrier exactly as the
    /// rows did, and opens nothing because a row looked like no verdict.
    #[test]
    fn an_unmigrated_verify_row_is_read_as_its_pass() {
        let (_env, store) = scratch("legacy");
        let mut m = Task::new("a member", "m-1");
        m.status_raw = Status::Review.as_str().into();
        store.save_task(&m).unwrap();
        row(&store, "v-001", "m-1", "2026-10-05T11:00:00Z", Status::Blocked, "blocked: no");
        assert!(blocked(&store.tasks(), "m-1").is_some_and(|p| p.was.as_deref() == Some("v-001")));
        row(&store, "v-002", "m-1", "2026-10-05T12:00:00Z", Status::Review, "review: yes");
        assert!(verified(&store.tasks(), "m-1"), "the newest row is the verdict");
        // A pass on the row itself, newer than both, is newer than both.
        let mut m = store.find_task("m-1").unwrap();
        let mut p = Pass::opened(None, None);
        p.at = "2026-10-06T00:00:00Z".into();
        write(&mut m, &[p]);
        store.save_task(&m).unwrap();
        assert!(!verified(&store.tasks(), "m-1"), "a pass still reading is not a verdict");
    }

    /// `wsp verified` from a governor's seat, with no pass of its own open, is
    /// a pass in the governor's name — the way a block is answered by hand.
    #[test]
    fn a_verdict_from_a_seat_with_no_pass_open_is_a_pass_of_its_own() {
        let (_env, store) = scratch("byhand");
        let mut m = Task::new("a member", "m-1");
        m.status_raw = Status::Review.as_str().into();
        store.save_task(&m).unwrap();
        record(&store, "m-1", State::Holds, "the finding was about code nobody shipped", Some("w1:p1")).unwrap();
        let ps = passes(&store.find_task("m-1").unwrap());
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].pane.as_deref(), Some("w1:p1"));
        assert_eq!(ps[0].ending, Ending::Ended, "nothing for the tick to end: wsp started no agent for it");
        assert!(seats(&store.tasks()).is_empty(), "and the governor's seat is not named a verifier's");
    }
}
