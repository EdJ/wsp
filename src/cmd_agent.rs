//! Agent binding, context resolution, and the views that join the store to
//! herdr's live panes.

use serde_json::json;

use crate::herdr;
use crate::model::Status;
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::sync;
use crate::util::{self, Paint};
use crate::Args;

/// The project the caller is standing in. `-p` always wins; otherwise the
/// precedence chain is pin > binding > cwd > workspace label.
pub fn current_project(
    store: &Store,
    args: &Args,
    index: &Index,
) -> Result<Option<String>, i32> {
    if let Some(p) = args.get("project") {
        if p == "none" || p == "inbox" {
            return Ok(None);
        }
        return match index.find(&p) {
            Some(found) => Ok(Some(found.id.clone())),
            None => {
                eprintln!("wsp: no such project `{p}`");
                Err(1)
            }
        };
    }

    let env = herdr::Env::read();
    let pins = store.pins();

    let bound_project = env.pane_id.as_ref().and_then(|pane| {
        store
            .bindings()
            .get(pane)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| store.task(id))
            .and_then(|t| t.project)
    });

    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());

    let r = resolve::resolve(
        index,
        &pins,
        bound_project,
        env.workspace_id.as_deref(),
        None,
        cwd.as_deref(),
    );
    if r.project.is_some() {
        return Ok(r.project);
    }

    // Last resort: ask herdr for this workspace's label.
    if let Some(ws) = env.workspace_id.as_deref() {
        if herdr::available() {
            if let Ok(list) = herdr::workspaces() {
                if let Some(w) = list.iter().find(|w| w.id == ws) {
                    return Ok(index.project_for_label(&w.label));
                }
            }
        }
    }
    Ok(None)
}

fn pane_id(args: &Args) -> Option<String> {
    args.get("pane").or_else(|| herdr::Env::read().pane_id)
}

pub fn claim(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp claim <id>   (inside a herdr pane)");
        return 2;
    };
    let Some(mut t) = store.find_task(&needle) else {
        eprintln!("wsp: no task matching `{needle}`");
        return 1;
    };
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane to bind — run this inside a herdr pane, or pass --pane");
        return 2;
    };

    let env = herdr::Env::read();
    let cwd = std::env::current_dir().map(|c| util::contract(&c)).unwrap_or_default();
    let workspace = env.workspace_id.clone().unwrap_or_default();
    let session = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();

    store.set_binding(
        &pane,
        json!({
            "task_id": t.id,
            "pane_id": pane,
            "workspace_id": workspace,
            "agent_session_id": session,
            "cwd": cwd,
            "started_at": util::now_iso(),
        }),
    );

    if t.status() != Status::Doing {
        t.set_status(Status::Doing);
    }
    t.log(&format!("claimed by pane {pane}"));
    let _ = store.save_task(&t);
    store.log_event("task-claimed", json!({ "id": t.id, "pane": pane }));
    store.git_commit(&format!("wsp: claim {} — {}", t.id, t.title));

    // Reflect it in the sidebar immediately rather than waiting for the daemon.
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);

    if args.json() {
        println!("{}", json!({ "task": t.json(), "pane": pane }));
    } else {
        let p = Paint::new();
        println!("{} {}  {}", p.cyan("▸"), p.bold(&t.id), t.title);
        println!("  {}", p.dim(&format!("bound to {pane}")));
    }
    0
}

pub fn release(store: &Store, args: &Args) -> i32 {
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane — pass --pane or run inside herdr");
        return 2;
    };
    let had = store.bindings().get(&pane).cloned();
    let removed = store.clear_binding(&pane);
    if removed {
        if let Some(task_id) = had.as_ref().and_then(|b| b.get("task_id")).and_then(|t| t.as_str()) {
            store.log_event("task-released", json!({ "id": task_id, "pane": pane }));
        }
        let mut cache = sync::Cache::default();
        let _ = sync::sync(store, &mut cache, true);
    }
    if args.json() {
        println!("{}", json!({ "pane": pane, "released": removed }));
    } else if removed {
        println!("released {pane}");
    } else {
        println!("nothing bound to {pane}");
    }
    0
}

