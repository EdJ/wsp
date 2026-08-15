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

use crate::cmd_agent::current_project;
use crate::herdr;
use crate::model::{Status, Task};
use crate::resolve::{self, Index};
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
const MAX_RULES: usize = 40;

/// The protocol an agent works to, kept in the store rather than in this
/// binary. It is the user's to write, versioned with the tasks it talks about,
/// and readable by anything that can read a file — none of which is true of a
/// string compiled in here.
fn rules(store: &Store) -> Option<String> {
    let text = std::fs::read_to_string(store.root.join("agents.md")).ok()?;
    let kept: Vec<&str> = text.lines().take(MAX_RULES).collect();
    let out = kept.join("\n").trim_end().to_string();
    (!out.is_empty()).then_some(out)
}

struct Other {
    pane: String,
    where_: String,
    project: String,
    task: Option<Task>,
    /// The terminal title, for a pane with no claim — the only thing herdr
    /// knows about what it is doing.
    title: String,
    state: String,
    needs_you: bool,
}

pub fn brief(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let tasks = store.tasks();
    let bindings = store.bindings();
    let claims = store.claims();
    let pins = store.pins();
    let env = herdr::Env::read();

    let project = current_project(store, args, &index).unwrap_or(None);
    let tags = project.as_deref().map(|p| index.effective_tags(p)).unwrap_or_default();
    let path: Vec<String> = match &project {
        Some(p) => {
            let mut chain = index.ancestors(p);
            chain.reverse();
            chain.push(p.clone());
            chain
        }
        None => Vec::new(),
    };
    let about = project.as_deref().and_then(|p| index.get(p)).map(|p| p.brief.clone()).unwrap_or_default();

    // What this pane is on. The binding is the live answer; the claim is the
    // durable one and outlives a restart, so a session that comes back before
    // the daemon has reconciled still knows what it was doing.
    let mine: Option<&Task> = env
        .pane_id
        .as_ref()
        .and_then(|p| bindings.get(p))
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .or_else(|| {
            env.workspace_id.as_deref().and_then(|ws| {
                claims
                    .iter()
                    .find(|(_, c)| c.get("workspace_id").and_then(|x| x.as_str()) == Some(ws))
                    .map(|(id, _)| id.as_str())
            })
        })
        .and_then(|id| tasks.iter().find(|t| t.id == id));

    // The backlog for this project, minus whatever is already in hand.
    let mut open: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.status().is_open())
        .filter(|t| match &project {
            Some(p) => t.project.as_deref() == Some(p.as_str()),
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

    // Everyone else. Panes rather than agents, because a shell standing in a
    // tree is the fact that produced this whole line of work — but our own
    // furniture is not somebody, and neither is this pane.
    //
    // PROVISIONAL: this whole block comes out when `overlap::standing_beside`
    // lands (t-260815-011). It answers "who else is working"; the question
    // that matters more is "who else is standing where I am", which needs the
    // root resolution being written next door. One definition, not two.
    let panes = if herdr::available() { herdr::panes().unwrap_or_default() } else { Vec::new() };
    let workspaces = if herdr::available() { herdr::workspaces().unwrap_or_default() } else { Vec::new() };
    let label_of = |ws: &str| -> String {
        workspaces.iter().find(|w| w.id == ws).map(|w| w.label.clone()).unwrap_or_default()
    };

    let mut others: Vec<Other> = Vec::new();
    for p in &panes {
        if Some(&p.pane_id) == env.pane_id.as_ref() {
            continue;
        }
        if p.label == crate::panel::PANEL_LABEL || p.label == crate::panel::VIEW_LABEL {
            continue;
        }
        if p.agent.is_empty() {
            continue;
        }
        let task = bindings
            .get(&p.pane_id)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| tasks.iter().find(|t| t.id == id))
            .cloned();
        let label = label_of(&p.workspace_id);
        let r = resolve::resolve(
            &index,
            &pins,
            task.as_ref().and_then(|t| t.project.clone()),
            Some(&p.workspace_id),
            Some(&label),
            Some(&p.cwd),
        );
        let idle = p.agent_status == "idle";
        others.push(Other {
            pane: p.pane_id.clone(),
            where_: if label.is_empty() { p.cwd.clone() } else { label },
            project: r.project.unwrap_or_default(),
            needs_you: idle && task.as_ref().map(|t| t.status() == Status::Doing).unwrap_or(false),
            task,
            title: p.title.clone(),
            state: p.agent_status.clone(),
        });
    }
    // Whoever wants a decision first, then whoever is working, then the rest.
    others.sort_by_key(|o| (u8::from(!o.needs_you), u8::from(o.state != "working"), o.pane.clone()));
    let others_shown = others.len().min(MAX_OTHERS);

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": project,
                "path": path,
                "tags": tags,
                "brief": about,
                "pane": env.pane_id,
                "workspace": env.workspace_id,
                "task": mine.map(|t| t.json()),
                "open": open.iter().map(|t| t.json()).collect::<Vec<_>>(),
                "others": others.iter().map(|o| json!({
                    "pane": o.pane, "where": o.where_, "project": o.project,
                    "task": o.task.as_ref().map(|t| t.json()), "title": o.title,
                    "state": o.state, "needs_you": o.needs_you,
                })).collect::<Vec<_>>(),
                "rules": rules(store),
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    let row = |label: &str, body: String| println!("{} {}", p.dim(&util::pad(label, 6)), body);

    // Where you are, and what this place is for.
    match &project {
        Some(_) => {
            let mut head = path.join("/");
            if !tags.is_empty() {
                head.push_str(&format!("  {}", p.dim(&tags.join(" "))));
            }
            row("where", head);
            if !about.trim().is_empty() {
                row("", p.dim(&util::truncate(about.trim(), 68)));
            }
        }
        None => row("where", p.dim("no project resolved for this pane").to_string()),
    }

    // What you are on. `—` rather than silence: an agent with no task is a
    // thing worth noticing, since claiming one is the first move.
    match mine {
        Some(t) => {
            row(
                "you",
                format!("{}  {}  {}", p.bold(&t.id), p.dim(t.status().as_str()), util::truncate(&t.title, 52)),
            );
            // What it is part of. Direction lands on a parent and the work
            // happens a sub-task at a time, so the piece in hand is rarely the
            // reason it is being done.
            if let Some(parent) = t.parent.as_ref().and_then(|id| tasks.iter().find(|x| &x.id == id)) {
                row(
                    "under",
                    format!("{}  {}", p.dim(&parent.id), p.dim(&util::truncate(&parent.title, 52))),
                );
            }
            let under = resolve::counts_under(&tasks, &t.id);
            if under.open > 0 {
                row("", p.dim(&format!("{} sub-task(s) open beneath it", under.open)));
            }
        }
        None => row("you", p.dim("nothing claimed — wsp claim <id>, or wsp add \"…\" first").to_string()),
    }

    if shown > 0 {
        for (i, t) in open.iter().take(shown).enumerate() {
            let prio = if t.priority() == crate::model::Priority::High { p.yellow("!") } else { " ".into() };
            let under = resolve::counts_under(&tasks, &t.id);
            let kids = if under.open > 0 { p.dim(&format!("  ({} open)", under.open)) } else { String::new() };
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
        if open.len() > shown {
            row("", p.dim(&format!("{} more · wsp ls", open.len() - shown)));
        }
    }

    if others_shown > 0 {
        for (i, o) in others.iter().take(others_shown).enumerate() {
            let what = match &o.task {
                Some(t) => format!("{}  {}", p.dim(&t.id), util::truncate(&t.title, 40)),
                None => p.dim(&format!("unclaimed · {}", util::truncate(&o.title, 40))),
            };
            let flag = if o.needs_you { p.yellow("  ← wants a decision") } else { String::new() };
            row(
                if i == 0 { "others" } else { "" },
                format!("{}  {}{}", p.dim(&util::pad(&util::truncate(&o.where_, 14), 14)), what, flag),
            );
        }
        if others.len() > others_shown {
            row("", p.dim(&format!("{} more · wsp wip", others.len() - others_shown)));
        }
    }

    if let Some(text) = rules(store) {
        println!();
        for line in text.lines() {
            println!("{}", p.dim(line));
        }
    }
    0
}
