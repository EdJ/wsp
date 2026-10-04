//! `wsp brief` — what an agent is handed when a session starts.
//!
//! Everything here is already answerable by some other subcommand: `where` for
//! the project, `ls` for the backlog, `wip` for the other agents. The point of
//! one command is that a session-start hook can afford exactly one, and that
//! what it prints is a *briefing* rather than a report — the few facts an agent
//! cannot work correctly without, in the order it needs them, short enough that
//! nobody is tempted to turn it off.
//!
//! It never fails. A store with nothing in it, a herdr that is not answering, a
//! pane belonging to no project: each of those is a shorter brief, not an
//! error. A hook that errors on a fresh machine is a hook people delete.

use serde_json::json;

use crate::cmd_agent::{self, current_project};
use crate::cmd_govern;
use crate::cmd_mandate;
use crate::herdr;
use crate::model::Task;
use crate::overlap;
use crate::resolve;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// How much backlog to show. The brief is read every session and paid for in
/// context every session; the tail is one `wsp ls` away.
const MAX_TASKS: usize = 6;
/// Other agents, newest attention first. More than this and it stops being a
/// briefing and starts being `wip`.
const MAX_OTHERS: usize = 6;
/// The standing rules, if the store carries any.
///
/// Was 40, which was under half of what `agents.md` had grown to — and the cut
/// fell in the middle of the commit procedure, so every agent was briefed on
/// staging into its own index and none on committing with it, building it, or
/// looking at the pane afterwards. A cap on the rules is right; a cap that
/// silently keeps the first half of a numbered list is not.
const MAX_RULES: usize = 120;
/// Decisions binding this project. Few, and the most recent — a decision is
/// read to know what is already settled, and the settled thing that matters is
/// rarely the oldest.
const MAX_DECISIONS: usize = 4;

/// How much of the brief a caller wants. One axis with three points rather than
/// two flags that can disagree.
///
/// `Normal` is what a person, or an agent mid-session, gets from `wsp brief`,
/// and the standing rule about it is that **it does not grow**. It is run
/// constantly — by every agent, and by the coordinating seat several times an
/// hour — so a thousand tokens added here is a thousand tokens times every call
/// in every session on the machine.
///
/// `Terse` is `Normal` minus what the caller says it already has.
///
/// `Session` is the payload, and the only caller entitled to it is the
/// `SessionStart` hook. Every request in a session re-reads its whole context,
/// so a token present at request 0 is paid by every request after it — which
/// sounds like a reason to inject nothing, and is in fact the reason to inject
/// *this*. Measured on robustness-031: an agent that arrived with a task title
/// spent 14,450 tokens over requests 4–16 rebuilding context the spawning
/// session already had, and then carried it for the remaining ~86 requests
/// anyway. Handing it over at request 0 costs about half that and removes the
/// round-trips. The same arithmetic run backwards is why `Normal` must not
/// grow, and why this is a separate mode rather than a better default.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Depth {
    Session,
    Normal,
    Terse,
}

impl Depth {
    fn of(args: &Args) -> Depth {
        match (args.has("session"), args.terse()) {
            // `--session` wins over `--terse`. They are contradictory and the
            // hook is the one that passes `--session`, so a `WSP_TERSE` left in
            // the environment must not quietly strip the payload it exists to
            // deliver.
            (true, _) => Depth::Session,
            (false, true) => Depth::Terse,
            (false, false) => Depth::Normal,
        }
    }
    fn session(self) -> bool {
        self == Depth::Session
    }
}

/// The claimed task's own prose, whole. Capped only because nothing else bounds
/// it, and the cap names what it dropped for the reason [`MAX_RULES`] does — a
/// briefing that stops mid-overview reads exactly like an overview that stops
/// there.
const MAX_TASK_LINES: usize = 120;
/// Where the task's **Details** section stops in the payload. The split of the
/// payload (`core-049`) is certain-now against fetch-on-first-use, and within a
/// task's own prose the two halves are its sections: the overview is *what this
/// is* — the thing robustness-031 measured an agent rebuilding at 14,450 tokens
/// when it was not handed — and details is supplementary, read when the work
/// turns out to need it. Measured on the live store, 2026-08-25: 211 open tasks
/// carry prose, median five lines of it in details; the ten whales run past a
/// hundred. Twenty-four keeps every ordinary task byte-for-byte what it was,
/// points at the rest, and takes roughly half of every capped payload.
const MAX_DETAILS_LINES: usize = 24;
/// The handbook, over the whole project chain.
const MAX_HANDBOOK_LINES: usize = 120;
/// Where an **ancestor's** handbook stops in the payload. A handbook above the
/// project being worked is the same text for every agent under that ancestor —
/// paid for by every spawn beneath it, whatever it says — while what it carries
/// is standing rules: needed before acting on them, not needed at request 0.
/// Its first paragraph says what the place is; [`block`] prints where the rest
/// lives. The nearest project's handbook stays under [`MAX_HANDBOOK_LINES`]
/// whole, because that one was written for exactly this work.
const MAX_HANDBOOK_LEAD: usize = 8;
/// Decisions on the parent, most recent last. The parent is where direction
/// lands, so these are the constraints on the piece in hand.
const MAX_PARENT_DECISIONS: usize = 6;
/// Siblings named by id in what is injected above. Title and status only: the
/// question they answer is "what is robustness-017", and answering it with the
/// whole task would be the fetching this replaces, done eagerly.
const MAX_REFS: usize = 8;
/// The tail of the task's log. Short on purpose — most of a log is status
/// churn, at about eight tokens a line — but not zero, because direction handed
/// to a task after it was written arrives here and nowhere else. The log line
/// on this very task carried the file list that saved its agent the 28,000
/// tokens of searching the task exists to remove. Each entry is bounded too,
/// by [`MAX_LOG_ENTRY_CHARS`]: the churn assumption stopped holding the day
/// review notes started landing in logs.
const MAX_LOG: usize = 4;
/// How much of one log entry the payload shows, in characters rather than
/// lines — an entry *is* one line in the store, so a line cap would be a
/// tautology.
///
/// [`MAX_LOG`]'s pricing assumed churn: claimed, released, noted. Measured on
/// the live store, single entries now run past 2,000 characters (~500 tokens),
/// and four of those is half a payload spent on the one block the handbook
/// itself names as the wrong home for prose. 400 is two or three sentences —
/// enough to know what happened and whether the rest is worth fetching — and
/// what is cut past it is named, never silent: see `session_lines`.
const MAX_LOG_ENTRY_CHARS: usize = 400;
/// Where an abridged parent decision stops. First sentence, then here: the
/// sentence is the rule and the rest is the argument, which is exactly the
/// split `wsp project show` abridges its own decisions block on — same cut,
/// same reason, different pointer (`wsp show <parent>`).
const MAX_BIND_LEAD: usize = 150;

/// The protocol an agent works to, kept in the store rather than in this
/// binary. It is the user's to write, versioned with the tasks it talks about,
/// and readable by anything that can read a file — none of which is true of a
/// string compiled in here.
fn rules(store: &Store) -> Option<String> {
    let path = store.root.join("agents.md");
    let text = std::fs::read_to_string(&path).ok()?;
    let total = text.lines().count();
    let kept: Vec<&str> = text.lines().take(MAX_RULES).collect();
    let mut out = kept.join("\n").trim_end().to_string();
    if out.is_empty() {
        return None;
    }
    // Never drop rules quietly. A rule an agent has not been given is a rule it
    // will not follow, and a briefing that ends mid-procedure reads exactly
    // like a procedure that ends there.
    if total > MAX_RULES {
        out.push_str(&format!(
            "\n\n({} more lines — read the rest: cat {})",
            total - MAX_RULES,
            util::contract(&path)
        ));
    }
    Some(out)
}

/// `wsp commit-help` — the shared-tree commit procedure, asked for rather than
/// imposed.
///
/// It was two thirds of `agents.md`, which meant the brief spent fifty lines on
/// git ritual in every session, before the agent reading it knew whether it
/// would commit anything at all. Most sessions never stage a thing; the ones
/// that do are about to read it carefully anyway. So the brief keeps one line
/// pointing here, and the procedure is read at the moment it is used.
///
/// In the store beside `agents.md`, for the reason [`rules`] gives: it is the
/// user's to write, and it changes when the tooling does rather than when this
/// binary is rebuilt.
pub fn commit_help(store: &Store, args: &Args) -> i32 {
    let path = store.root.join("committing.md");
    let text = std::fs::read_to_string(&path)
        .ok()
        .map(|t| t.trim_end().to_string())
        .filter(|t| !t.is_empty());

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "path": util::contract(&path),
                "text": text,
            }))
            .unwrap_or_default()
        );
        return i32::from(text.is_none());
    }

    match text {
        Some(t) => {
            println!("{t}");
            stash_here();
            0
        }
        // Unlike the brief, this one is allowed to fail: it was asked for, and
        // an empty answer to "how do I commit here" is worse than none. Say
        // where the file goes and where the reasoning lives.
        None => {
            let p = Paint::new();
            eprintln!("{} no {}", p.yellow("✗"), util::contract(&path));
            eprintln!(
                "  {}",
                p.dim("the procedure it should hold is the *Two agents in one tree* section of the wsp README")
            );
            1
        }
    }
}

/// The one line `committing.md` cannot know is true: there is a stash on the
/// stack of the repository you are standing in, right now.
///
/// The file already carries the recovery — *`git stash show --stat` first and
/// recover by path, never `pop`* — and it carries it whether or not there is
/// anything to recover, which is how a true sentence reads as background. The
/// stash is refused at creation now (`crate::guard`), so an entry here is
/// stranded work from before the guard or a person's own, and either way it is
/// visible to every worktree and one `pop` away from landing in the wrong one.
///
/// Printed only when there is one, which is the whole reason it is affordable:
/// the normal answer is silence and costs one `git` call on a command nobody
/// runs in a loop.
fn stash_here() {
    let dir = std::env::current_dir().unwrap_or_default();
    let held = crate::guard::stashed(&dir);
    if held.is_empty() {
        return;
    }
    let p = Paint::new();
    let mut lines = crate::guard::stash_lines(&held).into_iter();
    eprintln!();
    eprintln!("{} {}", p.yellow("▲"), lines.next().unwrap_or_default());
    for rest in lines {
        eprintln!("{}", p.dim(&rest));
    }
}

/// Task ids written into a chunk of prose — `robustness-031`, and nothing else.
///
/// Scanned rather than declared. A task names its siblings by writing them into
/// its overview — "independent of robustness-033", "read robustness-031's overview
/// first" — and that is the reference an arriving agent goes and looks up. The
/// `refs` frontmatter field is a different thing holding paths, and reading it
/// as this would inject the wrong list.
fn mentioned(text: &str, out: &mut Vec<String>) {
    let b = text.as_bytes();
    let digits = |from: usize, n: usize| {
        from + n <= b.len() && b[from..from + n].iter().all(u8::is_ascii_digit)
    };
    let mut i = 0;
    while i + 4 < b.len() {
        // Mid-word is not a reference: `t-` inside `wsp-t-260816-096` is, but
        // inside `output-260816-096` it is not, and the boundary is what tells
        // them apart.
        let boundary = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if !(boundary && b[i] == b't' && b[i + 1] == b'-' && digits(i + 2, 6) && b[i + 8] == b'-') {
            i += 1;
            continue;
        }
        // One to four digits of sequence, however wide the day's ids ran.
        let mut end = i + 9;
        while end < b.len() && b[end].is_ascii_digit() && end - (i + 9) < 4 {
            end += 1;
        }
        if end > i + 9 {
            let id = text[i..end].to_string();
            if !out.contains(&id) {
                out.push(id);
            }
        }
        i = end;
    }
}