pub fn pin(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp pin <project> [-w workspace]");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(proj) = index.find(&needle) else {
        eprintln!("wsp: no such project `{needle}`");
        return 1;
    };
    let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };

    store.set_pin(&ws, &proj.id);
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);

    if args.json() {
        println!("{}", json!({ "workspace": ws, "project": proj.id }));
    } else {
        println!("workspace {ws} pinned to {}", proj.id);
    }
    0
}

pub fn unpin(store: &Store, args: &Args) -> i32 {
    let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };
    let removed = store.clear_pin(&ws);
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);
    if args.json() {
        println!("{}", json!({ "workspace": ws, "unpinned": removed }));
    } else if removed {
        println!("workspace {ws} unpinned");
    } else {
        println!("workspace {ws} was not pinned");
    }
    0
}

pub fn where_am_i(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let env = herdr::Env::read();
    let pins = store.pins();
    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());

    let binding = env.pane_id.as_ref().and_then(|p| store.bindings().get(p).cloned());
    let bound_task = binding
        .as_ref()
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .and_then(|id| store.task(id));

    let label = match (&env.workspace_id, herdr::available()) {
        (Some(ws), true) => herdr::workspaces()
            .ok()
            .and_then(|list| list.into_iter().find(|w| &w.id == ws).map(|w| w.label)),
        _ => None,
    };

    let r = resolve::resolve(
        &index,
        &pins,
        bound_task.as_ref().and_then(|t| t.project.clone()),
        env.workspace_id.as_deref(),
        label.as_deref(),
        cwd.as_deref(),
    );

    let tags = r.project.as_ref().map(|p| index.effective_tags(p)).unwrap_or_default();
    // What cwd alone would have said — worth showing, because a claimed pane
    // keeps its project even after you cd somewhere else.
    let by_cwd = cwd.as_deref().and_then(|c| index.project_for_cwd(c));

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": r.project,
                "source": r.source,
                "tags": tags,
                "by_cwd": by_cwd,
                "workspace_id": env.workspace_id,
                "workspace_label": label,
                "pane_id": env.pane_id,
                "cwd": cwd,
                "task": bound_task.as_ref().map(|t| t.json()),
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    match &r.project {
        Some(proj) => {
            println!("{}  {}", p.bold(proj), p.dim(&format!("via {}", r.source)));
            if !tags.is_empty() {
                println!("{}", p.dim(&tags.join(" ")));
            }
        }
        None => println!("{}", p.dim("no project resolved for this pane")),
    }
    if let Some(t) = &bound_task {
        println!("\n{} {}  {}", p.cyan("▸"), p.bold(&t.id), t.title);
    }
    if let Some(c) = &by_cwd {
        if Some(c) != r.project.as_ref() {
            println!("\n{}", p.dim(&format!("cwd alone would say {c} — `wsp release` to follow the directory instead")));
        }
    }
    0
}

