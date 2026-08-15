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

/// `Trance Video · 3h12m` — a claim as one line.
///
/// Both this and `worked_line` join what they have and skip what they do not,
/// because every part is optional: a claim made outside herdr has no label, and
/// one made before the clock was recorded has no duration. Formatting them with
/// fixed separators left `" · 3s · to t-260815-002"` hanging off nothing.
pub fn claim_line(c: &serde_json::Value) -> String {
    let get = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match (get("workspace_label"), get("workspace_id")) {
        ("", "") => {}
        ("", id) => parts.push(id.to_string()),
        (label, "") => parts.push(label.to_string()),
        (label, id) => parts.push(format!("{label} ({id})")),
    }
    let held = util::since(get("claimed_at"));
    if held > 0 {
        parts.push(util::duration_human(held));
    }
    parts.join(" · ")
}

/// `Trance Video · 3h12m · to t-260814-026` — the claim that ended.
pub fn worked_line(w: &serde_json::Value) -> String {
    let get = |k: &str| w.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match (get("workspace_label"), get("workspace_id")) {
        ("", id) if !id.is_empty() => parts.push(id.to_string()),
        (label, _) if !label.is_empty() => parts.push(label.to_string()),
        _ => {}
    }
    let spent = w.get("seconds").and_then(|x| x.as_i64()).unwrap_or(0);
    if spent > 0 {
        parts.push(util::duration_human(spent));
    }
    match get("handed_to") {
        "" => parts.push(get("reason").to_string()),
        next => parts.push(format!("to {next}")),
    }
    parts.join(" · ")
}

/// End a claim and leave the trace behind.
///
/// An agent works several tasks in sequence, so every way a claim can end —
/// handed to the next task, released by hand, finished — has to answer what
/// becomes of the one being left. It keeps its status, because `doing` with
/// nobody on it is a true and useful state: it is the work that is underway
/// and waiting for you. What it loses is the claim, and what it gains is the
/// record of who had it and for how long.
///
/// Does nothing when there was no claim: `done` on a task nobody ever picked
/// up must not write a line saying it was released.
pub fn hand_off(store: &Store, task_id: &str, to: Option<&str>, reason: &str) {
    let Some(claim) = store.claims().get(task_id).cloned() else {
        return;
    };
    let get = |k: &str| claim.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let from = get("claimed_at");
    let secs = util::since(&from);

    store.set_worked(
        task_id,
        json!({
            "workspace_id": get("workspace_id"),
            "workspace_label": get("workspace_label"),
            "cwd": get("cwd"),
            "host": get("host"),
            "from": from,
            "to": util::now_iso(),
            "seconds": secs,
            "handed_to": to,
            "reason": reason,
        }),
    );
    store.clear_claim(task_id);

    if let Some(mut t) = store.task(task_id) {
        let spent = if secs > 0 { format!(" after {}", util::duration_human(secs)) } else { String::new() };
        match to {
            Some(next) => t.log(&format!("handed off to {next}{spent}")),
            None => t.log(&format!("released{spent}")),
        }
        t.touch();
        let _ = store.save_task(&t);
    }
    store.log_event(
        if to.is_some() { "task-handoff" } else { "task-released" },
        json!({ "id": task_id, "to": to, "reason": reason, "seconds": secs }),
    );
}

/// Every task this pane, or the workspace it sits in, is about to stop working.
///
/// Two lists rather than one, because a claim outlives the pane that made it:
/// after a herdr restart the binding is gone and only the claim says the
/// workspace was on that task, so migrating there without looking would leave
/// the old claim standing and `reconcile` would bind the agent straight back
/// to the task it had left.
fn leaving(store: &Store, pane: &str, workspace: &str, taking: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(prev) = store
        .bindings()
        .get(pane)
        .and_then(|b| b.get("task_id"))
        .and_then(|x| x.as_str())
    {
        if prev != taking {
            out.push(prev.to_string());
        }
    }
    if !workspace.is_empty() {
        for (task, c) in store.claims() {
            if task == taking || out.contains(&task) {
                continue;
            }
            if c.get("workspace_id").and_then(|x| x.as_str()) == Some(workspace) {
                out.push(task);
            }
        }
    }
    out
}