/// Everything the brief reads, gathered into one value.
///
/// The same bargain [`crate::panel::Snapshot`] makes, and the one this file's
/// header has been claiming all along: "a store with nothing in it, a herdr
/// that is not answering, a pane belonging to no project: each of those is a
/// shorter brief, not an error." That is three promises about states nobody
/// can arrange on demand, in the one command every session starts with, and
/// while the reading and the drawing were the same function there was nowhere
/// to hang a fixture that held any of them.
pub(crate) struct Briefing {
    /// The panes, and the store behind them. `standing_beside` is the one
    /// definition of who else is here — `wsp overlap` and `wsp claim` read the
    /// same vector — so this is that value rather than a second copy of the
    /// store beside it.
    pub world: overlap::World,
    /// Standing direction for this workspace, if there is any.
    pub mandate: Option<String>,
    /// Every seat that exists. Composed against, rather than resolved here,
    /// because which seat answers for this pane depends on the project the
    /// brief is still working out.
    pub governors: std::collections::BTreeMap<String, serde_json::Value>,
    /// Every task in a *running* worklist, to where it sits. One read of
    /// `worklists/`, and empty in the ordinary state — see
    /// [`crate::worklist::Running`]. Composed against for two different
    /// questions: which group the task in hand is in, and the front of the
    /// walk that finds the seat answering for it.
    pub lists: crate::worklist::Running,
    /// Where the list this workspace *governs* has got to, when the seat it
    /// holds is on a worklist rather than a project.
    ///
    /// Read here rather than worked out in `compose`, which reads nothing that
    /// is not in `b`: a position is a walk over the store, and it is the one
    /// fact in a brief that costs more than a lookup. Nothing is spent on it
    /// unless this pane is a custodian and the thing it is custodian of is a
    /// list, which is one pane on the machine on the nights there is one at all.
    pub seat_at: Option<crate::worklist::Position>,
    /// The store's own rules, already capped.
    pub rules: Option<String>,
    /// Where this pane resolved to. Taken in rather than worked out here: the
    /// chain is `wsp where`'s subject and it reads the process environment.
    pub project: Option<String>,
    pub pane: Option<String>,
    pub workspace: Option<String>,
    /// The rotation in flight that names this pane as its successor, as
    /// `(scope, from)`: what it is about to hold, and the pane wsp ends once
    /// it does. Read rather than derived, because between the moment a rotation
    /// seats its successor and the moment the slot moves, nothing else in wsp
    /// says this pane is anything but a worker; see
    /// [`crate::cmd_govern::incoming`].
    pub incoming: Option<(String, String)>,
    /// Why wsp could not end that pane, once it has tried.
    /// [`crate::cmd_govern::ending_failed`].
    pub ending_failed: Option<String>,
    /// This process's directory, contracted. herdr reports the shell's, which
    /// is stale the moment anyone `cd`s.
    pub cwd: Option<String>,
    /// The machine's daemon, when it has none — [`crate::daemon::loud`]'s one
    /// sentence, read here because `compose` is pure over this struct and the
    /// `ps` it costs does not belong in a function that draws.
    ///
    /// Drawn on the **seat** line and nowhere else (`wsp-145`). The brief is
    /// re-read on every request of every session, so an ordinary agent must
    /// still pay nothing for a fact that never concerns it — and a seat is the
    /// one reader for whom "nothing is going to wake me" is a fact about its
    /// own job. Six days of a dead daemon on a machine that looked busy are
    /// what this is for; see [`crate::launchd`].
    pub daemon: Option<String>,
}

/// Where a brief is *for*, when that is not where the process is.
///
/// [`Briefing::live`] answers "where am I" out of the process environment:
/// herdr's `WSP_*`, [`cmd_agent::my_pane`], the working directory. That is the
/// right question for a session-start hook and the wrong one for `wsp spawn`,
/// which composes a brief for a seat it has just opened and is about to start
/// an agent in. Composed with `live`, that brief would carry the *spawning*
/// session's pane, its tree, and therefore — through `task_in_hand` — its task.
///
/// So the discovery is the argument and the composition is shared. Everything
/// from [`compose`] down was already pure over [`Briefing`]; this is the only
/// thing that tied a brief to a session that exists.
pub(crate) struct At<'a> {
    /// The project the work resolved to. Passed rather than resolved, because
    /// the caller has already done the resolution `-p` and the cwd chain would
    /// redo differently from over here.
    pub project: Option<&'a str>,
    /// The seat, in herdr's spelling. It exists by the time this is asked —
    /// `spawn` opens the seat before it starts anything in it — so this is a
    /// real pane id and not a guess.
    pub pane: Option<&'a str>,
    pub workspace: Option<&'a str>,
    /// The tree the agent will be standing in, which for a task spawn is its
    /// own `wsp checkout` and not the trunk. This is what puts the `tree  your
    /// own` line in front of an agent that would otherwise commit as if it
    /// were on the trunk.
    pub cwd: Option<&'a str>,
}

impl Briefing {
    /// The read for a seat that is about to exist.
    ///
    /// The claim has already landed when `spawn` calls this — the order is
    /// workspace, claim, agent — so `mine` resolves through the claim keyed on
    /// this workspace, exactly as it will for the session once it opens.
    pub(crate) fn at(store: &Store, seat: At<'_>) -> Briefing {
        let world = overlap::World::live(store);
        let governors = store.governors();
        // The seat, or the rotation that is about to make this pane the seat.
        // During a rotation the slot still names its predecessor — it moves
        // only once the successor's first turn is confirmed — so `governs`
        // answers nothing here, and without the record the successor's brief
        // would introduce it as a worker with a strange order to run.
        let governed = held(&governors, seat.workspace, seat.pane);
        let handovers = store.handovers();
        let incoming = seat.pane.and_then(|pane| cmd_govern::incoming(&handovers, Some(pane)));
        let ending_failed =
            incoming.as_ref().and_then(|(scope, _)| cmd_govern::ending_failed(&handovers, scope));
        let seat_at = governed
            .clone()
            .or_else(|| incoming.as_ref().map(|(scope, _)| scope.clone()))
            .and_then(|scope| crate::worklist::running_position(store, &scope));
        // The agent's row already exists by the time this is asked — the
        // claim that landed before this call minted it (`compound-092`
        // stage B) — so this is a plain lookup, never a mint.
        let agent = seat.pane.and_then(|p| store.agent_in_seat(p));
        Briefing {
            project: seat.project.map(str::to_string),
            mandate: cmd_mandate::current(store, agent.as_deref(), seat.workspace),
            lists: crate::worklist::Running::read(store),
            seat_at,
            governors,
            rules: rules(store),
            pane: seat.pane.map(str::to_string),
            workspace: seat.workspace.map(str::to_string),
            incoming,
            ending_failed,
            // Normalised rather than taken as given: the caller's path may be
            // absolute or already contracted, and `own_tree` expands whatever
            // is here before asking whether it is a checkout.
            cwd: seat.cwd.map(|c| util::contract(&util::expand(c))),
            daemon: crate::daemon::loud(crate::daemon::running(&store.state).as_deref(), crate::daemon::a_backend_answered()),
            world,
        }
    }

    /// The live read. `current_project` can fail on a bad `-p`; a brief never
    /// does, so an unresolvable project is no project, which is a shorter
    /// brief.
    pub(crate) fn live(store: &Store, args: &Args) -> Briefing {
        let world = overlap::World::live(store);
        let env = herdr::Env::read();
        let governors = store.governors();
        // The same two-step read [`Briefing::at`] makes, for the same reason:
        // a rotation's successor reads its brief from its `SessionStart` hook,
        // which runs before the slot moves to it. Off `my_pane()` rather than
        // `HERDR_WORKSPACE_ID`/`HERDR_PANE_ID` — `cmd_message::whoami`'s reason,
        // here too: this is a decision and not a display, so the port's own
        // answer to *where am I* is the one it is asked with.
        let pane = cmd_agent::my_pane();
        let governed = held(&governors, env.workspace_id.as_deref(), pane.as_deref());
        let handovers = store.handovers();
        let incoming = cmd_govern::incoming(&handovers, pane.as_deref());
        let ending_failed =
            incoming.as_ref().and_then(|(scope, _)| cmd_govern::ending_failed(&handovers, scope));
        let seat_at = governed
            .clone()
            .or_else(|| incoming.as_ref().map(|(scope, _)| scope.clone()))
            .and_then(|scope| crate::worklist::running_position(store, &scope));
        let agent = pane.as_deref().and_then(|p| store.agent_in_seat(p));
        Briefing {
            project: current_project(store, args, &world.index).unwrap_or(None),
            mandate: cmd_mandate::current(store, agent.as_deref(), env.workspace_id.as_deref()),
            lists: crate::worklist::Running::read(store),
            seat_at,
            governors,
            rules: rules(store),
            // The seat, through `my_pane`'s one reading of it, so that this
            // answers for an agent a supervisor is hosting as well as one in a
            // pane. Everything downstream keys on it — what this seat holds,
            // whether it is the governing one — and a headless agent that read
            // no seat opened believing it held nothing, which is the whole of
            // what a `SessionStart` brief exists to prevent.
            pane,
            workspace: env.workspace_id,
            incoming,
            ending_failed,
            cwd: std::env::current_dir().ok().map(|c| util::contract(&c)),
            daemon: crate::daemon::loud(crate::daemon::running(&store.state).as_deref(), crate::daemon::a_backend_answered()),
            world,
        }
    }
}

/// The scope this pane holds the slot of: asked with the pane when there is
/// one, and with the room only when there is not, which is
/// [`cmd_govern::seat_query`]'s order.
///
/// Both readings used to ask only when there was a room, and a room here is
/// `HERDR_WORKSPACE_ID`. A compound seat has none, so after a rotation the
/// successor's brief said `coordinating here` while governors.json named it
/// the seat, and `wsp govern` agreed with the file (`wsp-128`). One match, the
/// same exact-pane one `govern` and `despawn` make, so the brief and the record
/// cannot disagree.
fn held(governors: &std::collections::BTreeMap<String, serde_json::Value>, workspace: Option<&str>, pane: Option<&str>) -> Option<String> {
    let who = pane.filter(|p| !p.is_empty()).or(workspace)?;
    cmd_govern::governs(governors, &crate::place::Seat::new(who))
}

/// The brief, composed: every decision made and nothing drawn yet.
pub(crate) struct Brief {
    pub project: Option<String>,
    /// The chain from the root down to this project, which is what `where`
    /// prints as `a/b/c`.
    pub path: Vec<String>,
    pub tags: Vec<String>,
    pub about: String,
    pub mandate: Option<String>,
    pub rules: Option<String>,
    /// The task this pane is on, what it hangs under, and how much is open
    /// beneath it.
    pub mine: Option<Task>,
    pub parent: Option<Task>,
    pub under_mine: usize,
    /// The backlog, each with its own count of open sub-tasks. Whole, because
    /// `--json` carries all of it; [`Brief::shown`] is where the text stops.
    pub open: Vec<(Task, usize)>,
    pub shown: usize,
    /// What is settled and still binds, oldest first — superseded entries are
    /// not in it. [`Brief::dropped`] is how many of the record are not here.
    pub decided: Vec<(String, String)>,
    pub dropped: usize,
    /// Panes that can reach the files under your hands, and everyone else.
    pub near: Vec<overlap::Standing>,
    pub far: Vec<overlap::Standing>,
    /// Of the far set, the ones worth naming, and how many are left over.
    pub others: Vec<overlap::Standing>,
    pub hidden: usize,
    /// Whether this brief is an agent going looking, and whether it found
    /// anything — the one thing the brief tells herdr rather than the reader.
    /// `None` when the question does not apply.
    pub looking: Option<bool>,
    /// The seat that answers for work here, and whether this pane is it.
    ///
    /// `None` is the ordinary case — no seat anywhere above this project — and
    /// it draws nothing at all. That matters more here than anywhere else in
    /// wsp: the brief is re-read on every request of every session, so a line
    /// that is present when nobody is coordinating is a line paid tens of
    /// thousands of times a night for a fact that is always the same.
    pub seat: Option<(cmd_govern::Seat, bool)>,

    /// Which group of a running worklist the task in hand is a member of.
    ///
    /// One line, and only under a running list: `list  batch  group 2 of 4`.
    /// It is what an agent cannot work out from anywhere else — that the work
    /// in its hands is part of a run, that something is waiting at a barrier
    /// behind it, and which seat its raised hands reach. `None` for every agent
    /// on every ordinary night, and it draws nothing at all: the same bargain
    /// [`Brief::seat`] makes, for the same reason.
    pub list: Option<crate::worklist::Placing>,

    /// The scope this pane's workspace is the governor of, if it is one.
    ///
    /// The second half of the seat, and a different question from it. `seat`
    /// above answers *who coordinates the work in front of me* by walking up
    /// from this pane's project; this answers *what am I answerable for*, and
    /// the two disagree exactly when they should — a workspace holding the
    /// `wsp` slot while standing in `robustness` reads the first as its own
    /// seat and only this one as its job.
    ///
    /// `None` for every ordinary agent, which is every agent nearly all of the
    /// time, and draws nothing at all. That is the same bargain `seat` makes
    /// and it matters for the same reason: this is the output every request of
    /// every session pays for.
    pub custodian: Option<String>,

    /// Where that list has got to, when what this pane governs is a worklist.
    ///
    /// The seat line names the position instead of a project, because for a
    /// governor of a list *that is the job*: which group is being waited on is
    /// the whole state of the thing it is holding. `None` when the seat is on a
    /// project, which is every seat until a list is running.
    pub seat_at: Option<crate::worklist::Position>,

    /// The machine's daemon, when it has none — see [`Briefing::daemon`], which
    /// is where the argument for reading it is.
    pub daemon: Option<String>,

    /// The pane this pane replaced, while the handover record that names both
    /// is still standing: from the successor's seat opening until wsp has ended
    /// the predecessor, or for good if it could not. One line, on one pane on
    /// the machine, and nothing at all otherwise. See
    /// [`crate::cmd_spawn::rotate`] for why this rides the brief rather than
    /// the typed work order.
    pub succeeding: Option<String>,

    /// Whether the slot has moved to this pane yet. wsp carries out the ending
    /// once it has; before that, the line says it will.
    pub rotation_pending: bool,

    /// Why the predecessor is still running, when wsp tried to end it and
    /// could not.
    pub ending_failed: Option<String>,

    /// The `wsp checkout` tree this pane is standing in, when it is in one.
    ///
    /// Worth a line of a brief, which is the most expensive output wsp has —
    /// every request of every session pays for it — because it is two facts an
    /// agent cannot get from anywhere else and gets wrong in opposite
    /// directions without: that the ordinary commit procedure does not apply
    /// here, and that a commit in this tree is on a branch and has not reached
    /// the trunk until it is landed. An agent that does not know the second one
    /// leaves finished work sitting in a directory nobody reviews.
    pub own_tree: Option<String>,