pub fn wip(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let tasks = store.tasks();
    let bindings = store.bindings();
    let pins = store.pins();

    let agents = if herdr::available() { herdr::agents().unwrap_or_default() } else { Vec::new() };
    let workspaces = if herdr::available() { herdr::workspaces().unwrap_or_default() } else { Vec::new() };

    struct Row {
        project: String,
        task: String,
        task_id: String,
        pane: String,
        workspace: String,
        state: String,
        needs_you: bool,
    }

    let mut rows: Vec<Row> = Vec::new();
    for a in &agents {
        let bound = bindings
            .get(&a.pane_id)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| tasks.iter().find(|t| t.id == id));

        let label = workspaces.iter().find(|w| w.id == a.workspace_id).map(|w| w.label.clone());
        let r = resolve::resolve(
            &index,
            &pins,
            bound.and_then(|t| t.project.clone()),
            Some(&a.workspace_id),
            label.as_deref(),
            Some(&a.cwd),
        );

        let idle = a.agent_status == "idle";
        let needs_you = idle && bound.map(|t| t.status() == Status::Doing).unwrap_or(false);

        rows.push(Row {
            project: r.project.unwrap_or_else(|| "—".into()),
            task: bound
                .map(|t| t.title.clone())
                .unwrap_or_else(|| if a.title.is_empty() { "(unbound)".into() } else { format!("({})", a.title) }),
            task_id: bound.map(|t| t.id.clone()).unwrap_or_default(),
            pane: a.pane_id.clone(),
            workspace: label.unwrap_or_default(),
            state: a.agent_status.clone(),
            needs_you,
        });
    }
    rows.sort_by(|a, b| a.project.cmp(&b.project).then(a.pane.cmp(&b.pane)));

    let blocked: Vec<_> = tasks.iter().filter(|t| t.status() == Status::Blocked).collect();
    let inbox = tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count();
    let needs = rows.iter().filter(|r| r.needs_you).count();

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "agents": rows.iter().map(|r| json!({
                    "project": r.project, "task": r.task, "task_id": r.task_id,
                    "pane": r.pane, "workspace": r.workspace, "state": r.state,
                    "needs_you": r.needs_you,
                })).collect::<Vec<_>>(),
                "needs_you": needs,
                "blocked": blocked.iter().map(|t| t.json()).collect::<Vec<_>>(),
                "inbox": inbox,
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    if rows.is_empty() {
        println!("{}", p.dim("no agents running"));
    } else {
        println!(
            "{}  ·  {} agents  ·  {}",
            p.bold("WIP"),
            rows.len(),
            if needs > 0 { p.yellow(&format!("{needs} need you")) } else { p.dim("all busy") }
        );
        println!();
        let pw = rows.iter().map(|r| r.project.chars().count()).max().unwrap_or(7).max(7);
        let tw = 46;
        println!(
            "{}  {}  {}  {}",
            p.dim(&util::pad("PROJECT", pw)),
            p.dim(&util::pad("TASK", tw)),
            p.dim(&util::pad("PANE", 7)),
            p.dim("STATE")
        );
        for r in &rows {
            let state = match r.state.as_str() {
                "working" => p.green(&util::pad("working", 8)),
                "idle" => p.dim(&util::pad("idle", 8)),
                other => p.dim(&util::pad(other, 8)),
            };
            let flag = if r.needs_you { p.yellow("← needs you") } else { String::new() };
            println!(
                "{}  {}  {}  {} {}",
                util::pad(&r.project, pw),
                util::pad(&util::truncate(&r.task, tw), tw),
                p.dim(&util::pad(&r.pane, 7)),
                state,
                flag
            );
        }
    }

    if !blocked.is_empty() {
        println!();
        println!("{}  {}", p.red(&util::pad("BLOCKED", 8)), blocked.len());
        for t in &blocked {
            println!(
                "  {}  {}  {}",
                p.dim(&t.id),
                t.project.clone().unwrap_or_else(|| "—".into()),
                util::truncate(&t.title, 56)
            );
        }
    }
    if inbox > 0 {
        println!("\n{}  {}   {}", p.dim(&util::pad("INBOX", 8)), inbox, p.dim("wsp inbox"));
    }
    0
}

pub fn sync_once(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket at {}", herdr::socket_path().display());
        return 1;
    }
    // A one-shot sync always forces: there is no warm cache to trust.
    let mut cache = sync::Cache::default();
    match sync::sync(store, &mut cache, true) {
        Ok(r) => {
            if args.json() {
                println!(
                    "{}",
                    json!({ "workspaces": r.workspaces, "panes": r.panes, "reaped": r.reaped })
                );
            } else {
                println!(
                    "synced {} workspaces, {} panes{}",
                    r.workspaces,
                    r.panes,
                    if r.reaped > 0 { format!(", reaped {} stale binding(s)", r.reaped) } else { String::new() }
                );
            }
            0
        }
        Err(e) => {
            eprintln!("wsp: sync failed: {e}");
            1
        }
    }
}

