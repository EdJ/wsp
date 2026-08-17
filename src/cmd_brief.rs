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
/// *this*. Measured on t-260816-096: an agent that arrived with a task title
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
/// The handbook, over the whole project chain.
const MAX_HANDBOOK_LINES: usize = 120;
/// Decisions on the parent, most recent last. The parent is where direction
/// lands, so these are the constraints on the piece in hand.
const MAX_PARENT_DECISIONS: usize = 6;
/// Siblings named by id in what is injected above. Title and status only: the
/// question they answer is "what is t-260816-060", and answering it with the
/// whole task would be the fetching this replaces, done eagerly.
const MAX_REFS: usize = 8;
/// The tail of the task's log. Short on purpose — most of a log is status
/// churn, at about eight tokens a line — but not zero, because direction handed
/// to a task after it was written arrives here and nowhere else. The log line
/// on this very task carried the file list that saved its agent the 28,000
/// tokens of searching the task exists to remove.
const MAX_LOG: usize = 4;

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

/// Task ids written into a chunk of prose — `t-260816-096`, and nothing else.
///
/// Scanned rather than declared. A task names its siblings by writing them into
/// its overview — "independent of t-260817-002", "read t-260816-096's overview
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
    /// The store's own rules, already capped.
    pub rules: Option<String>,
    /// Where this pane resolved to. Taken in rather than worked out here: the
    /// chain is `wsp where`'s subject and it reads the process environment.
    pub project: Option<String>,
    pub pane: Option<String>,
    pub workspace: Option<String>,
    /// This process's directory, contracted. herdr reports the shell's, which
    /// is stale the moment anyone `cd`s.
    pub cwd: Option<String>,
}

impl Briefing {
    /// The live read. `current_project` can fail on a bad `-p`; a brief never
    /// does, so an unresolvable project is no project, which is a shorter
    /// brief.
    pub(crate) fn live(store: &Store, args: &Args) -> Briefing {
        let world = overlap::World::live(store);
        let env = herdr::Env::read();
        Briefing {
            project: current_project(store, args, &world.index).unwrap_or(None),
            mandate: cmd_mandate::current(store, env.workspace_id.as_deref()),
            rules: rules(store),
            pane: env.pane_id,
            workspace: env.workspace_id,
            cwd: std::env::current_dir().ok().map(|c| util::contract(&c)),
            world,
        }
    }
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
    let mine: Option<&Task> = b
        .pane
        .as_ref()
        .and_then(|p| b.world.bindings.get(p))
        .and_then(|x| x.get("task_id"))
        .and_then(|t| t.as_str())
        .or_else(|| {
            b.workspace.as_deref().and_then(|ws| {
                b.world
                    .claims
                    .iter()
                    .find(|(_, c)| c.get("workspace_id").and_then(|x| x.as_str()) == Some(ws))
                    .map(|(id, _)| id.as_str())
            })
        })
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

    let decided: Vec<(String, String)> = path
        .iter()
        .filter_map(|id| index.get(id))
        .flat_map(|proj| crate::model::decisions(&proj.body))
        .collect();
    let dropped = decided.len().saturating_sub(MAX_DECISIONS);

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

    let parent_decided: Vec<(String, String)> =
        parent.map(|p| crate::model::decisions(&p.body)).unwrap_or_default();

    let mine_log: Vec<String> = mine
        .and_then(|t| crate::model::section_of(&t.body, "Log"))
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
        "task": r.mine.as_ref().map(|t| t.json()),
        "decisions": r.decided.iter().map(|(w, t)| json!({ "date": w, "text": t })).collect::<Vec<_>>(),
        "open": r.open.iter().map(|(t, _)| t.json()).collect::<Vec<_>>(),
        "here": r.near.iter().map(|s| s.json()).collect::<Vec<_>>(),
        "others": r.far.iter().map(|s| s.json()).collect::<Vec<_>>(),
        "rules": r.rules,
    });
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
        for sec in ["Overview", "Details"] {
            if let Some(text) = crate::model::section_of(&t.body, sec) {
                out.push(String::new());
                block(&mut out, p, &sec.to_lowercase(), &text, MAX_TASK_LINES, &format!("wsp show {}", t.id));
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
            for (i, l) in r.mine_log.iter().enumerate() {
                out.push(format!(
                    "{} {}",
                    p.dim(&util::pad(if i == 0 { "log" } else { "" }, 6)),
                    p.dim(l)
                ));
            }
        }
    }

    // The parent's decisions. Direction lands on a parent and the work happens
    // a sub-task at a time, so these are the constraints on the piece in hand
    // and they are not written down anywhere the piece itself can see.
    if !r.parent_decided.is_empty() {
        let dropped = r.parent_decided.len().saturating_sub(MAX_PARENT_DECISIONS);
        out.push(String::new());
        for (i, (when, what)) in r.parent_decided.iter().skip(dropped).enumerate() {
            out.push(format!(
                "{} {} {what}",
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
    }

    // What the prose above names. Enough to know which of these is worth
    // opening, and no more — answering "what is t-260816-060" with the whole of
    // t-260816-060 would be the fetching this replaces, done eagerly and for
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
    for (proj, text) in &r.handbook {
        if left == 0 {
            break;
        }
        out.push(String::new());
        block(&mut out, p, "read", text, left, &format!("wsp project show {proj} --handbook"));
        left = left.saturating_sub(text.lines().count());
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

    // Standing direction, before anything about the work itself. An agent
    // under a mandate is allowed to pick up the next piece without being asked
    // again, and behaviour that has to happen unprompted cannot wait for the
    // agent to go looking for permission.
    if let Some(m) = &r.mandate {
        row("mandate", format!("{}  {}", p.bold(m), p.dim("take work here without asking")));
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
        }
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
    for (i, (when, what)) in r.decided.iter().skip(r.dropped).enumerate() {
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
            },
            mandate: Some("wsp".into()),
            rules: Some("commit through your own index".into()),
            project: Some("wsp".into()),
            pane: Some("w1:p1".into()),
            workspace: Some("w1".into()),
            cwd: Some("/home/ed/claude/wsp".into()),
        }
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
            },
            mandate: None,
            rules: None,
            project: None,
            pane: None,
            workspace: None,
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