    // The session payload. Composed always, because composing it is a walk over
    // values already in hand and a `Brief` that meant different things in
    // different modes would be two structs; rendered only under
    // [`Depth::Session`], which is where the cost actually is.
    /// `## Handbook` down the project chain, root first, as `(project, text)`.
    /// Inherited the way tags and decisions are: `wsp` says how work is done in
    /// this tree and where the code's own documentation lives, `robustness`
    /// adds what is true of `robustness`, and neither has to repeat the other.
    pub handbook: Vec<(String, String)>,
    /// Decisions on the parent — where direction lands — oldest first.
    pub parent_decided: Vec<(String, String)>,
    /// The tail of the claimed task's log, oldest first.
    pub mine_log: Vec<String>,
    /// Siblings named by id in the payload above, with what they are and how
    /// far along they got.
    pub refs: Vec<Task>,
}

/// Work out the whole brief. Pure: everything it reads is in `b`.
pub(crate) fn compose(b: &Briefing) -> Brief {
    let index = &b.world.index;
    let tasks = &b.world.tasks;

    let tags = b.project.as_deref().map(|p| index.effective_tags(p)).unwrap_or_default();
    let path: Vec<String> = match &b.project {
        Some(p) => {
            let mut chain = index.ancestors(p);
            chain.reverse();
            chain.push(p.clone());
            chain
        }
        None => Vec::new(),
    };
    let about = b.project.as_deref().and_then(|p| index.get(p)).map(|p| p.brief.clone()).unwrap_or_default();

    // What this pane is on. The binding is the live answer; the claim is the
    // durable one and outlives a restart, so a session that comes back before
    // the daemon has reconciled still knows what it was doing.
    let mine: Option<&Task> = cmd_agent::task_in_hand(
        &b.world.bindings,
        &b.world.claims,
        b.pane.as_deref(),
        b.workspace.as_deref(),
    )
    .and_then(|id| tasks.iter().find(|t| t.id == id));

    // The backlog for this project, minus whatever is already in hand.
    let scope: Option<Vec<String>> = b.project.as_deref().map(|p| index.subtree(p));
    let mut open: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.status().is_open())
        // The subtree, as `ls` and `next` scope it. Exact-project was a third
        // answer to one question: under a mandate on `wsp` the brief would
        // list nothing from `data` while `next` handed you a task out of it.
        .filter(|t| match &scope {
            Some(ids) => t.project.as_ref().map(|p| ids.contains(p)).unwrap_or(false),
            None => t.project.is_none(),
        })
        .filter(|t| Some(t.id.as_str()) != mine.map(|m| m.id.as_str()))
        .collect();
    // A sub-task whose parent is also on the list is already spoken for: the
    // parent carries the count. Listing both spends the cap twice on one piece
    // of work and buries the other five.
    let listed: Vec<String> = open.iter().map(|t| t.id.clone()).collect();
    open.retain(|t| match &t.parent {
        Some(p) => !listed.contains(p),
        None => true,
    });
    open.sort_by(|a, b| {
        a.status()
            .rank()
            .cmp(&b.status().rank())
            .then(a.priority().rank().cmp(&b.priority().rank()))
            .then(a.id.cmp(&b.id))
    });
    let shown = open.len().min(MAX_TASKS);

    // What binds, not the whole record. A decision a later one supersedes has
    // been withdrawn, and four lines of a briefing spent stating a rule that no
    // longer holds is worse than spending nothing — the reader acts on it. The
    // record itself is intact and one command away, which is where the count
    // below points; `project show` prints the withdrawn entries struck through
    // rather than dropping them, because that is the view you go to for the
    // history rather than for the rules.
    //
    // Per project and then flattened, never the other way round: an id names a
    // decision within one file, so `d2` on `wsp` and `d2` on `render` are two
    // different decisions and a supersession must not reach across.
    let all: Vec<(String, String)> = path
        .iter()
        .filter_map(|id| index.get(id))
        .flat_map(|proj| crate::model::decisions(&proj.body))
        .collect();
    let decided: Vec<(String, String)> = path
        .iter()
        .filter_map(|id| index.get(id))
        .flat_map(|proj| crate::model::live_decisions(&proj.body))
        .map(|d| (d.when, d.text))
        .collect();
    // Everything not printed here, superseded entries included, so the pointer
    // to the full block appears whenever there is more of it to read.
    let dropped = all.len() - decided.len().min(MAX_DECISIONS);

    // Everyone else, nearest first. `standing_beside` is the one definition of
    // that reckoning, so the brief's only job is deciding what a briefing shows
    // of it.
    let all = overlap::standing_beside(
        &b.world,
        b.pane.as_deref().unwrap_or_default(),
        b.cwd.as_deref(),
    );

    // Two questions, and only the first is a warning. Panes that can reach the
    // files under your hands go at the top and get the colour; everyone else
    // is context.
    let (near, far): (Vec<overlap::Standing>, Vec<overlap::Standing>) =
        all.into_iter().partition(|s| s.relation.is_near());

    // Of twenty-two panes here, twenty are shells that have been sitting in a
    // directory since Tuesday. Naming them would push the two that matter off
    // the bottom, so the far set names whoever is holding something and counts
    // the rest in one line.
    let named: Vec<overlap::Standing> =
        far.iter().filter(|s| s.agent || s.task.is_some()).cloned().collect();
    let quiet = far.len() - named.len();
    let far_shown = named.len().min(MAX_OTHERS);
    let hidden = named.len() - far_shown + quiet;

    // The whole chain's handbooks, general to specific. Empty sections are
    // simply absent — a project with nothing to say is not a heading with
    // nothing under it.
    let handbook: Vec<(String, String)> = path
        .iter()
        .filter_map(|id| index.get(id))
        .filter_map(|proj| {
            crate::model::section_of(&proj.body, "Handbook").map(|t| (proj.id.clone(), t))
        })
        .collect();

    let parent: Option<&Task> = mine
        .and_then(|t| t.parent.as_ref())
        .and_then(|id| tasks.iter().find(|x| &x.id == id));

    // Live only, for the reason the project's are: this is direction handed
    // down, and direction that was withdrawn is not direction.
    let parent_decided: Vec<(String, String)> = parent
        .map(|p| crate::model::live_decisions(&p.body))
        .unwrap_or_default()
        .into_iter()
        .map(|d| (d.when, d.text))
        .collect();

    let mine_log: Vec<String> = mine
        .and_then(|t| crate::model::section_of(&t.body, "Log"))
        // The brief is the one place an agent reads its own history, and it is
        // read at 01:15 as often as at any other hour. The stamp goes out as
        // the reader's date, the same as the decisions above it.
        .map(|log| crate::model::localise_dates(&log))
        .map(|log| {
            let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
            lines
                .iter()
                .skip(lines.len().saturating_sub(MAX_LOG))
                .map(|l| l.trim().trim_start_matches("- ").trim().to_string())
                .collect()
        })
        .unwrap_or_default();

    // Every id the payload mentions, looked up once. The task itself and its
    // parent are already spelled out above, so naming them again here would be
    // the same rows twice.
    let mut ids: Vec<String> = Vec::new();
    if let Some(t) = mine {
        mentioned(&t.title, &mut ids);
        mentioned(&t.body, &mut ids);
    }
    for (_, what) in &parent_decided {
        mentioned(what, &mut ids);
    }
    let refs: Vec<Task> = ids
        .iter()
        .filter(|id| Some(id.as_str()) != mine.map(|t| t.id.as_str()))
        .filter(|id| Some(id.as_str()) != parent.map(|t| t.id.as_str()))
        // An id that resolves to nothing is dropped rather than reported. It is
        // usually a task that has been removed, or one from another store, and
        // a line saying so would be noise in the one place noise is expensive.
        .filter_map(|id| tasks.iter().find(|t| &t.id == id))
        .take(MAX_REFS)
        .cloned()
        .collect();

    // The pane's and not the room's, which is the whole of worklist-035 read
    // from the brief: two agents spawned into a custodian's workspace were each
    // told `seat  governor of acc  yours to sequence, direct, review` — a work
    // order neither had asked for, on a night nobody was reading. During a
    // rotation the record is what says the successor is the seat, because the
    // slot itself has not moved yet.
    let governed = held(&b.governors, b.workspace.as_deref(), b.pane.as_deref());
    let custodian = governed.clone().or_else(|| {
        b.incoming.as_ref().map(|(scope, _)| scope.clone())
    });
    let rotation_pending = cmd_govern::rotation_pending(governed.as_deref(), b.incoming.as_ref());

    Brief {
        handbook,
        parent_decided,
        mine_log,
        refs,
        // A briefing under a mandate, with nothing in hand, *is* an agent going
        // looking: the mandate is the permission and the list below is the
        // backlog, so this session's next move is to pick something out of it.
        // It is the longest of the looking windows — a whole session start —
        // and the one nobody sent, so nothing else would say so. A brief with
        // no mandate is left alone: an agent that has to ask before it takes
        // anything is waiting on a person, not looking.
        looking: (mine.is_none() && b.mandate.is_some()).then(|| !open.is_empty()),
        // The task in hand rather than only the project it lives in, because
        // the first step of the walk is keyed on the task: an agent working a
        // member of tonight's list is answered for by whoever is running the
        // list. With nothing running this is `None` and the walk is the
        // ancestor walk it was.
        seat: cmd_govern::seat_for(
            &b.governors,
            index,
            mine.and_then(|t| b.lists.list_of(&t.id)),
            b.project.as_deref(),
        )
        .map(|s| {
            // The pane's and not the room's: a worker beside a custodian was
            // told the seat above its work was itself — worklist-035. The
            // room comes off the governor record (`cmd_govern::room_of`)
            // rather than a struct field (`compound-096`).
            let mine = b.workspace.as_deref().is_some_and(|ws| {
                let room = cmd_govern::room_of(&b.governors, &s.scope);
                room == ws && (s.pane.is_empty() || b.pane.as_deref().is_none_or(|p| p == s.pane))
            });
            (s, mine)
        }),
        list: mine.and_then(|t| b.lists.of(&t.id)).cloned(),
        daemon: b.daemon.clone(),
        seat_at: b.seat_at.clone(),
        custodian,
        succeeding: b.incoming.as_ref().map(|(_, from)| from.clone()),
        rotation_pending,
        ending_failed: b.ending_failed.clone(),
        // A path rule rather than a question for git, so this stays free in the
        // one command a session-start hook runs.
        own_tree: b
            .cwd
            .as_deref()
            .and_then(|c| crate::cmd_checkout::worktree_of(&util::expand(c)))
            .and_then(|d| d.file_name().and_then(|s| s.to_str()).map(str::to_string)),
        under_mine: mine.map(|t| resolve::counts_under(tasks, &t.id).open).unwrap_or(0),
        parent: parent.cloned(),
        mine: mine.cloned(),
        open: open
            .iter()
            .map(|t| ((*t).clone(), resolve::counts_under(tasks, &t.id).open))
            .collect(),
        shown,
        project: b.project.clone(),
        path,
        tags,
        about,
        mandate: b.mandate.clone(),
        rules: b.rules.clone(),
        decided,
        dropped,
        others: named.into_iter().take(far_shown).collect(),
        hidden,
        near,
        far,
    }
}

fn brief_json(b: &Briefing, r: &Brief, depth: Depth) -> serde_json::Value {
    let mut v = json!({
        "project": r.project,
        "path": r.path,
        "tags": r.tags,
        "brief": r.about,
        "pane": b.pane,
        "workspace": b.workspace,
        "mandate": r.mandate,
        "seat": r.seat.as_ref().map(|(s, mine)| json!({
            // `project` is kept as the key while the value has become a
            // scope — a project id or a worklist slug. The bar this change is
            // held to is that with no worklist running every output in this
            // tree is byte-for-byte what it was, and a renamed key breaks a
            // reader for the benefit of a word.
            "project": s.scope, "workspace": cmd_govern::room_of(&b.governors, &s.scope), "pane": s.pane, "mine": mine,
        })),
        "custodian": r.custodian,
        "task": r.mine.as_ref().map(|t| t.json()),
        "decisions": r.decided.iter().map(|(w, t)| json!({ "date": w, "text": t })).collect::<Vec<_>>(),
        "open": r.open.iter().map(|(t, _)| t.json()).collect::<Vec<_>>(),
        "here": r.near.iter().map(|s| s.json()).collect::<Vec<_>>(),
        "others": r.far.iter().map(|s| s.json()).collect::<Vec<_>>(),
        "rules": r.rules,
        "tree": r.own_tree,
    });
    // Written in rather than declared above, so a payload with no worklist
    // running is byte-for-byte the payload it always was. A `null` here would
    // be a key of its own kind of lie — the absence of a list is not a list
    // that is nowhere — and it would be paid for by every reader for the
    // benefit of none.
    if let Some(l) = &r.list {
        v["list"] = json!({ "list": l.list, "group": l.group, "of": l.of });
    }
    // The rotation in flight that names this pane, written in rather than
    // always-present: a brief with no handover is byte-for-byte the brief it
    // always was, and a null key is an absence dressed as a fact — the same
    // rule `list` above follows.
    if let Some(from) = &r.succeeding {
        v["succeeds"] = json!({ "pane": from, "pending": r.rotation_pending });
        if let Some(why) = &r.ending_failed {
            v["succeeds"]["failed"] = json!(why);
        }
    }
    // The same reckoning as the text, and gated the same way — a `--json`
    // caller that grew the payload without asking for it would be the mode
    // split defeated through the other door.
    if depth.session() {
        v["handbook"] = r
            .handbook
            .iter()
            .map(|(proj, text)| json!({ "project": proj, "text": text }))
            .collect::<Vec<_>>()
            .into();
        v["binds"] = r
            .parent_decided
            .iter()
            .map(|(w, t)| json!({ "date": w, "text": t }))
            .collect::<Vec<_>>()
            .into();
        v["names"] = r.refs.iter().map(|t| t.json()).collect::<Vec<_>>().into();
        v["body"] = r.mine.as_ref().map(|t| t.body.clone()).into();
    }
    v
}

