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
use crate::model::Task;
use crate::overlap;
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

pub fn brief(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let tasks = store.tasks();
    let bindings = store.bindings();
    let claims = store.claims();
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

    // Everyone else, nearest first. `standing_beside` is the one definition
    // of that reckoning — `wsp overlap` and `wsp claim` read the same vector —
    // so the brief's only job is deciding what a briefing shows of it.
    let world = overlap::World::live(store);
    let here = std::env::current_dir().ok().map(|c| util::contract(&c));
    let all = overlap::standing_beside(
        &world,
        env.pane_id.as_deref().unwrap_or_default(),
        here.as_deref(),
    );

    // Two questions, and only the first is a warning. Panes that can reach the
    // files under your hands go at the top and get the colour; everyone else
    // is context.
    let (near, far): (Vec<&overlap::Standing>, Vec<&overlap::Standing>) =
        all.iter().partition(|s| s.relation.is_near());

    // Of twenty-two panes here, twenty are shells that have been sitting in a
    // directory since Tuesday. Naming them would push the two that matter off
    // the bottom, so the far set names whoever is holding something and counts
    // the rest in one line.
    let (far_named, far_quiet): (Vec<&&overlap::Standing>, Vec<&&overlap::Standing>) =
        far.iter().partition(|s| s.agent || s.task.is_some());

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
                "here": near.iter().map(|s| s.json()).collect::<Vec<_>>(),
                "others": far.iter().map(|s| s.json()).collect::<Vec<_>>(),
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

    // Who can reach the files you are about to edit. First, and in the colour
    // that means a decision, because this is the line that would have caught
    // two agents in one checkout this morning.
    for (i, o) in near.iter().enumerate() {
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

    let far_shown = far_named.len().min(MAX_OTHERS);
    for (i, o) in far_named.iter().take(far_shown).enumerate() {
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
    let hidden = far_named.len() - far_shown + far_quiet.len();
    if hidden > 0 {
        row(
            if far_shown == 0 { "others" } else { "" },
            p.dim(&format!("{hidden} more · wsp overlap")).to_string(),
        );
    }

    if let Some(text) = rules(store) {
        println!();
        for line in text.lines() {
            println!("{}", p.dim(line));
        }
    }
    0
}
