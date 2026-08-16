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

    Brief {
        // A briefing under a mandate, with nothing in hand, *is* an agent going
        // looking: the mandate is the permission and the list below is the
        // backlog, so this session's next move is to pick something out of it.
        // It is the longest of the looking windows — a whole session start —
        // and the one nobody sent, so nothing else would say so. A brief with
        // no mandate is left alone: an agent that has to ask before it takes
        // anything is waiting on a person, not looking.
        looking: (mine.is_none() && b.mandate.is_some()).then(|| !open.is_empty()),
        under_mine: mine.map(|t| resolve::counts_under(tasks, &t.id).open).unwrap_or(0),
        parent: mine
            .and_then(|t| t.parent.as_ref())
            .and_then(|id| tasks.iter().find(|x| &x.id == id))
            .cloned(),
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

fn brief_json(b: &Briefing, r: &Brief) -> serde_json::Value {
    json!({
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
    })
}

/// The brief as text, one line per element and nothing printed.
fn brief_lines(r: &Brief, p: &Paint, terse: bool) -> Vec<String> {
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

    // Before the rendering because it is a fact about the pane, not about how
    // the brief is printed: `--json` looks the same to herdr as a person does.
    if let Some(found) = r.looking {
        cmd_agent::say_looking(store, &b.world.panes, r.project.as_deref(), found);
    }

    match args.json() {
        true => println!("{}", serde_json::to_string_pretty(&brief_json(&b, &r)).unwrap_or_default()),
        false => {
            for l in brief_lines(&r, &Paint::new(), args.terse()) {
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

        let text = brief_lines(&r, &plain(), true).join("\n");
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

        let text = brief_lines(&r, &plain(), true).join("\n");
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

        let text = brief_lines(&r, &plain(), true).join("\n");
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
        let out = brief_lines(&r, &plain(), false);
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

        let text = brief_lines(&r, &plain(), true).join("\n");
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
        assert!(brief_lines(&r, &plain(), true).join("\n").contains("wsp claim it"), "the next move, named");

        // Nothing to find is still looking — and says the mandate is done
        // rather than going quiet.
        b.world.tasks.retain(|t| !t.status().is_open() || t.project.is_none());
        let r = compose(&b);
        assert_eq!(r.looking, Some(false));
        assert!(brief_lines(&r, &plain(), true).join("\n").contains("the mandate is done"));

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
        let full = brief_lines(&r, &plain(), false).join("\n");
        let terse = brief_lines(&r, &plain(), true).join("\n");

        assert!(full.contains("commit through your own index"));
        assert!(!terse.contains("commit through your own index"));
        assert!(terse.contains("rules omitted"), "named rather than dropped:\n{terse}");

        // A store carrying no rules says nothing either way — there is nothing
        // to have omitted.
        let mut b = briefing();
        b.rules = None;
        let text = brief_lines(&compose(&b), &plain(), true).join("\n");
        assert!(!text.contains("rules omitted"), "{text}");
    }

    /// The json is the same reckoning as the text, not a second one — and it
    /// carries the whole backlog where the text stops at six.
    #[test]
    fn the_json_is_the_same_brief() {
        let b = briefing();
        let r = compose(&b);
        let v = brief_json(&b, &r);
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