/// A block of prose under a label, wrapped in nothing and capped at `max`.
///
/// Never trimmed quietly. Prose that stops at a line limit reads exactly like
/// prose that was written that short, and the difference is whatever the reader
/// is about to go and re-derive — the same failure [`MAX_RULES`] is written
/// against, and the cost of getting it wrong here is a whole session working
/// from half an overview.
fn block(out: &mut Vec<String>, p: &Paint, label: &str, text: &str, max: usize, more: &str) {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    for (i, l) in lines.iter().take(max).enumerate() {
        out.push(format!(
            "{} {}",
            p.dim(&util::pad(if i == 0 { label } else { "" }, 6)),
            l
        ));
    }
    if lines.len() > max {
        out.push(format!(
            "{} {}",
            p.dim(&util::pad("", 6)),
            p.dim(&format!("({} more lines — {more})", lines.len() - max))
        ));
    }
}

/// What the session-start hook adds, and nothing else ever gets.
///
/// The order is the order it is needed in: the work in hand, what it hangs
/// under, what it names, and last the standing reading for this project. An
/// agent that reads only the first block can still start.
fn session_lines(r: &Brief, p: &Paint) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    if let Some(t) = &r.mine {
        // The task's own statement of itself. This is the single thing an
        // arriving agent fetched first and most expensively, and it is already
        // in the hand of whoever spawned it.
        //
        // The two sections are capped differently, and that is the payload
        // split applied within one task: overview at [`MAX_TASK_LINES`],
        // details at [`MAX_DETAILS_LINES`] — what it is, whole-ish; how else
        // to read it, pointed.
        for (sec, max) in [("Overview", MAX_TASK_LINES), ("Details", MAX_DETAILS_LINES)] {
            if let Some(text) = crate::model::section_of(&t.body, sec) {
                out.push(String::new());
                block(&mut out, p, &sec.to_lowercase(), &text, max, &format!("wsp show {}", t.id));
            }
        }
        let own = crate::model::decisions(&t.body);
        if !own.is_empty() {
            out.push(String::new());
            for (i, (when, what)) in own.iter().enumerate() {
                out.push(format!(
                    "{} {} {what}",
                    p.dim(&util::pad(if i == 0 { "settled" } else { "" }, 6)),
                    p.dim(when)
                ));
            }
        }
        if !r.mine_log.is_empty() {
            out.push(String::new());
            // Bounded per entry, and never bounded quietly: an entry that
            // stops mid-sentence reads exactly like one that was written
            // short, so the count of shortened entries is printed with where
            // the rest lives. The `--json` payload carries the entries whole —
            // this is the text surface's economy, not the record's.
            let mut cut = 0;
            for (i, l) in r.mine_log.iter().enumerate() {
                let shown = util::truncate(l, MAX_LOG_ENTRY_CHARS);
                if shown.chars().count() < l.chars().count() {
                    cut += 1;
                }
                out.push(format!(
                    "{} {}",
                    p.dim(&util::pad(if i == 0 { "log" } else { "" }, 6)),
                    p.dim(&shown)
                ));
            }
            if cut > 0 {
                out.push(format!(
                    "{} {}",
                    p.dim(&util::pad("", 6)),
                    p.dim(&format!("{cut} of {} shortened · wsp show {}", r.mine_log.len(), t.id))
                ));
            }
        }
    }

    // The parent's decisions. Direction lands on a parent and the work happens
    // a sub-task at a time, so these are the constraints on the piece in hand
    // and they are not written down anywhere the piece itself can see.
    //
    // Abridged to the first sentence, on the precedent of `wsp project show`'s
    // decisions index: the sentence is the rule and what follows is the
    // argument for it, and six whole decisions on this store measured past
    // 3,000 tokens — d26 alone is ~600 — re-read by every request of every
    // session beneath it. The rule survives; the argument is one command away,
    // and the count below says how much went.
    if !r.parent_decided.is_empty() {
        let dropped = r.parent_decided.len().saturating_sub(MAX_PARENT_DECISIONS);
        out.push(String::new());
        let mut cut = 0;
        for (i, (when, what)) in r.parent_decided.iter().skip(dropped).enumerate() {
            let lead = util::truncate(util::first_sentence(what), MAX_BIND_LEAD);
            if lead.chars().count() < what.trim_end().chars().count() {
                cut += 1;
            }
            out.push(format!(
                "{} {} {lead}",
                p.dim(&util::pad(if i == 0 { "binds" } else { "" }, 6)),
                p.dim(when)
            ));
        }
        if dropped > 0 {
            let id = r.parent.as_ref().map(|t| t.id.as_str()).unwrap_or("");
            out.push(format!(
                "{} {}",
                p.dim(&util::pad("", 6)),
                p.dim(&format!("{dropped} earlier · wsp show {id}"))
            ));
        }
        if cut > 0 {
            let id = r.parent.as_ref().map(|t| t.id.as_str()).unwrap_or("");
            out.push(format!(
                "{} {}",
                p.dim(&util::pad("", 6)),
                p.dim(&format!("{cut} of {} abridged · wsp show {id}", r.parent_decided.len()))
            ));
        }
    }

    // What the prose above names. Enough to know which of these is worth
    // opening, and no more — answering "what is robustness-017" with the whole of
    // robustness-017 would be the fetching this replaces, done eagerly and for
    // every id rather than the one that mattered.
    for (i, t) in r.refs.iter().enumerate() {
        if i == 0 {
            out.push(String::new());
        }
        out.push(format!(
            "{} {}  {} {}",
            p.dim(&util::pad(if i == 0 { "names" } else { "" }, 6)),
            p.dim(&t.id),
            p.dim(&util::pad(t.status().as_str(), 7)),
            util::truncate(&t.title, 52)
        ));
    }

    // The handbook, last, and the whole chain of it. It is the part that is the
    // same for every agent in this project rather than particular to this task,
    // which is why it reads after the work rather than before it — and why it
    // is a pointer to the repository's own documentation rather than a copy of
    // it.
    let mut left = MAX_HANDBOOK_LINES;
    let last = r.handbook.len().saturating_sub(1);
    for (i, (proj, text)) in r.handbook.iter().enumerate() {
        if left == 0 {
            break;
        }
        let max = if i == last { left } else { MAX_HANDBOOK_LEAD };
        out.push(String::new());
        block(&mut out, p, "read", text, max, &format!("wsp project show {proj} --handbook"));
        left = left.saturating_sub(text.lines().count().min(max));
    }
    out
}