pub fn claim(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp claim <id>   (inside a herdr pane)");
        return 2;
    };
    let Some(t) = store.find_task(&needle) else {
        eprintln!("wsp: no task matching `{needle}`");
        return 1;
    };
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane to bind — run this inside a herdr pane, or pass --pane");
        return 2;
    };

    let env = herdr::Env::read();
    let session = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();

    // Claiming on behalf of another pane — from the panel, say — means the
    // environment describes the caller, not the target. Ask herdr what that
    // pane actually belongs to, or the claim records a workspace that nothing
    // can later resolve.
    let target = herdr::panes().unwrap_or_default().into_iter().find(|p| p.pane_id == pane);
    let workspace = target
        .as_ref()
        .map(|p| p.workspace_id.clone())
        .filter(|w| !w.is_empty())
        .or_else(|| env.workspace_id.clone())
        .unwrap_or_default();
    let cwd = match &target {
        Some(p) if !p.cwd.is_empty() => p.cwd.clone(),
        _ => std::env::current_dir().map(|c| util::contract(&c)).unwrap_or_default(),
    };

    // Whatever this agent was on, it is not on it after this. The claim moves
    // with the agent; the trace stays with the task.
    let left = leaving(store, &pane, &workspace, &t.id);
    for task in &left {
        hand_off(store, task, Some(&t.id), "handoff");
    }

    // The other direction: a task taken off another agent. Two panes bound to
    // one task is not a state anything downstream can read — the tree hangs a
    // pane under the task it is bound to and takes the first it finds, so the
    // second would simply not be drawn.
    let displaced: Vec<String> =
        store.panes_for_task(&t.id).into_iter().filter(|p| p != &pane).collect();
    for other in &displaced {
        store.clear_binding(other);
    }

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

    // The durable half. A pane id is worthless the moment the pane dies, so
    // record the workspace instead — by id, and by the label and cwd herdr
    // keeps in its own session file, which survive the id being reissued.
    let ws_label = herdr::workspaces()
        .unwrap_or_default()
        .into_iter()
        .find(|w| w.id == workspace)
        .map(|w| w.label)
        .unwrap_or_default();
    store.set_claim(
        &t.id,
        json!({
            "workspace_id": workspace,
            "workspace_label": ws_label,
            "cwd": cwd,
            "host": util::hostname(),
            "claimed_at": util::now_iso(),
        }),
    );

    // `hand_off` wrote to the tasks it released, and one of them may be this
    // one — a re-claim of work the same agent put down. Re-read rather than
    // saving the copy taken before all that.
    let mut t = store.task(&t.id).unwrap_or(t);
    if t.status() != Status::Doing {
        t.set_status(Status::Doing);
    }
    for other in &displaced {
        t.log(&format!("taken over from pane {other}"));
    }
    match left.first() {
        Some(prev) => t.log(&format!("claimed by pane {pane}, taken up from {prev}")),
        None => t.log(&format!("claimed by pane {pane}")),
    }
    let _ = store.save_task(&t);
    store.log_event(
        "task-claimed",
        json!({ "id": t.id, "pane": pane, "from": left, "took_over": displaced }),
    );
    store.git_commit(&format!("wsp: claim {} — {}", t.id, t.title));

    // Reflect it in the sidebar immediately rather than waiting for the daemon.
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);

    if args.json() {
        println!(
            "{}",
            json!({ "task": t.json(), "pane": pane, "from": left, "took_over": displaced })
        );
    } else {
        let p = Paint::new();
        println!("{} {}  {}", p.cyan("▸"), p.bold(&t.id), t.title);
        println!("  {}", p.dim(&format!("bound to {pane}")));
        // Naming what was put down is the whole point of a migration being one
        // command: the agent moved, and you can see what it moved off.
        for prev in &left {
            let title = store.task(prev).map(|x| x.title).unwrap_or_default();
            println!("  {}", p.dim(&format!("left {prev}  {}", util::truncate(&title, 48))));
        }
        for other in &displaced {
            println!("  {}", p.dim(&format!("taken from {other}")));
        }
    }
    0
}