/// Entry point for herdr `[[events]]` hooks. The event name arrives as an
/// argument, its payload in `HERDR_PLUGIN_EVENT_JSON`.
pub fn hook(store: &Store, args: &Args) -> i32 {
    let event = args
        .rest
        .first()
        .cloned()
        .or_else(|| std::env::var("HERDR_PLUGIN_EVENT").ok())
        .unwrap_or_default();
    let env = herdr::Env::read();

    match event.as_str() {
        "pane.exited" | "pane.closed" | "pane_exited" | "pane_closed" => {
            if let Some(pane) = env.pane_id.clone() {
                if store.clear_binding(&pane) {
                    store.log_event("pane-exited", json!({ "pane": pane }));
                }
            }
        }
        "workspace.created" | "workspace_created" => {
            if let Some(ws) = env.workspace_id.clone() {
                crate::panel::install_if_adopted(store, &ws);
            }
        }
        "workspace.closed" | "workspace_closed" => {
            if let Some(ws) = env.workspace_id.clone() {
                store.clear_pin(&ws);
            }
        }
        _ => {}
    }

    if !herdr::available() {
        return 0;
    }
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, false);
    0
}

pub fn doctor(store: &Store, args: &Args) -> i32 {
    let mut problems: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    if !store.exists() {
        problems.push(format!("no store at {} — run `wsp init`", util::contract(&store.root)));
    } else {
        notes.push(format!("store {}", util::contract(&store.root)));
    }
    if !store.root.join(".git").exists() {
        notes.push("store is not a git repo (history disabled)".into());
    }

    let index = Index::new(store.projects());
    let tasks = store.tasks();
    notes.push(format!("{} projects, {} tasks", index.projects.len(), tasks.len()));

    for p in &index.projects {
        if let Some(parent) = &p.parent {
            if index.get(parent).is_none() {
                problems.push(format!("project {} has missing parent `{}`", p.id, parent));
            }
        }
        // Cycle detection: walking ancestors returns to self.
        if index.ancestors(&p.id).contains(&p.id) {
            problems.push(format!("project {} is in a parent cycle", p.id));
        }
        for r in &p.roots {
            if !util::expand(r).exists() {
                problems.push(format!("project {} root does not exist: {}", p.id, r));
            }
        }
    }

    for t in &tasks {
        if let Some(proj) = &t.project {
            if index.get(proj).is_none() {
                problems.push(format!("task {} references unknown project `{}`", t.id, proj));
            }
        }
        if t.title.trim().is_empty() {
            problems.push(format!("task {} has an empty title", t.id));
        }
        if let Some(parent) = &t.parent {
            if !tasks.iter().any(|x| &x.id == parent) {
                problems.push(format!("task {} references unknown parent `{}`", t.id, parent));
            }
        }
    }

    let bindings = store.bindings();
    for (pane, b) in &bindings {
        let id = b.get("task_id").and_then(|x| x.as_str()).unwrap_or("");
        if store.task(id).is_none() {
            problems.push(format!("binding on {pane} points at missing task `{id}`"));
        }
    }

    if herdr::available() {
        match herdr::agents() {
            Ok(agents) => {
                let live: Vec<String> = agents.iter().map(|a| a.pane_id.clone()).collect();
                let stale = bindings.keys().filter(|p| !live.contains(p)).count();
                if stale > 0 {
                    notes.push(format!("{stale} binding(s) on dead panes — `wsp sync` reaps them"));
                }
                notes.push(format!("herdr up, {} agents", agents.len()));
            }
            Err(e) => problems.push(format!("herdr socket present but unreachable: {e}")),
        }
    } else {
        notes.push("herdr socket not found (CLI still works, sidebar tokens will not update)".into());
    }

    if args.json() {
        println!("{}", json!({ "problems": problems, "notes": notes }));
        return if problems.is_empty() { 0 } else { 1 };
    }

    let p = Paint::new();
    for n in &notes {
        println!("{} {}", p.dim("·"), n);
    }
    for prob in &problems {
        println!("{} {}", p.red("✗"), prob);
    }
    if problems.is_empty() {
        println!("{} no problems", p.green("✓"));
        0
    } else {
        1
    }
}