/// The brief as text, one line per element and nothing printed.
fn brief_lines(r: &Brief, p: &Paint, depth: Depth) -> Vec<String> {
    let terse = depth == Depth::Terse;
    let mut out: Vec<String> = Vec::new();
    let mut row = |label: &str, body: String| out.push(format!("{} {}", p.dim(&util::pad(label, 6)), body));

    // Where you are, and what this place is for.
    match &r.project {
        Some(_) => {
            let mut head = r.path.join("/");
            if !r.tags.is_empty() {
                head.push_str(&format!("  {}", p.dim(&r.tags.join(" "))));
            }
            row("where", head);
            if !r.about.trim().is_empty() {
                row("", p.dim(&util::truncate(r.about.trim(), 68)));
            }
        }
        None => row("where", p.dim("no project resolved for this pane").to_string()),
    }
    // Said before the work rather than after it: what it changes is how you
    // commit, and an agent reads a brief once and commits hours later.
    //
    // **"and it is the whole project" is `core-042`, and it is there to stop a
    // reach rather than to be informative.** A tree sits at
    // `<checkout>/.worktrees/<task>`, so an agent that reads *the root* as the
    // project's recorded root walks up out of its own tree — and three opencodes
    // did, one of them editing `src/cmd_task.rs` in the shared checkout before
    // anyone noticed. Every tracked file is already here, including the
    // `README.md` the handbook sends them to. Six words against that.
    //
    // **And it is enforced now, not only said** (`wsp-123`, Ed on review,
    // 2026-09-27: the trunk is not writeable, permanently — work happens in
    // worktrees). Every root of the project is an `external_directory` deny,
    // so the trunk and the trees beside this one are refused without anybody
    // being asked; the trunk is read from here, through git.
    if r.own_tree.is_some() {
        row(
            "tree",
            p.dim("your own, and it is the whole project — commit freely; `wsp land` puts it on the trunk, and `git show master:<path>` reads it")
                .to_string(),
        );
    }

    // Standing direction, before anything about the work itself. An agent
    // under a mandate is allowed to pick up the next piece without being asked
    // again, and behaviour that has to happen unprompted cannot wait for the
    // agent to go looking for permission.
    if let Some(m) = &r.mandate {
        row("mandate", format!("{}  {}", p.bold(m), p.dim("take work here without asking")));
    }

    // Who is coordinating, when anybody is. Beside the mandate because the two
    // are the same kind of fact — what this pane is allowed to do without
    // asking, and who it is working alongside — and above the work for the same
    // reason. Fifteen tokens, and only in a session that has a seat above it:
    // it changes what an agent does with a question, from sitting on it until
    // somebody looks at a panel to raising it at another agent that is watching
    // for exactly that.
    // The custodian's own line comes first and replaces the seat line, because
    // for that one session they are the same fact said from opposite ends —
    // "somebody is coordinating here" and "it is you" — and the second is the
    // one that changes what the reader does next. It names every slot the
    // workspace holds, where the seat line can only name the nearest, which for
    // an agent answering for `wsp` while standing in `robustness` is the wrong
    // half of its job.
    //
    // And, on the same line and only on it, whether anything on this machine is
    // going to wake this seat. `wsp-145`: a governor's job is mostly waiting to
    // be handed something, and the thing that hands it something is a daemon
    // that had stopped six days earlier while every reading of the machine went
    // on saying it was busy. An ordinary agent is told nothing — this line
    // draws only under a seat, and the brief is re-read on every request of
    // every session.
    let awake = r.daemon.as_ref().map(|d| format!("  {}", p.yellow(d)));
    if let Some(scope) = &r.custodian {
        // A seat on a worklist says where the run is, because for the agent
        // holding it that *is* the position: which group is being waited on is
        // the whole state of the thing in its hands, and it is the one fact
        // that changes under it while it is not looking. A seat on a project
        // has no such number and the line is what it always was.
        let at = r.seat_at.as_ref().map(|pos| match pos.at {
            Some(at) => format!("group {at} of {}", pos.of),
            None => "every group finished".to_string(),
        });
        row(
            "seat",
            match &at {
                Some(at) => format!(
                    "{}  {}  {}{}",
                    p.bold(&format!("governor of {scope}")),
                    p.dim(at),
                    p.dim("yours to sequence, direct, review · wsp flag --seat is your inbox"),
                    awake.clone().unwrap_or_default(),
                ),
                None => format!(
                    "{}  {}{}",
                    p.bold(&format!("governor of {scope}")),
                    p.dim("yours to sequence, direct, review · wsp flag --seat is your inbox"),
                    awake.clone().unwrap_or_default(),
                ),
            },
        );
    } else if let Some((s, mine)) = &r.seat {
        row(
            "seat",
            format!(
                "{}  {}{}",
                p.bold(&s.scope),
                match mine {
                    true => p.dim("yours — wsp flag --seat is your inbox"),
                    false => p.dim("coordinating here · wsp flag <id> reaches it"),
                },
                awake.unwrap_or_default(),
            ),
        );
    }

    // The pane this one replaced, and what is becoming of it. Beside the seat
    // line because it is the same fact read from the incoming end; drawn only
    // while a handover record names this pane, which is one pane on the
    // machine. **Every branch is a report, never an instruction.** It used to
    // say `run wsp despawn --pane <from>`, and Claude Code's auto-mode
    // classifier refused that as one agent ending another's workload. It was
    // right to: a line in a brief is not authority (`wsp-128`). wsp ends the
    // predecessor from the predecessor's own `--rotate`, and when that fails
    // the reader is told so and who can act. It is not asked to act.
    if let Some(from) = &r.succeeding {
        row(
            "handover",
            match (r.rotation_pending, &r.ending_failed) {
                (true, _) => p.dim(&format!(
                    "the seat moves to you once your first turn starts · wsp then ends {from}, who you replaced - not yours to do"
                )),
                (false, None) => p.dim(&format!("wsp is ending {from}, who you replaced - not yours to do")),
                (false, Some(why)) => format!(
                    "{}  {}",
                    p.yellow(&format!("{from}, who you replaced, is still running")),
                    p.dim(&format!("wsp could not end it: {why} · a person has to")),
                ),
            },
        );
    }

    // What you are on. `—` rather than silence: an agent with no task is a
    // thing worth noticing, since claiming one is the first move.
    match &r.mine {
        Some(t) => {
            row(
                "you",
                format!("{}  {}  {}", p.bold(&t.id), p.dim(t.status().as_str()), util::truncate(&t.title, 52)),
            );
            // What it is part of. Direction lands on a parent and the work
            // happens a sub-task at a time, so the piece in hand is rarely the
            // reason it is being done.
            if let Some(parent) = &r.parent {
                row(
                    "under",
                    format!("{}  {}", p.dim(&parent.id), p.dim(&util::truncate(&parent.title, 52))),
                );
            }
            if r.under_mine > 0 {
                row("", p.dim(&format!("{} sub-task(s) open beneath it", r.under_mine)));
            }
            // What it is part of the *other* way. `under` is the backlog's
            // shape and this is the run's: which list picked this task up and
            // which group of it, so an agent knows there is a barrier behind it
            // and — with the seat line above — who is standing at it. Nothing
            // at all on a night with no worklist running, which is every night
            // so far.
            if let Some(l) = &r.list {
                row(
                    "list",
                    format!(
                        "{}  {}",
                        p.bold(&l.list),
                        p.dim(&format!("group {} of {}", l.group, l.of))
                    ),
                );
            }
            // The files this row names outside its own tree, which is a
            // different `refs` from the `names` block below: those are tasks
            // the prose mentions, these are paths on the task itself.
            //
            // **`core-042`, and it is the half that makes the mechanism work.**
            // `cmd_spawn::reach` turns this list into the one place a spawned
            // agent may reach outside its worktree without a prompt — and a
            // path it may reach but is never told about is no use at all.
            // Driven: `worklist-surface`'s five members all need one spec whose
            // path lives in a *parent's* prose, so both agents of group 1 read
            // their whole brief and never learned the file existed. It is paid
            // for by every request of the session, so it is one line per path
            // and nothing at all on the rows — most of them — that name none.
            for (i, at) in r.mine.iter().flat_map(|t| t.refs.iter()).enumerate() {
                row(if i == 0 { "files" } else { "" }, p.dim(at).to_string());
            }
        }
        // A custodian holding nothing is not an agent that has failed to claim
        // anything: it is an agent doing its job. The default line below tells
        // it to go and claim something, which is the one instruction that would
        // take the position apart — the seat that borrowed a task to stand on
        // is exactly what robustness-048 was filed about.
        None if r.custodian.is_some() => row(
            "you",
            p.dim("holding nothing, which is the job — the work below is for the agents you start")
                .to_string(),
        ),
        None if r.mandate.is_some() => {
            row("you", p.dim("nothing claimed").to_string());
            // The loop, spelled out on the one line where it is actionable.
            match r.open.first() {
                Some((t, _)) => row(
                    "next",
                    format!(
                        "{}  {}  {}",
                        p.bold(&t.id),
                        util::truncate(&t.title, 44),
                        p.dim("wsp claim it")
                    ),
                ),
                None => row("next", p.dim("nothing actionable here — the mandate is done").to_string()),
            }
        }
        None => row("you", p.dim("nothing claimed — wsp claim <id>, or wsp add \"…\" first").to_string()),
    }

    // What is already settled, before the list of things to pick up. A decision
    // is a constraint on what may be taken, so it belongs in front of the
    // backlog rather than after it — claude-92's argument for `project show`,
    // and it applies here for the same reason.
    //
    // The whole chain, not just this project: a decision made on `wsp` binds
    // what is picked up in `data`, exactly as a tag does, and for the same
    // reason — the work is inside it.
    for (i, (when, what)) in
        r.decided.iter().skip(r.decided.len().saturating_sub(MAX_DECISIONS)).enumerate()
    {
        row(
            if i == 0 { "decided" } else { "" },
            format!("{} {}", p.dim(when), util::truncate(what, 56)),
        );
    }
    if r.dropped > 0 {
        let leaf = r.project.as_deref().unwrap_or("");
        row("", p.dim(&format!("{} earlier · wsp project show {leaf}", r.dropped)).to_string());
    }

    if r.shown > 0 {
        for (i, (t, under)) in r.open.iter().take(r.shown).enumerate() {
            let prio = crate::cmd_task::paint_prio(p, t.priority());
            let kids = if *under > 0 { p.dim(&format!("  ({under} open)")) } else { String::new() };
            row(
                if i == 0 { "open" } else { "" },
                format!(
                    "{}  {} {} {}{}",
                    p.dim(&t.id),
                    p.dim(&util::pad(t.status().as_str(), 7)),
                    prio,
                    util::truncate(&t.title, 46),
                    kids
                ),
            );
        }
        if r.open.len() > r.shown {
            row("", p.dim(&format!("{} more · wsp ls", r.open.len() - r.shown)));
        }
    }

    // Who can reach the files you are about to edit. First, and in the colour
    // that means a decision, because this is the line that would have caught
    // two agents in one checkout this morning.
    for (i, o) in r.near.iter().enumerate() {
        let held = match o.since {
            Some(secs) if secs > 0 => format!(" · {}", util::duration_human(secs)),
            _ => String::new(),
        };
        row(
            if i == 0 { "here" } else { "" },
            format!(
                "{}  {}{}",
                p.yellow(&util::pad(&util::truncate(&o.workspace, 12), 12)),
                util::truncate(&o.name(), 40),
                p.dim(&format!("  {}{held}", o.relation.as_str()))
            ),
        );
    }

    for (i, o) in r.others.iter().enumerate() {
        let flag = if o.needs_you() { p.yellow("  ← wants a decision") } else { String::new() };
        row(
            if i == 0 { "others" } else { "" },
            format!(
                "{}  {}{}",
                p.dim(&util::pad(&util::truncate(&o.workspace, 12), 12)),
                p.dim(&util::truncate(&o.name(), 40)),
                flag
            ),
        );
    }
    // The quiet ones get a number, not names. A shell that has been standing
    // in a directory since Tuesday is worth knowing the count of and nothing
    // more.
    if r.hidden > 0 {
        row(
            if r.others.is_empty() { "others" } else { "" },
            p.dim(&format!("{} more · wsp overlap", r.hidden)).to_string(),
        );
    }

    // The payload, between the roster and the rules: after everything that says
    // where this is and who else is here, before the standing text that is the
    // same in every session on the machine.
    if depth.session() {
        out.extend(session_lines(r, p));
    }

    // The rules, last and in full — unless the caller says it has them.
    //
    // This is the block `--terse` exists for. It is the same text in every
    // session on the machine and 59% of a brief in a claimed pane even after
    // two cuts, and the hook that delivers it has already run by the time
    // anybody can type `wsp brief` — so every later brief in that session pays
    // for text sitting a few thousand tokens up its own context. There were 35
    // of those across the sessions this was measured on.
    //
    // Named rather than dropped. A brief that quietly stops before the rules
    // reads like a store with no rules in it, which is the failure MAX_RULES is
    // written against, arriving by a different door.
    match &r.rules {
        Some(_) if terse => {
            out.push(String::new());
            out.push(p.dim("(rules omitted — wsp brief, without --terse)"));
        }
        Some(text) => {
            out.push(String::new());
            for line in text.lines() {
                out.push(p.dim(line));
            }
        }
        None => {}
    }
    out
}

/// The session brief as one block of plain text, for a caller that is going to
/// *hand* it to somebody rather than print it.
///
/// Same composition and same rendering as the hook's — deliberately, and it is
/// the point of the split above. A second, shorter brief written for spawned
/// agents would be a second contract: every cap, every ordering argument and
/// every "before adding this, multiply" in this file would have to be re-made
/// for it, and the two would drift on the first change that only remembered
/// one. What `spawn` hands an unbriefed kind is therefore the same text a
/// Claude Code reads out of `SessionStart`, composed for the seat instead of
/// for this process.
///
/// Unpainted, because it is being embedded in a work order rather than written
/// to a terminal: escape codes reaching an agent's prompt are noise it has to
/// read past, and `Paint::new()` would decide by whether *`spawn`* had a tty.
pub(crate) fn session_text(b: &Briefing) -> String {
    brief_lines(&compose(b), &Paint::plain(), Depth::Session).join("\n")
}

