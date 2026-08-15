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

    let mandate = cmd_mandate::current(store, env.workspace_id.as_deref());

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
    let scope: Option<Vec<String>> = project.as_deref().map(|p| index.subtree(p));
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

    // A briefing under a mandate, with nothing in hand, *is* an agent going
    // looking: the mandate is the permission and the list below is the
    // backlog, so this session's next move is to pick something out of it. It
    // is the longest of the looking windows — a whole session start — and the
    // one nobody sent, so nothing else would have said so.
    //
    // Before the JSON branch because it is a fact about the pane, not about the
    // rendering. A brief with no mandate is left alone: an agent that has to
    // ask before it takes anything is waiting on a person, not looking.
    if mine.is_none() && mandate.is_some() {
        cmd_agent::say_looking(store, &world.panes, project.as_deref(), !open.is_empty());
    }

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
                "mandate": mandate,
                "task": mine.map(|t| t.json()),
                "decisions": decided.iter().map(|(w, t)| json!({ "date": w, "text": t })).collect::<Vec<_>>(),
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

    // Standing direction, before anything about the work itself. An agent
    // under a mandate is allowed to pick up the next piece without being asked
    // again, and behaviour that has to happen unprompted cannot wait for the
    // agent to go looking for permission.
    if let Some(m) = &mandate {
        row("mandate", format!("{}  {}", p.bold(m), p.dim("take work here without asking")));
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
        None if mandate.is_some() => {
            row("you", p.dim("nothing claimed").to_string());
            // The loop, spelled out on the one line where it is actionable.
            match open.first() {
                Some(t) => row(
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
    for (i, (when, what)) in decided.iter().skip(dropped).enumerate() {
        row(
            if i == 0 { "decided" } else { "" },
            format!("{} {}", p.dim(when), util::truncate(what, 56)),
        );
    }
    if dropped > 0 {
        let leaf = project.as_deref().unwrap_or("");
        row("", p.dim(&format!("{dropped} earlier · wsp project show {leaf}")).to_string());
    }

    if shown > 0 {
        for (i, t) in open.iter().take(shown).enumerate() {
            let prio = crate::cmd_task::paint_prio(&p, t.priority());
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
    match rules(store) {
        Some(_) if args.terse() => {
            println!();
            println!("{}", p.dim("(rules omitted — wsp brief, without --terse)"));
        }
        Some(text) => {
            println!();
            for line in text.lines() {
                println!("{}", p.dim(line));
            }
        }
        None => {}
    }
    0
}