/// Rebuild bindings from claims and whatever herdr currently has open.
///
/// This is what makes a binding disposable. herdr restores workspaces, their
/// layouts and their agent sessions across a restart, but pane ids are not
/// stable across it and nothing outside wsp knows which task a pane was on.
/// So: for every claim belonging to this host, find the workspace it names —
/// by id, or failing that by the label and cwd herdr persists — and bind its
/// most plausible pane.
///
/// Returns how many bindings were re-established.
pub fn reconcile(store: &Store) -> usize {
    let claims = store.claims();
    if claims.is_empty() {
        return 0;
    }
    let Ok(panes) = herdr::panes() else { return 0 };
    let workspaces = herdr::workspaces().unwrap_or_default();
    let host = util::hostname();

    let bindings = store.bindings();
    let already: Vec<String> = bindings
        .values()
        .filter_map(|b| b.get("task_id").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .collect();

    // A pane holds one task, here as everywhere else. Two claims naming the
    // same workspace used to pick the same pane and the second quietly
    // overwrote the first — and claims are walked in id order, so an agent came
    // back from a restart bound to the *older* task, the one it had left.
    let mut taken: Vec<String> = bindings.keys().cloned().collect();

    let mut fixed = 0;
    for (task_id, c) in &claims {
        if already.iter().any(|t| t == task_id) {
            continue;
        }
        let get = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("");
        // A claim made on another machine says nothing about this one.
        if !get("host").is_empty() && get("host") != host {
            continue;
        }

        let want_id = get("workspace_id");
        let want_label = get("workspace_label");
        let want_cwd = get("cwd");

        // The id if it still names a workspace; otherwise the label and cwd,
        // which is what survives a workspace being rebuilt under a new id.
        let ws = workspaces
            .iter()
            .find(|w| w.id == want_id)
            .or_else(|| {
                workspaces.iter().find(|w| !want_label.is_empty() && w.label == want_label)
            })
            .map(|w| w.id.clone());
        let Some(ws) = ws else { continue };

        // Prefer a pane running an agent, then any pane that is not one of our
        // own panels. A workspace normally has exactly one candidate.
        let mut candidates: Vec<&herdr::Pane> = panes
            .iter()
            .filter(|p| p.workspace_id == ws && p.label != crate::panel::PANEL_LABEL)
            .collect();
        candidates.sort_by_key(|p| u8::from(p.agent.is_empty()));
        let Some(pane) = candidates.iter().find(|p| !taken.contains(&p.pane_id)) else { continue };
        taken.push(pane.pane_id.clone());

        store.set_binding(
            &pane.pane_id,
            json!({
                "task_id": task_id,
                "pane_id": pane.pane_id,
                "workspace_id": ws,
                "agent_session_id": pane.session_id,
                "cwd": if want_cwd.is_empty() { pane.cwd.clone() } else { want_cwd.to_string() },
                "started_at": util::now_iso(),
                "reconciled": true,
            }),
        );
        store.log_event(
            "task-reconciled",
            json!({ "id": task_id, "pane": pane.pane_id, "workspace": ws }),
        );
        fixed += 1;
    }
    fixed
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
            // Releasing is a decision, so it clears the durable claim too —
            // unlike a pane exiting, which is only ever an accident of process
            // lifetime and must leave the intent standing. It ends the same way
            // a migration does, and leaves the same record behind.
            hand_off(store, task_id, None, "release");
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
    // `--top` marks a workspace as belonging to no project on purpose: the
    // home for whatever runs the whole space, and for terminals that are not
    // work. Without it, "no project" only ever means "nothing resolved", and
    // the two are not the same thing.
    if args.has("top") {
        let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
            eprintln!("wsp: no workspace — pass -w, or run inside herdr");
            return 2;
        };
        store.set_pin(&ws, crate::resolve::TOP_LEVEL);
        if args.json() {
            println!("{}", json!({ "workspace": ws, "project": null, "top": true }));
        } else {
            println!("workspace {ws} pinned outside the project tree");
        }
        return 0;
    }
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp pin <project> [-w workspace] | wsp pin --top");
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
            // Only the binding. The claim survives, because a pane exiting is
            // an accident of process lifetime and says nothing about whether
            // the work is still yours — this is precisely the cascade that
            // once cleared every binding on the machine at a stroke.
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
            if parent == &t.id {
                problems.push(format!("task {} is its own parent", t.id));
            }
        }
        // A loop resolves at every step, so the "unknown parent" check above
        // sees nothing wrong while the tree hangs a row beneath itself. The
        // one-step case is already named above, and naming it twice reads as
        // two faults.
        let mut walk = t.parent.clone().filter(|p| p != &t.id);
        let mut seen: Vec<String> = vec![t.id.clone()];
        while let Some(id) = walk {
            if seen.contains(&id) {
                if id == t.id {
                    problems.push(format!("task {} is in a parent cycle", t.id));
                }
                break;
            }
            seen.push(id.clone());
            walk = tasks.iter().find(|x| x.id == id).and_then(|x| x.parent.clone());
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

/// Turn live herdr workspaces into tasks the store knows about.
///
/// The old workspaces carry their meaning in a hand-typed label — "Trance
/// Video", "TET -> EIN" — and nowhere else. Closing them without reading them
/// first throws away the only record that the work exists. So: for every
/// workspace with no claim on it, propose a task in whichever project the
/// label or the cwd points at, and claim it there.
///
/// Prints a plan and does nothing unless `--yes`.
pub fn adopt(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let index = Index::new(store.projects());
    let pins = store.pins();
    let claims = store.claims();
    let workspaces = herdr::workspaces().unwrap_or_default();
    let panes = herdr::panes().unwrap_or_default();
    let apply = args.has("yes");

    // A label that is only the folder's name says nothing the cwd does not.
    let uninformative = |label: &str, cwd: &str| -> bool {
        let leaf = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        label.eq_ignore_ascii_case(leaf) || label.is_empty()
    };

    let mut plan: Vec<(String, String, Option<String>, String)> = Vec::new();
    for w in &workspaces {
        let ws_panes: Vec<&herdr::Pane> = panes
            .iter()
            .filter(|p| p.workspace_id == w.id && p.label != crate::panel::PANEL_LABEL)
            .collect();
        let Some(pane) = ws_panes.first() else { continue };
        if claims.values().any(|c| c.get("workspace_id").and_then(|x| x.as_str()) == Some(&w.id)) {
            continue;
        }
        if uninformative(&w.label, &pane.cwd) {
            continue;
        }
        // A workspace deliberately outside the tree is not work to adopt.
        if pins.get(&w.id).map(|p| p == crate::resolve::TOP_LEVEL).unwrap_or(false) {
            continue;
        }
        // Label first here, deliberately: the folder is shared by ten
        // workspaces and the label is the only thing that separates them.
        let project = index
            .project_for_label(&w.label)
            .or_else(|| index.project_for_cwd(&pane.cwd))
            .or_else(|| {
                resolve::resolve(&index, &pins, None, Some(&w.id), Some(&w.label), Some(&pane.cwd))
                    .project
            });
        plan.push((w.id.clone(), w.label.clone(), project, pane.pane_id.clone()));
    }

    if plan.is_empty() {
        println!("nothing to adopt — every workspace is claimed or unnamed");
        return 0;
    }

    let p = Paint::new();
    for (ws, label, project, _) in &plan {
        println!(
            "{}  {}  {}",
            p.dim(&util::pad(ws, 4)),
            util::pad(label, 22),
            p.dim(project.as_deref().unwrap_or("(inbox)"))
        );
    }
    if !apply {
        println!("\n{} workspace(s). Re-run with --yes to create the tasks and claim them.", plan.len());
        return 0;
    }

    let mut made = 0;
    for (ws, label, project, pane) in &plan {
        let Ok(id) = store.alloc_task_id() else { continue };
        let mut t = crate::model::Task::new(label, &id);
        t.project = project.clone();
        t.set_status(Status::Doing);
        t.log(&format!("adopted from herdr workspace {ws}"));
        if store.save_task(&t).is_err() {
            continue;
        }
        store.set_claim(
            &t.id,
            json!({
                "workspace_id": ws,
                "workspace_label": label,
                "cwd": panes.iter().find(|p| &p.pane_id == pane).map(|p| p.cwd.clone()).unwrap_or_default(),
                "host": util::hostname(),
                "claimed_at": util::now_iso(),
            }),
        );
        store.set_binding(
            pane,
            json!({
                "task_id": t.id,
                "pane_id": pane,
                "workspace_id": ws,
                "cwd": "",
                "started_at": util::now_iso(),
                "adopted": true,
            }),
        );
        store.log_event("task-adopted", json!({ "id": t.id, "workspace": ws, "label": label }));
        made += 1;
    }
    store.git_commit(&format!("wsp: adopt {made} workspace(s)"));
    println!("adopted {made} workspace(s)");
    0
}