pub fn brief(store: &Store, args: &Args) -> i32 {
    let b = Briefing::live(store, args);
    let r = compose(&b);
    let depth = Depth::of(args);

    // Before the rendering because it is a fact about the pane, not about how
    // the brief is printed: `--json` looks the same to herdr as a person does.
    if let Some(found) = r.looking {
        cmd_agent::say_looking(store, &b.world.panes, r.project.as_deref(), found);
    }

    match args.json() {
        true => println!(
            "{}",
            serde_json::to_string_pretty(&brief_json(&b, &r, depth)).unwrap_or_default()
        ),
        false => {
            for l in brief_lines(&r, &Paint::new(), depth) {
                println!("{l}");
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    fn plain() -> Paint {
        Paint::new()
    }

    fn task(id: &str, title: &str, project: Option<&str>, status: &str) -> Task {
        let mut t = Task::new(title, id);
        t.project = project.map(str::to_string);
        t.status_raw = status.to_string();
        t
    }

    fn pane(id: &str, ws: &str, cwd: &str, agent: &str, title: &str) -> herdr::Pane {
        herdr::Pane {
            pane_id: id.to_string(),
            workspace_id: ws.to_string(),
            cwd: cwd.to_string(),
            agent: agent.to_string(),
            agent_status: if agent.is_empty() { String::new() } else { "working".into() },
            title: title.to_string(),
            ..Default::default()
        }
    }

    /// A store with a project, a claimed pane, a backlog, and a second agent
    /// standing in the same tree.
    fn briefing() -> Briefing {
        let mut wsp = Project::new("wsp");
        wsp.roots = vec!["/home/ed/claude/wsp".into()];
        wsp.tags = vec!["rust".into()];
        wsp.brief = "the control plane".into();
        wsp.body = "## DECISIONS\n- 2026-08-16 the store is the only writer\n".into();
        let mut robust = Project::new("robustness");
        robust.parent = Some("wsp".into());

        let mut bindings = std::collections::BTreeMap::new();
        bindings.insert("w1:p1".to_string(), json!({ "task_id": "t-001" }));
        bindings.insert("w2:p1".to_string(), json!({ "task_id": "t-002" }));

        Briefing {
            world: overlap::World {
                panes: vec![
                    pane("w1:p1", "w1", "/home/ed/claude/wsp", "claude", "mine"),
                    pane("w2:p1", "w2", "/home/ed/claude/wsp", "claude", "somebody else"),
                    // A shell somewhere else entirely: context, and quiet.
                    pane("w9:p1", "w9", "/home/ed/music", "", "zsh"),
                ],
                workspaces: vec![
                    herdr::Workspace { id: "w1".into(), label: "mine".into(), ..Default::default() },
                    herdr::Workspace { id: "w2".into(), label: "theirs".into(), ..Default::default() },
                    herdr::Workspace { id: "w9".into(), label: "music".into(), ..Default::default() },
                ],
                tasks: vec![
                    task("t-001", "the task in hand", Some("wsp"), "doing"),
                    task("t-002", "somebody else's", Some("wsp"), "doing"),
                    task("t-003", "next up", Some("robustness"), "todo"),
                    task("t-004", "and after that", Some("wsp"), "todo"),
                    task("t-005", "unfiled", None, "todo"),
                ],
                index: crate::resolve::Index::new(vec![wsp, robust]),
                pins: std::collections::BTreeMap::new(),
                bindings,
                claims: std::collections::BTreeMap::new(),
                agents_held: std::collections::BTreeMap::new(),
            },
            mandate: Some("wsp".into()),
            // Nothing running, which is the baseline every line below is
            // measured against.
            lists: crate::worklist::Running::default(),
            seat_at: None,
            daemon: None,
            // Nobody coordinating. The ordinary state, and the baseline the
            // seat tests below add a seat to: the brief has to be identical
            // without one.
            governors: std::collections::BTreeMap::new(),
            rules: Some("commit through your own index".into()),
            project: Some("wsp".into()),
            pane: Some("w1:p1".into()),
            workspace: Some("w1".into()),
            // No handover in flight. The ordinary state, and the baseline the
            // rotation tests below add a record to.
            incoming: None,
            ending_failed: None,
            cwd: Some("/home/ed/claude/wsp".into()),
        }
    }

    /// The brief is re-read on every request of every session, so the test that
    /// matters about the seat line is the one about it not being there. No
    /// governor anywhere is the normal state and it has to cost nothing —
    /// literally nothing, byte for byte.
    #[test]
    fn a_brief_with_nobody_coordinating_says_nothing_about_a_seat() {
        let text = brief_lines(&compose(&briefing()), &plain(), Depth::Normal).join("\n");
        assert!(!text.contains("seat"), "{text}");
    }

    /// The same test, and the same bar, for the worklist line: **with nothing
    /// running the brief is byte-for-byte the brief it was.** A worklist is a
    /// thing a handful of nights a month have and every other session does not,
    /// and this output is paid for on every request of every one of them.
    #[test]
    fn a_brief_with_nothing_running_says_nothing_about_a_list() {
        let r = compose(&briefing());
        assert!(r.list.is_none(), "nothing is running, so this pane is in no group");
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(!text.contains("list"), "{text}");
    }

    /// And when there is one, it is one line: which list picked this task up
    /// and which group of it.
    ///
    /// The group is the *membership* rather than where the run has got to —
    /// what an agent wants to know is that there is a barrier behind the work
    /// in its hands and who is standing at it, and both of those are facts
    /// about its own group. Beside `under`, because the two are the same
    /// question asked of the two structures a task can be in: the backlog says
    /// what this work is for, the run says what is waiting on it.
    #[test]
    fn a_task_in_a_running_list_is_told_which_group_it_is_in() {
        let mut r = compose(&briefing());
        r.list = Some(crate::worklist::Placing { list: "batch".into(), group: 2, of: 4 });
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("list"), "{text}");
        assert!(text.contains("batch"), "{text}");
        assert!(text.contains("group 2 of 4"), "{text}");
    }

    /// A governor seated on a worklist is told where the run is, and that is
    /// the seat line naming the list and its position instead of a project.
    ///
    /// For the agent holding it that *is* the position: which group is being
    /// waited on is the whole state of the thing in its hands, and it is the
    /// one fact that moves under it while it is not looking. A seat on a
    /// project has no such number and the line is unchanged.
    #[test]
    fn a_governor_of_a_list_is_told_where_the_run_has_got_to() {
        let mut r = compose(&briefing());
        r.custodian = Some("batch".into());
        // `Settled` because that is what a *reading* verb asks — see
        // `worklist::running_position`, which is where this comes from live.
        // The two members lists are the barrier's and the sweep's business and
        // the seat line reads neither — nor the barriers crossed without a
        // verdict, which is the run's own history and not this agent's state.
        let at = |at| crate::worklist::Position {
            at,
            of: 5,
            members: Vec::new(),
            passed: Vec::new(),
            slipped: Vec::new(),
            unwritten: Vec::new(),
            reading: crate::worklist::Reading::Settled,
        };
        r.seat_at = Some(at(Some(2)));
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("governor of batch"), "{text}");
        assert!(text.contains("group 2 of 5"), "{text}");

        // Finished, which is a different thing from the list being `done`:
        // every group has passed and somebody has to say the run is over.
        r.seat_at = Some(at(None));
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("every group finished"), "{text}");

        // And a seat on a project says what it always said.
        r.seat_at = None;
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("governor of batch"), "{text}");
        assert!(!text.contains("group"), "{text}");
    }

    /// `wsp-145`. A governor's job is mostly waiting to be handed something, and
    /// what hands it something is the daemon's unattended pass — which had been
    /// gone for six days while every reading of the machine said it was busy,
    /// and which nothing on this output said. On the seat line, because a seat
    /// is the one reader for whom it is a fact about its own job.
    ///
    /// And *only* on the seat line: the brief is re-read on every request of
    /// every session, so an ordinary agent must still pay nothing for a fact
    /// that never concerns it. Both halves are asserted, because a change that
    /// drew it everywhere would be just as wrong as one that drew it nowhere.
    #[test]
    fn a_seat_is_told_when_nothing_on_this_machine_is_going_to_wake_it() {
        let dead = crate::daemon::loud(Some(&[]), true).expect("a machine with a backend and no daemon");

        let mut b = briefing();
        b.daemon = Some(dead.clone());
        let mut r = compose(&b);

        // A custodian, which is the reading whose seat line is the longest, so
        // it is where an appended clause is most likely to be dropped.
        r.custodian = Some("batch".into());
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("no wsp daemon"), "the custodian's seat line lost it: {text}");
        assert!(
            text.contains("yours to sequence, direct, review"),
            "the clause was put on instead of beside: {text}"
        );

        // A seat above this pane rather than holding the slot itself.
        r.custodian = None;
        b.governors.insert("wsp".into(), json!({ "workspace": "w9", "host": util::hostname() }));
        let r = compose(&b);
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("no wsp daemon"), "the seat line above this pane lost it: {text}");

        // And an ordinary agent — no seat anywhere above it — is told nothing.
        let mut plain_b = briefing();
        plain_b.governors.clear();
        plain_b.daemon = Some(dead);
        let r = compose(&plain_b);
        assert!(r.seat.is_none() && r.custodian.is_none(), "the fixture has a seat on it");
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(!text.contains("no wsp daemon"), "an ordinary agent is paying for a seat\'s fact: {text}");
    }

    /// And when there is one, it is one line naming the project rather than the
    /// workspace: an agent does not address a raised hand at a herdr id, it
    /// raises it on a task and the chain decides where that lands.
    #[test]
    fn a_seat_above_this_pane_is_named_in_one_line() {
        let mut b = briefing();
        b.governors.insert(
            "wsp".into(),
            json!({ "workspace": "w9", "host": util::hostname() }),
        );
        let r = compose(&b);
        assert_eq!(r.seat.as_ref().map(|(s, mine)| (s.scope.as_str(), *mine)), Some(("wsp", false)));
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("wsp flag <id> reaches it"), "{text}");
    }

    /// The seat reading its own brief is told it is the seat, and told where
    /// its inbox is — which is the one thing it cannot work out from a pane it
    /// is already sitting in.
    #[test]
    fn the_seat_reading_its_own_brief_is_told_it_is_the_seat() {
        let mut b = briefing();
        b.governors.insert(
            "wsp".into(),
            json!({ "workspace": "w1", "host": util::hostname() }),
        );
        let r = compose(&b);
        assert_eq!(r.seat.as_ref().map(|(_, mine)| *mine), Some(true), "w1 is this pane's workspace");
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("wsp flag --seat"), "{text}");
    }

    /// A custodian's brief is about the position rather than about a task, and
    /// the line that matters most is the one it must *not* draw.
    ///
    /// An agent in a slot holding nothing is doing its job. The default line
    /// for an empty pair of hands tells it to go and claim something, and a
    /// custodian that claims work is exactly what robustness-048 was filed about
    /// — the seat on the night this came from borrowed a task to have somewhere
    /// to stand, and every surface then described it as that task's agent.
    #[test]
    fn a_custodian_is_told_what_it_is_answerable_for_rather_than_to_go_and_claim() {
        let mut b = briefing();
        // The governor of `wsp` — the project, where this pane happens to be
        // standing in `robustness` under it. The seat line walks up from where
        // it stands; this line says what it answers for, and only it can.
        b.governors.insert("wsp".into(), json!({ "workspace": "w1", "host": util::hostname() }));
        b.project = Some("robustness".into());
        // Holding nothing, which is what a custodian holds.
        b.world.bindings.clear();
        b.world.claims.clear();

        let r = compose(&b);
        assert_eq!(r.custodian.as_deref(), Some("wsp"));
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("governor of wsp"), "what it is, in the words a person uses: {text}");
        assert!(text.contains("wsp flag --seat"), "its inbox: {text}");
        assert!(!text.contains("wsp claim <id>"), "a custodian was sent to claim work: {text}");
    }

    /// A rotation's successor is introduced by its brief as the seat it is
    /// about to hold, read off the store, because the typed work order is the
    /// channel that can be dropped.
    ///
    /// Between the moment a rotation seats the successor and the moment the
    /// slot moves, nothing else in wsp says this pane is anything but a worker:
    /// `governs` answers for the predecessor. The record is what bridges that
    /// window, which is why this test drives it through `Briefing.incoming`,
    /// the same read a session-start hook makes live.
    #[test]
    fn a_rotation_names_the_successor_the_seat_before_the_slot_has_moved() {
        let mut b = briefing();
        b.governors.clear();
        b.incoming = Some(("batch".into(), "w8M:p1".into()));

        let r = compose(&b);
        assert_eq!(r.custodian.as_deref(), Some("batch"), "the record says what this pane is about to hold");
        assert!(r.rotation_pending, "the slot has not moved yet - that is the whole window");
        assert_eq!(r.succeeding.as_deref(), Some("w8M:p1"));
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("once your first turn starts"), "{text}");

        // And with no rotation anywhere, none of it draws — the same bargain
        // the seat line makes, on an output paid for by every request.
        let r = compose(&briefing());
        assert!(r.succeeding.is_none());
        assert!(!brief_lines(&r, &plain(), Depth::Normal).join("\n").contains("handover"));
    }

    /// **`wsp-128`.** The brief told the successor to `wsp despawn --pane` its
    /// predecessor, and Claude Code's auto-mode classifier refused that as
    /// interfering with another agent's workload. In every state the handover
    /// record can be in, the brief now reports what wsp is doing about the
    /// predecessor, and never asks the successor to end a pane.
    #[test]
    fn a_successors_brief_never_asks_it_to_end_another_pane() {
        let moved = || {
            let mut b = briefing();
            b.governors.insert("batch".into(), json!({ "workspace": "w1", "host": util::hostname() }));
            b
        };
        let mut pending = briefing();
        pending.governors.clear();
        let mut failed = moved();
        failed.ending_failed = Some("`wsp despawn --pane w8M:p1` exited 1 - see handover.log".into());

        for (state, mut b) in [("pending", pending), ("moved", moved()), ("failed", failed)] {
            b.incoming = Some(("batch".into(), "w8M:p1".into()));
            let r = compose(&b);
            let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
            let handover = text.lines().find(|l| l.contains("handover")).unwrap_or_default().to_string();
            assert!(handover.contains("w8M:p1"), "{state}: it says who: {text}");
            assert!(!handover.contains("run `wsp despawn"), "{state}: {handover}");
            assert!(
                handover.contains("not yours to do") || handover.contains("a person has to"),
                "{state}: it says whose it is: {handover}"
            );
        }

        let mut b = moved();
        b.incoming = Some(("batch".into(), "w8M:p1".into()));
        b.ending_failed = Some("the pty would not close".into());
        let r = compose(&b);
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("still running") && text.contains("the pty would not close"), "{text}");
        assert!(!text.contains("once your first turn starts"), "that condition is behind it: {text}");
    }

    /// **`wsp-128`, the seat line.** A compound seat has no herdr workspace,
    /// and the brief asked what a pane governs only when it had one. A
    /// successor that governors.json named as the seat, and that `wsp govern`
    /// agreed was the seat, read `compound · coordinating here` in its brief.
    #[test]
    fn a_seat_with_no_herdr_workspace_reads_as_the_seat_it_holds() {
        let mut b = briefing();
        b.workspace = None;
        b.pane = Some("cpd-106".into());
        b.governors.clear();
        b.governors.insert(
            "batch".into(),
            json!({ "workspace": "cpd-106", "pane": "cpd-106", "host": util::hostname() }),
        );
        let r = compose(&b);
        assert_eq!(r.custodian.as_deref(), Some("batch"));
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("governor of batch"), "{text}");
    }

    /// The three facts a session cannot start without, in the order it needs
    /// them: where this is, what direction stands here, and what is in hand.
    #[test]
    fn a_brief_leads_with_where_you_are_and_what_you_are_holding() {
        let b = briefing();
        let r = compose(&b);
        assert_eq!(r.path, ["wsp"].map(String::from).to_vec());
        assert_eq!(r.mine.as_ref().map(|t| t.id.as_str()), Some("t-001"));

        // A project inside another shows the chain rather than the leaf. Where
        // you are is the whole path down: the ancestors are what carry the tags
        // and the decisions that bind here.
        let mut child = briefing();
        child.project = Some("robustness".into());
        assert_eq!(compose(&child).path, ["wsp", "robustness"].map(String::from).to_vec(), "root first");

        let text = brief_lines(&r, &plain(), Depth::Terse).join("\n");
        let line = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle} in:\n{text}"));
        assert!(line("where") < line("mandate"), "direction after the place it applies to");
        assert!(line("mandate") < line("you"), "…and before the work under it");
        assert!(line("you") < line("decided"), "what is settled comes before the backlog");
        assert!(line("decided") < line("open"), "a decision constrains what may be taken");
        assert!(text.contains("the task in hand"), "{text}");
        assert!(text.contains("take work here without asking"), "{text}");
    }

    /// A row that names a file outside the tree says so in the brief, because
    /// a path the agent may reach and is never told about is no mechanism.
    ///
    /// `core-042`. `cmd_spawn::reach` turns `Task::refs` into the one place a
    /// spawned agent may go outside its worktree without a prompt, and both
    /// agents of `worklist-surface`'s group 1 read their whole brief without
    /// learning the spec every member of that list needs even existed — its
    /// path lives in a parent's prose. Nothing at all on the rows that name no
    /// file, which is most of them, because this is paid on every request.
    #[test]
    fn the_files_a_row_names_outside_its_tree_are_in_the_brief_that_may_reach_them() {
        let mut b = briefing();
        let quiet = brief_lines(&compose(&b), &plain(), Depth::Terse).join("\n");
        assert!(!quiet.contains("files"), "a row naming none says nothing: {quiet}");

        if let Some(t) = b.world.tasks.iter_mut().find(|t| t.id == "t-001") {
            t.refs = vec!["~/claude/.scratch/worklist-ui-001.html".into()];
        }
        let text = brief_lines(&compose(&b), &plain(), Depth::Terse).join("\n");
        assert!(text.contains("~/claude/.scratch/worklist-ui-001.html"), "{text}");
        let line = |needle: &str| {
            text.lines().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle} in:\n{text}"))
        };
        assert!(line("you") < line("files"), "it is a fact about the row in hand: {text}");
    }

    /// The backlog is the subtree, not the exact project. Scoped exactly, a
    /// mandate on `wsp` briefed you on nothing from `robustness` while `next`
    /// handed you a task out of it — one question with two answers.
    #[test]
    fn the_backlog_is_the_subtree_and_never_what_is_already_in_hand() {
        let r = compose(&briefing());
        let ids: Vec<&str> = r.open.iter().map(|(t, _)| t.id.as_str()).collect();
        assert!(ids.contains(&"t-003"), "a child project's work is this project's backlog: {ids:?}");
        assert!(!ids.contains(&"t-001"), "what you are holding is not something to pick up");
        assert!(!ids.contains(&"t-005"), "the inbox is not inside a project");
        assert!(ids.contains(&"t-002"), "another agent's task is still open work here");
    }

    /// A pane belonging to no project is a shorter brief, not an error. The
    /// header of this file promises it and nothing tested it.
    #[test]
    fn a_pane_with_no_project_gets_a_shorter_brief() {
        let mut b = briefing();
        b.project = None;
        b.mandate = None;
        let r = compose(&b);
        assert!(r.path.is_empty());
        assert!(r.decided.is_empty(), "no project, so no decisions bind");

        let text = brief_lines(&r, &plain(), Depth::Terse).join("\n");
        assert!(text.contains("no project resolved for this pane"), "{text}");
        // With no project the backlog is the inbox, which is what unfiled work
        // is: work nobody has said where it belongs.
        assert_eq!(r.open.iter().map(|(t, _)| t.id.as_str()).collect::<Vec<_>>(), ["t-005"]);
    }

    /// A herdr that is not answering is a shorter brief too. The durable half
    /// — where you are, what you claimed, what is open — is the store's, and
    /// none of it needs a socket.
    #[test]
    fn no_herdr_costs_the_brief_the_other_agents_and_nothing_else() {
        let mut b = briefing();
        b.world.panes.clear();
        b.world.workspaces.clear();
        let r = compose(&b);

        assert!(r.near.is_empty(), "nobody reported, so nobody is standing here");
        assert!(r.far.is_empty());
        assert_eq!(r.hidden, 0);

        let text = brief_lines(&r, &plain(), Depth::Terse).join("\n");
        assert!(text.contains("the task in hand"), "the claim is the store's:\n{text}");
        assert!(text.contains("next up"), "and so is the backlog:\n{text}");
        assert!(
            !text.lines().any(|l| l.starts_with("here")),
            "no warning about a tree nobody is in:\n{text}"
        );
    }

    /// A store with nothing in it is the shortest brief, and still not an
    /// error. This is the first command on a fresh machine.
    #[test]
    fn an_empty_store_is_the_shortest_brief_rather_than_a_failure() {
        let b = Briefing {
            world: overlap::World {
                panes: Vec::new(),
                workspaces: Vec::new(),
                tasks: Vec::new(),
                index: crate::resolve::Index::new(Vec::new()),
                pins: std::collections::BTreeMap::new(),
                bindings: std::collections::BTreeMap::new(),
                claims: std::collections::BTreeMap::new(),
                agents_held: std::collections::BTreeMap::new(),
            },
            mandate: None,
            lists: crate::worklist::Running::default(),
            daemon: None,
            seat_at: None,
            governors: std::collections::BTreeMap::new(),
            rules: None,
            project: None,
            pane: None,
            workspace: None,
            incoming: None,
            ending_failed: None,
            cwd: None,
        };
        let r = compose(&b);
        let out = brief_lines(&r, &plain(), Depth::Normal);
        assert!(!out.is_empty(), "silence is not a briefing");

        let text = out.join("\n");
        assert!(text.contains("no project resolved"), "{text}");
        assert!(text.contains("nothing claimed"), "{text}");
        assert!(text.contains("wsp claim"), "and what to do about it:\n{text}");
        assert!(r.looking.is_none(), "no mandate, so this pane is not going looking");
    }

    /// One line, and only for the pane it is true of.
    ///
    /// A brief is the most expensive output wsp has — a session-start hook runs
    /// it, and every request of every session pays for what it printed — so a
    /// line has to earn its place against that. This one carries two facts an
    /// agent can get nowhere else and gets wrong in opposite directions
    /// without: that the commit procedure for a shared tree does not apply
    /// here, and that a commit in this tree is on a branch and has not reached
    /// the trunk until `wsp land`. Not knowing the second leaves finished work
    /// in a directory nobody reviews, which is the failure this whole
    /// arrangement would have introduced.
    #[test]
    fn a_pane_in_a_tree_of_its_own_is_told_so_and_one_in_the_trunk_is_not() {
        let mut b = briefing();
        b.cwd = Some("/home/ed/claude/wsp/.worktrees/t-001/src".into());
        let r = compose(&b);
        assert_eq!(r.own_tree.as_deref(), Some("t-001"));
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("wsp land"), "an agent was not told how to get its work out:\n{text}");

        let r = compose(&briefing());
        assert!(r.own_tree.is_none(), "the trunk is not a tree of its own");
        assert!(
            !brief_lines(&r, &plain(), Depth::Normal).join("\n").contains("wsp land"),
            "every brief in the shared tree pays for a line about a tree it is not in"
        );
    }

    /// Who can reach the files under your hands goes first and in the colour
    /// that means a decision. Everyone else is context, and the ones holding
    /// nothing are a number rather than names — twenty shells that have stood
    /// in a directory since Tuesday would push the two that matter off the
    /// bottom.
    #[test]
    fn the_pane_that_can_clobber_you_is_named_and_the_quiet_ones_are_counted() {
        let r = compose(&briefing());
        assert_eq!(r.near.len(), 1, "one other pane in this tree");
        assert_eq!(r.near[0].pane, "w2:p1");
        assert_eq!(r.hidden, 1, "the shell in ~/music is a count, not a name");
        assert!(r.others.is_empty(), "…and it holds nothing, so it is not named");

        let text = brief_lines(&r, &plain(), Depth::Terse).join("\n");
        assert!(text.contains("same tree"), "how close it is, said out loud:\n{text}");
        assert!(text.contains("1 more · wsp overlap"), "{text}");
    }

    /// An agent under a mandate with nothing in hand *is* going looking, and
    /// the brief is the longest of those windows — a whole session start, that
    /// nobody sent, so nothing else would say so. A brief with no mandate is
    /// left alone: waiting on a person is not looking.
    #[test]
    fn a_mandate_with_nothing_in_hand_is_a_pane_gone_looking() {
        let mut b = briefing();
        b.world.bindings.remove("w1:p1");
        let r = compose(&b);
        assert_eq!(r.looking, Some(true), "under a mandate, with work to find");
        assert!(brief_lines(&r, &plain(), Depth::Terse).join("\n").contains("wsp claim it"), "the next move, named");

        // Nothing to find is still looking — and says the mandate is done
        // rather than going quiet.
        b.world.tasks.retain(|t| !t.status().is_open() || t.project.is_none());
        let r = compose(&b);
        assert_eq!(r.looking, Some(false));
        assert!(brief_lines(&r, &plain(), Depth::Terse).join("\n").contains("the mandate is done"));

        // And with no mandate the pane is not looking at all.
        b.mandate = None;
        assert!(compose(&b).looking.is_none());
    }

    /// `--terse` drops the rules and says it did. A brief that quietly stops
    /// before them reads exactly like a store with no rules in it, which is
    /// the failure MAX_RULES is written against arriving by another door.
    #[test]
    fn terse_names_the_rules_it_dropped() {
        let r = compose(&briefing());
        let full = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        let terse = brief_lines(&r, &plain(), Depth::Terse).join("\n");

        assert!(full.contains("commit through your own index"));
        assert!(!terse.contains("commit through your own index"));
        assert!(terse.contains("rules omitted"), "named rather than dropped:\n{terse}");

        // A store carrying no rules says nothing either way — there is nothing
        // to have omitted.
        let mut b = briefing();
        b.rules = None;
        let text = brief_lines(&compose(&b), &plain(), Depth::Terse).join("\n");
        assert!(!text.contains("rules omitted"), "{text}");
    }

    /// A briefing with everything the session payload is made of: a claimed
    /// task carrying prose, a decision and a log; a parent carrying direction;
    /// a sibling named by id in that prose; and a handbook at two levels of the
    /// project chain.
    fn with_work() -> Briefing {
        let mut b = briefing();
        b.project = Some("robustness".into());

        let mut wsp = Project::new("wsp");
        wsp.roots = vec!["/home/ed/claude/wsp".into()];
        wsp.tags = vec!["rust".into()];
        wsp.body = "## Decisions\n- 2026-08-16 the store is the only writer\n\n\
                    ## Handbook\nthe code's own map is architecture.md at the root\n"
            .into();
        let mut robust = Project::new("robustness");
        robust.parent = Some("wsp".into());
        robust.body = "## Handbook\nnothing lands here without a test\n".into();
        b.world.index = crate::resolve::Index::new(vec![wsp, robust]);
        // A realistically shaped id, because that is what the scanner reads:
        // store-allocated ids are always `t-YYMMDD-NNN`, and a scanner loose
        // enough to match `t-003` would pull `t-12` out of a version number.
        b.world.tasks.push(task("t-260815-007", "the sibling it leans on", Some("wsp"), "todo"));

        for t in b.world.tasks.iter_mut() {
            match t.id.as_str() {
                "t-001" => {
                    t.parent = Some("t-004".into());
                    t.body = "## Overview\nthe shape of it, which leans on t-260815-007 \
                              and on t-260815-999 that nobody kept\n\n\
                              ## Decisions\n- 2026-08-17 do the small half first\n\n\
                              ## Log\n- 2026-08-17 claimed by pane w1:p1\n"
                        .into();
                }
                "t-004" => {
                    t.body = "## Decisions\n- 2026-08-16 measure it, do not assume it\n".into();
                }
                _ => {}
            }
        }
        b
    }

    /// A brief lists what binds *now*. A decision a later one supersedes has
    /// been withdrawn, so stating it in four lines that every session re-reads
    /// is worse than saying nothing — the reader acts on it. It is not lost:
    /// the count says the record is longer than this, and `project show` prints
    /// the withdrawn entries struck through rather than dropping them.
    #[test]
    fn a_brief_states_the_rules_in_force_and_not_the_ones_withdrawn() {
        let mut b = with_work();
        let mut wsp = Project::new("wsp");
        wsp.body = "## Decisions\n\
                    - 2026-08-16T09:00:00Z (d1) the store autocommits with git add -A\n\
                    - 2026-08-17T09:00:00Z (d2 supersedes d1) the commit is scoped to what was written\n"
            .into();
        b.world.index = crate::resolve::Index::new(vec![wsp]);
        b.project = Some("wsp".into());

        let r = compose(&b);
        let text = brief_lines(&r, &plain(), Depth::Normal).join("\n");
        assert!(text.contains("the commit is scoped"), "the rule in force is stated:\n{text}");
        assert!(!text.contains("git add -A"), "the withdrawn one is not:\n{text}");
        assert_eq!(r.dropped, 1, "and the record is named as longer than this");
        assert!(text.contains("1 earlier · wsp project show wsp"), "{text}");
    }

    /// The rule the whole mode exists to keep: **plain `wsp brief` does not
    /// grow**. It is run constantly, mid-session, by every agent and by the
    /// coordinating seat, so anything added to it is paid by every call in
    /// every session on the machine — which is the same arithmetic that makes
    /// the payload worth injecting once at the top, run backwards.
    ///
    /// Asserted as a size relation rather than by naming strings, because the
    /// failure this guards against is somebody deciding a useful line belongs
    /// in the default after all.
    #[test]
    fn the_payload_is_the_session_mode_and_plain_brief_does_not_grow() {
        let r = compose(&with_work());
        let text = |d| brief_lines(&r, &plain(), d).join("\n");
        let (session, normal, terse) =
            (text(Depth::Session), text(Depth::Normal), text(Depth::Terse));

        assert!(session.len() > normal.len(), "the payload is what --session adds");

        // Every block of the payload, absent from the default and present in
        // the session brief.
        for needle in [
            "the shape of it",              // the task's own overview
            "do the small half first",      // what it has already settled
            "measure it, do not assume it", // what its parent binds it to
            "t-260815-007",                 // the sibling its prose names
            "architecture.md",              // the handbook's pointer at the code
            "nothing lands here without a test",
        ] {
            assert!(session.contains(needle), "missing from --session: {needle}\n{session}");
            assert!(!normal.contains(needle), "leaked into plain brief: {needle}\n{normal}");
            assert!(!terse.contains(needle), "leaked into --terse: {needle}\n{terse}");
        }

        // What the default already said, it still says. A mode that added
        // context by moving it would be a regression wearing a saving's
        // clothes.
        for needle in ["the task in hand", "next up", "same tree"] {
            assert!(normal.contains(needle), "{normal}");
            assert!(session.contains(needle), "{session}");
        }
    }
    /// A log entry is bounded per entry, and the bound is stated rather than
    /// silent. A review note that ran to 2,000 characters arrives looking
    /// exactly like a sentence somebody wrote that long unless the brief says
    /// it was cut — and says where the rest lives, which is the task itself,
    /// not the brief.
    #[test]
    fn a_long_log_entry_is_shortened_and_the_shortening_is_named() {
        let mut b = with_work();
        for t in b.world.tasks.iter_mut() {
            if t.id == "t-001" {
                // Filler first, and a marker only the tail carries: the
                // negative assertion has to be about something the visible
                // head cannot contain.
                t.body = "## Overview\nthe shape of it\n\n## Log\n\
                          - 2026-08-17 claimed by pane w1:p1\n\
                          - the governor's note, on testing: "
                    .to_string()
                    + &"and the argument runs on ".repeat(60)
                    + "TAILMARKER\n";
            }
        }

        let handed = session_text(&b);
        // The churn line is untouched — it was never the problem.
        assert!(handed.contains("claimed by pane w1:p1"), "{handed}");
        // The long one shows its head and names the cut.
        assert!(handed.contains("the governor's note"), "{handed}");
        assert!(
            handed.contains("1 of 2 shortened · wsp show t-001"),
            "the cut is stated, with where the rest lives:\n{handed}"
        );
        assert!(!handed.contains("TAILMARKER"), "{handed}");
    }

    /// The payload split, within one task's own prose (`core-049`): the
    /// overview is *what this is* and rides whole-ish; details is
    /// fetch-on-first-use and stops at its bound with the rest named. The cut
    /// is stated by [`block`] like every other one here — a section that
    /// stopped mid-argument with no count would read as prose that simply ends.
    #[test]
    fn details_arrives_pointed_and_the_overview_whole() {
        let mut b = with_work();
        for t in b.world.tasks.iter_mut() {
            if t.id == "t-001" {
                let mut body = String::from("## Overview\n");
                for i in 0..30 {
                    body += &format!("overview line {i}\n");
                }
                body += "\n## Details\n";
                for i in 0..60 {
                    body += &format!("details line {i}\n");
                }
                t.body = body.into();
            }
        }

        let handed = session_text(&b);
        // The overview survives past where details stops: the two sections are
        // capped on different axes of neediness, not one budget shared.
        assert!(handed.contains("overview line 29"), "{handed}");
        assert!(handed.contains("details line 23"), "{handed}");
        assert!(!handed.contains("details line 24"), "{handed}");
        assert!(
            handed.contains("(36 more lines — wsp show t-001)"),
            "the rest is named, never silent:\n{handed}"
        );
    }

    /// And the split across the chain: an ancestor's handbook arrives as its
    /// lead and a pointer, because it is text every sibling spawn under that
    /// ancestor repeats; the nearest project's handbook is what the budget was
    /// for. Both halves still arrive — an agent that needs the rules above it
    /// is told exactly where they live — but only once per session rather than
    /// eagerly at every level.
    #[test]
    fn an_ancestor_handbook_arrives_as_a_lead_and_the_nearest_one_whole() {
        let mut b = with_work();
        for p in b.world.index.projects.iter_mut() {
            match p.id.as_str() {
                "wsp" => {
                    let mut hb = String::from("## Handbook\nthe root handbook opens here\n");
                    for i in 0..20 {
                        hb += &format!("root rule line {i}\n");
                    }
                    p.body = hb.into();
                }
                "robustness" => {
                    p.body = "## Handbook\nnothing lands here without a test\n".into();
                }
                _ => {}
            }
        }
        b.project = Some("robustness".into());

        let handed = session_text(&b);
        assert!(handed.contains("the root handbook opens here"), "the lead still orients:\n{handed}");
        assert!(!handed.contains("root rule line 7"), "{handed}");
        assert!(
            handed.contains(&format!("({} more lines — wsp project show wsp --handbook)", 21 - 8)),
            "and says where the rest lives:\n{handed}"
        );
        assert!(
            handed.contains("nothing lands here without a test"),
            "the nearest handbook is not the thing that gets cut:\n{handed}"
        );
    }

    /// A parent decision binds as its *rule* — the first sentence, the same cut
    /// `wsp project show` abridges its decisions index on — with the argument
    /// one command away and the count of what went printed beside it. Six
    /// whole decisions measured past 3,000 tokens of payload; d26 alone is
    /// ~600, and none of it is the rule.
    #[test]
    fn a_long_parent_decision_binds_as_its_first_sentence() {
        let mut b = with_work();
        for t in b.world.tasks.iter_mut() {
            if t.id == "t-004" {
                t.body = "## Decisions\n- 2026-08-16 measure it, do not assume it. \
                          What follows is six hundred words on why assuming cost somebody a day, \
                          which no session beneath this one needs re-read to it on every request.\n"
                    .into();
            }
        }

        let handed = session_text(&b);
        assert!(handed.contains("measure it, do not assume it."), "the rule survives:\n{handed}");
        assert!(!handed.contains("six hundred words"), "the argument does not:\n{handed}");
        assert!(
            handed.contains("1 of 1 abridged · wsp show t-004"),
            "and the cut is named:\n{handed}"
        );
    }

    /// A decision that is only its rule is passed through whole: no ellipsis,
    /// no abridged line, nothing that reads like the brief did something to it.
    #[test]
    fn a_parent_decision_that_is_one_sentence_is_untouched() {
        let b = with_work();
        let handed = session_text(&b);
        // pad("binds", 6) plus the row's own space: two spaces after the label.
        assert!(handed.contains("binds  2026-08-16 measure it"), "{handed}");
        assert!(!handed.contains("abridged"), "{handed}");
    }

    /// What `wsp spawn` hands a kind with no session hook is this brief, whole,
    /// and with nothing in it a terminal would have to interpret.
    ///
    /// Two things, and both are about it being *handed over* rather than
    /// printed. It is the `--session` payload and not plain `wsp brief`,
    /// because what it replaces is the `wsp brief --session` the agent used to
    /// be told to run — a shorter one would be a saving the agent pays back at
    /// request 2. And it is unpainted whatever `spawn`'s own stdout is: escape
    /// codes reaching an agent's prompt are noise it reads past, and
    /// `Paint::new()` would decide by whether the *spawning* terminal had a
    /// tty, which is not a fact about the agent at all.
    #[test]
    fn what_spawn_hands_over_is_the_session_brief_with_nothing_to_interpret() {
        let b = with_work();
        let handed = session_text(&b);
        let r = compose(&b);

        assert!(handed.contains("the shape of it"), "not the payload: {handed}");
        assert!(
            handed.len() > brief_lines(&r, &plain(), Depth::Normal).join("\n").len(),
            "handed less than --session: {handed}"
        );
        assert!(!handed.contains('\u{1b}'), "escape codes in a work order: {handed:?}");
        // And it could have failed: the same brief drawn by a paint that writes
        // escapes does.
        assert!(brief_lines(&r, &Paint::painted(), Depth::Session).join("\n").contains('\u{1b}'));
    }

    /// A handbook is inherited the way tags and decisions are, and read root
    /// first: `wsp` says where the code's own documentation lives, `robustness`
    /// adds what is true of `robustness`, and neither repeats the other.
    ///
    /// The pointer rather than the map is the point. A technical description of
    /// a tree belongs in the tree, versioned with the code and reviewed in the
    /// same diff; held here it drifts the moment somebody refactors, and it
    /// would be re-read by every request of every session for the sake of the
    /// fraction of it any one task needs.
    #[test]
    fn the_handbook_is_inherited_down_the_chain_and_points_at_the_repository() {
        let r = compose(&with_work());
        assert_eq!(
            r.handbook.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            ["wsp", "robustness"],
            "general before particular"
        );

        // A project with nothing to say is absent, not an empty heading.
        let mut b = with_work();
        let mut bare = Project::new("robustness");
        bare.parent = Some("wsp".into());
        let keep = b.world.index.get("wsp").cloned().unwrap();
        b.world.index = crate::resolve::Index::new(vec![keep, bare]);
        assert_eq!(compose(&b).handbook.len(), 1);
    }

    /// The siblings a task names are the ones written into its prose, and the
    /// answer given is title and status — enough to know which is worth
    /// opening. Answering with the whole task would be the fetching this
    /// replaces, done eagerly and for every id rather than the one that
    /// mattered.
    #[test]
    fn the_ids_a_task_mentions_are_looked_up_once_and_answered_briefly() {
        let r = compose(&with_work());
        let ids: Vec<&str> = r.refs.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["t-260815-007"], "named in the prose, and it resolves");

        // t-999 is in the same sentence and resolves to nothing — a removed
        // task, or one from another store. Dropped rather than reported: a line
        // saying so would be noise in the one place noise is expensive.
        assert!(!ids.contains(&"t-260815-999"));
        // The task itself and its parent are spelled out above in full, so
        // naming them again here would be the same rows twice.
        assert!(!ids.contains(&"t-001") && !ids.contains(&"t-004"));

        // One row, and it carries the title — enough to tell whether this is
        // the sibling worth opening. `t-260815-999` still appears in the
        // overview, because the overview is printed as it was written; what it
        // does not get is a row promising there is something there to read.
        let session = brief_lines(&r, &plain(), Depth::Session).join("\n");
        let named: Vec<&str> = session.lines().filter(|l| l.starts_with("names")).collect();
        assert_eq!(named.len(), 1, "{session}");
        assert!(named[0].contains("the sibling it leans on"), "{session}");
        assert!(!named[0].contains("t-260815-999"), "{session}");
    }

    /// The id scanner, on the shapes that actually turn up in prose. A false
    /// positive costs a row nobody wanted; a false negative costs the lookup
    /// this mode exists to save.
    #[test]
    fn task_ids_are_read_out_of_prose_and_only_at_a_word_boundary() {
        let mut got = Vec::new();
        mentioned(
            "under t-260816-096, see t-260817-001 and t-260817-001 again. \
             not output-260816-096, not t-26081-1, not t-260816-, (t-260816-0002).",
            &mut got,
        );
        assert_eq!(got, ["t-260816-096", "t-260817-001", "t-260816-0002"], "{got:?}");

        // Punctuation and line ends are boundaries; the middle of a word is
        // not.
        let mut edge = Vec::new();
        mentioned("t-260815-003\n[t-260815-004] wspt-260815-005", &mut edge);
        assert_eq!(edge, ["t-260815-003", "t-260815-004"], "{edge:?}");
    }

    /// Nothing claimed is a shorter payload, not an empty one: the handbook is
    /// the project's and stands whether or not this pane is holding anything.
    #[test]
    fn a_session_with_nothing_claimed_still_gets_the_handbook() {
        let mut b = with_work();
        b.world.bindings.remove("w1:p1");
        let r = compose(&b);
        assert!(r.mine.is_none() && r.refs.is_empty() && r.mine_log.is_empty());

        let session = brief_lines(&r, &plain(), Depth::Session).join("\n");
        assert!(session.contains("architecture.md"), "{session}");
        assert!(!session.contains("the shape of it"), "no task, so no task prose:\n{session}");
    }

    /// The payload is where an agent reads its own history back, and it is read
    /// at 01:15 as often as at any other hour — the reported case is an agent
    /// being handed four lines dated the day before the night it was working.
    /// The stored stamp never reaches it: it is converted, and it would be
    /// twice the width of the date it replaced if it were not.
    #[test]
    fn the_log_an_agent_is_handed_is_dated_in_its_own_calendar() {
        let mut b = with_work();
        for t in b.world.tasks.iter_mut().filter(|t| t.id == "t-001") {
            t.body = t.body.replace(
                "- 2026-08-17 claimed by pane w1:p1",
                "- 2026-08-14 claimed by pane w1:p1\n- 2026-08-16T23:15:00Z blocked: which offset source",
            );
        }
        let r = compose(&b);
        assert_eq!(r.mine_log.len(), 2, "{:?}", r.mine_log);
        assert!(
            r.mine_log[0].starts_with("2026-08-14 claimed"),
            "a line written before the hour was stored stands: {:?}",
            r.mine_log
        );
        assert!(
            r.mine_log[1].starts_with(&format!("{} blocked:", util::local_ymd("2026-08-16T23:15:00Z"))),
            "and the new one reads local: {:?}",
            r.mine_log
        );

        let session = brief_lines(&r, &plain(), Depth::Session).join("\n");
        assert!(!session.contains("23:15:00Z"), "no stored stamp reaches the payload:\n{session}");
    }

    /// The json is the same reckoning as the text, not a second one — and it
    /// carries the whole backlog where the text stops at six.
    #[test]
    fn the_json_is_the_same_brief() {
        let b = briefing();
        let r = compose(&b);
        let v = brief_json(&b, &r, Depth::Normal);
        assert_eq!(v["project"], json!("wsp"));
        assert_eq!(v["path"], json!(["wsp"]));
        assert_eq!(v["mandate"], json!("wsp"));
        assert_eq!(v["task"]["id"], json!("t-001"));
        assert_eq!(v["pane"], json!("w1:p1"));
        assert_eq!(v["here"].as_array().unwrap().len(), r.near.len());
        assert_eq!(v["others"].as_array().unwrap().len(), r.far.len(), "json carries the far set whole");
        assert_eq!(v["open"].as_array().unwrap().len(), r.open.len());
        assert_eq!(v["decisions"].as_array().unwrap().len(), 1);
    }
}
