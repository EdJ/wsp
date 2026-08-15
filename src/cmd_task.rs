//! Task commands.

use serde_json::json;

use crate::cmd_agent::{claim_line, current_project, worked_line};
use crate::model::{Priority, Status, Task};
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

pub fn add(store: &Store, args: &Args) -> i32 {
    let title = args.text(0);
    if title.trim().is_empty() {
        eprintln!("usage: wsp add \"title\" [-p project] [-t tag]… [--prio high]");
        return 2;
    }

    let index = Index::new(store.projects());

    // Resolve the parent *before* an id exists. `alloc_task_id` reserves the
    // file with O_EXCL, so afterwards `--parent 004` can match the task being
    // created and make it its own parent — which resolves, so `doctor` sees
    // nothing wrong, and the tree hangs a row beneath itself for ever.
    let parent = match args.get("parent") {
        Some(needle) => match store.find_task(&needle) {
            Some(p) => Some(p),
            None => {
                eprintln!("wsp: no such parent task `{needle}`");
                return 1;
            }
        },
        None => None,
    };

    // A sub-task belongs where its parent belongs. Filing it anywhere else
    // splits a piece of work across two places in the tree, and the tree is
    // the only thing that shows the two are related.
    let project = if args.has("inbox") {
        None
    } else if let (Some(p), false) = (&parent, args.has("project")) {
        p.project.clone()
    } else {
        match current_project(store, args, &index) {
            Ok(p) => p,
            Err(code) => return code,
        }
    };
    if let Some(p) = &parent {
        if args.has("project") && p.project != project {
            eprintln!(
                "wsp: parent {} is in {}, not {}",
                p.id,
                p.project.clone().unwrap_or_else(|| "the inbox".into()),
                project.clone().unwrap_or_else(|| "the inbox".into())
            );
            return 1;
        }
    }

    let id = match store.alloc_task_id() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("wsp: cannot allocate id: {e}");
            return 1;
        }
    };

    let mut t = Task::new(title.trim(), &id);
    t.project = project.clone();
    t.tags = args.all("tag");
    t.refs = args.all("ref").iter().map(|r| util::contract(&util::expand(r))).collect();
    if let Some(prio) = args.get("prio").or_else(|| args.get("priority")) {
        match Priority::parse(&prio) {
            Some(p) => t.priority_raw = p.as_str().to_string(),
            None => {
                eprintln!("wsp: priority must be high|normal|low");
                return 2;
            }
        }
    }
    if let Some(s) = args.get("status") {
        match Status::parse(&s) {
            Some(s) => t.status_raw = s.as_str().to_string(),
            None => {
                eprintln!("wsp: unknown status `{s}`");
                return 2;
            }
        }
    }
    if project.is_none() {
        t.status_raw = "inbox".into();
    }
    t.parent = parent.as_ref().map(|p| p.id.clone());

    if let Err(e) = store.save_task(&t) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("task-added", json!({ "id": t.id, "project": t.project, "title": t.title }));
    store.git_commit(&format!("wsp: add {} — {}", t.id, t.title));

    if args.json() {
        println!("{}", t.json());
    } else {
        let p = Paint::new();
        println!(
            "{} {}  {}",
            p.green("+"),
            p.bold(&t.id),
            t.title
        );
        println!(
            "  {}",
            p.dim(&format!(
                "{}  {}",
                t.project.clone().unwrap_or_else(|| "(inbox)".into()),
                t.status().as_str()
            ))
        );
    }
    0
}

fn filtered(store: &Store, args: &Args, index: &Index, scope: Option<String>) -> Vec<Task> {
    let want_status = args.get("status").and_then(|s| Status::parse(&s));
    let want_tag = args.get("tag");
    let show_all = args.has("all");

    let scope_ids: Option<Vec<String>> = scope.map(|s| index.subtree(&s));

    let mut out: Vec<Task> = store
        .tasks()
        .into_iter()
        .filter(|t| match &want_status {
            Some(s) => t.status() == *s,
            None => show_all || t.status().is_open(),
        })
        .filter(|t| match &scope_ids {
            Some(ids) => t.project.as_ref().map(|p| ids.contains(p)).unwrap_or(false),
            None => true,
        })
        .filter(|t| match &want_tag {
            Some(tag) => {
                let mut tags = t.tags.clone();
                if let Some(p) = &t.project {
                    tags.extend(index.effective_tags(p));
                }
                tags.iter().any(|x| x == tag)
            }
            None => true,
        })
        .collect();

    out.sort_by(|a, b| {
        a.status()
            .rank()
            .cmp(&b.status().rank())
            .then(a.priority().rank().cmp(&b.priority().rank()))
            .then(a.id.cmp(&b.id))
    });
    out
}

pub fn list(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());

    // Default scope: the project you're standing in. `--all` widens it.
    let scope = if args.has("all") && !args.has("project") {
        None
    } else {
        match current_project(store, args, &index) {
            Ok(p) => p,
            Err(code) => return code,
        }
    };

    let tasks = filtered(store, args, &index, scope.clone());
    if args.json() {
        let out: Vec<_> = tasks.iter().map(|t| t.json()).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }

    let p = Paint::new();
    if tasks.is_empty() {
        match &scope {
            Some(s) => println!("{}", p.dim(&format!("no open tasks in {s} — wsp add \"…\""))),
            None => println!("{}", p.dim("no open tasks — wsp add \"…\"")),
        }
        return 0;
    }

    if let Some(s) = &scope {
        println!("{}", p.dim(&format!("{}  ({} open)", s, tasks.len())));
    }
    print_tasks(&tasks, &p, scope.is_none());
    0
}

pub fn inbox(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let mut tasks: Vec<Task> = store
        .tasks()
        .into_iter()
        .filter(|t| t.project.is_none() && t.status().is_open())
        .collect();
    tasks.sort_by(|a, b| a.priority().rank().cmp(&b.priority().rank()).then(a.id.cmp(&b.id)));

    if args.json() {
        let out: Vec<_> = tasks.iter().map(|t| t.json()).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }
    let _ = index;
    let p = Paint::new();
    if tasks.is_empty() {
        println!("{}", p.dim("inbox empty"));
        return 0;
    }
    println!("{}", p.dim(&format!("inbox  ({})", tasks.len())));
    print_tasks(&tasks, &p, false);
    0
}

fn print_tasks(tasks: &[Task], p: &Paint, show_project: bool) {
    let idw = tasks.iter().map(|t| t.id.chars().count()).max().unwrap_or(12);
    // Children under their parent, indented. The parent carries what is still
    // open beneath it, because a one-line parent that says nothing about its
    // children is a task you would tick off without looking.
    for (t, depth) in crate::resolve::nest(tasks) {
        let t = &t;
        let indent = "  ".repeat(depth);
        let under = crate::resolve::counts_under(tasks, &t.id);
        let kids = if under.open > 0 {
            p.dim(&format!("  ({} open)", under.open))
        } else if under.done > 0 {
            p.dim(&format!("  ({} done)", under.done))
        } else {
            String::new()
        };
        let st = match t.status() {
            Status::Doing => p.cyan(&util::pad("doing", 8)),
            Status::Blocked => p.red(&util::pad("blocked", 8)),
            Status::Review => p.yellow(&util::pad("review", 8)),
            other => p.dim(&util::pad(other.as_str(), 8)),
        };
        let prio = match t.priority() {
            Priority::High => p.yellow("!"),
            Priority::Low => p.dim("·"),
            Priority::Normal => " ".into(),
        };
        let project = if show_project {
            p.dim(&format!("  [{}]", t.project.clone().unwrap_or_else(|| "inbox".into())))
        } else {
            String::new()
        };
        println!(
            "  {} {} {} {}{}{}{}",
            p.dim(&util::pad(&t.id, idw)),
            st,
            prio,
            indent,
            util::truncate(&t.title, 62usize.saturating_sub(indent.len())),
            kids,
            project
        );
    }
}

pub fn show(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp show <id>");
        return 2;
    };
    let Some(t) = store.find_task(&needle) else {
        eprintln!("wsp: no task matching `{needle}`");
        return 1;
    };
    if args.json() {
        let mut v = t.json();
        v["body"] = json!(t.body);
        // Who has it and who had it. An agent reading this back is usually
        // asking exactly that, and the alternative is reading two state files
        // it has no business knowing the names of.
        v["claim"] = store.claims().get(&t.id).cloned().unwrap_or(json!(null));
        v["worked"] = store.worked().get(&t.id).cloned().unwrap_or(json!(null));
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return 0;
    }

    let p = Paint::new();
    let index = Index::new(store.projects());
    println!("{}  {}", p.bold(&t.id), t.title);
    let mut tags = t.tags.clone();
    if let Some(proj) = &t.project {
        tags.extend(index.effective_tags(proj));
    }
    println!();
    println!("{} {}", p.dim(&util::pad("project", 9)), t.project.clone().unwrap_or_else(|| "(inbox)".into()));
    println!("{} {}", p.dim(&util::pad("status", 9)), t.status().as_str());
    println!("{} {}", p.dim(&util::pad("priority", 9)), t.priority().as_str());
    println!("{} {}", p.dim(&util::pad("tags", 9)), tags.join(" "));
    if !t.refs.is_empty() {
        println!("{} {}", p.dim(&util::pad("refs", 9)), t.refs.join("  "));
    }
    println!("{} {}", p.dim(&util::pad("created", 9)), t.created);
    println!("{} {}", p.dim(&util::pad("updated", 9)), t.updated);

    // Where it sits in the work, both ways. A sub-task read on its own is
    // missing the thing that says why it exists.
    let all = store.tasks();
    if let Some(parent) = t.parent.as_ref().and_then(|id| all.iter().find(|x| &x.id == id)) {
        println!(
            "{} {}  {}",
            p.dim(&util::pad("under", 9)),
            p.dim(&parent.id),
            util::truncate(&parent.title, 56)
        );
    }
    let kids = crate::resolve::children_of(&all, &t.id);
    for (i, k) in kids.iter().enumerate() {
        println!(
            "{} {} {} {}",
            p.dim(&util::pad(if i == 0 { "sub" } else { "" }, 9)),
            p.dim(&k.id),
            p.dim(&util::pad(k.status().as_str(), 8)),
            util::truncate(&k.title, 56)
        );
    }

    for (pane, b) in store.bindings() {
        if b.get("task_id").and_then(|x| x.as_str()) == Some(t.id.as_str()) {
            println!("{} {}", p.dim(&util::pad("agent", 9)), pane);
        }
    }
    if let Some(c) = store.claims().get(&t.id) {
        println!("{} {}", p.dim(&util::pad("claimed", 9)), claim_line(c));
    } else if let Some(w) = store.worked().get(&t.id) {
        // The last agent on it, once there is no current one — the trace a
        // migration leaves, so a task never loses every sign it was worked.
        println!("{} {}", p.dim(&util::pad("worked", 9)), worked_line(w));
    }
    // Decisions first and in their own block: on a task they are what was
    // settled about this work, and burying them mid-body between Details and
    // the log is how a reader misses the one line that would have stopped them.
    let decided = crate::model::decisions(&t.body);
    if !decided.is_empty() {
        println!("\n{}", p.dim("DECISIONS"));
        for (when, what) in &decided {
            println!("  {}  {}", p.dim(&util::pad(when, 10)), what);
        }
    }
    let mut rest = t.body.clone();
    crate::model::set_section_in(&mut rest, "Decisions", "");
    if !rest.trim().is_empty() {
        println!("\n{}", rest.trim());
    }
    0
}

fn mutate<F>(store: &Store, args: &Args, verb: &str, f: F) -> i32
where
    F: FnOnce(&mut Task),
{
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp {verb} <id>");
        return 2;
    };
    let Some(mut t) = store.find_task(&needle) else {
        eprintln!("wsp: no task matching `{needle}`");
        return 1;
    };
    f(&mut t);
    t.touch();
    if let Err(e) = store.save_task(&t) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event(
        &format!("task-{verb}"),
        json!({ "id": t.id, "project": t.project, "status": t.status().as_str(), "title": t.title }),
    );
    store.git_commit(&format!("wsp: {verb} {} — {}", t.id, t.title));

    if args.json() {
        println!("{}", t.json());
    } else {
        let p = Paint::new();
        let mark = match t.status() {
            Status::Done => p.green("✓"),
            Status::Blocked => p.red("■"),
            Status::Doing => p.cyan("▸"),
            _ => p.dim("·"),
        };
        println!("{} {}  {}  {}", mark, p.bold(&t.id), p.dim(t.status().as_str()), t.title);
    }
    0
}

pub fn set_status(store: &Store, args: &Args, status: Status) -> i32 {
    let verb = status.as_str().to_string();
    mutate(store, args, &verb, |t| {
        t.set_status(status);
        t.log(&format!("→ {}", status.as_str()));
    })
}

pub fn done(store: &Store, args: &Args) -> i32 {
    // Let go of the work before finishing it: the log then reads in the order
    // it happened — the claim ended, then the task did — and the whole thing
    // lands in the one commit `mutate` makes on its way out.
    //
    // The claim, not just the binding. A finished task that keeps its claim
    // holds a workspace nothing will ever release: `adopt` goes on skipping it,
    // and after a restart `reconcile` binds an agent back to work that is over.
    // Finishing a parent finishes a claim about its children too, and that
    // claim is checkable. Refusing here is the same shape as `project rm`:
    // the CLI says no, and from the panel that refusal becomes the next
    // question rather than something a keystroke quietly overrode.
    if let Some(t) = args.rest.first().and_then(|needle| store.find_task(needle)) {
        let open = crate::resolve::counts_under(&store.tasks(), &t.id).open;
        if open > 0 && !args.has("force") {
            eprintln!(
                "wsp: {} has {open} open sub-task(s) — finish them, or `wsp done {} --force`",
                t.id, t.id
            );
            return 1;
        }
        for pane in store.panes_for_task(&t.id) {
            store.clear_binding(&pane);
        }
        crate::cmd_agent::hand_off(store, &t.id, None, "done");
    }
    mutate(store, args, "done", |t| {
        t.set_status(Status::Done);
        t.log("→ done");
    })
}

pub fn block(store: &Store, args: &Args) -> i32 {
    let reason = args.text(1);
    if reason.trim().is_empty() {
        eprintln!("usage: wsp block <id> \"reason\"");
        return 2;
    }
    mutate(store, args, "blocked", |t| {
        t.set_status(Status::Blocked);
        t.log(&format!("blocked: {}", reason.trim()));
    })
}

pub fn note(store: &Store, args: &Args) -> i32 {
    let text = args.text(1);
    if text.trim().is_empty() {
        eprintln!("usage: wsp note <id> \"text\"");
        return 2;
    }
    mutate(store, args, "note", |t| t.log(text.trim()))
}

/// `wsp decide <id> "…"` — record what was settled, on a task or a project.
///
/// Takes either, because the two are the same act at different heights: a
/// decision about *this piece of work* belongs on the task, and one that binds
/// everything under a heading belongs on the project, where the agent who has
/// not read this task will still meet it. Resolving a task first and falling
/// back to a project keeps one verb for both — `wsp decide wsp "…"` and
/// `wsp decide 022 "…"` are the same sentence about different scopes.
///
/// Append-only by construction: there is no `wsp undecide`. A decision that
/// turns out wrong is superseded by a later one saying so, which is the honest
/// record — the reasoning that was live at the time is what a reader three
/// months on needs, not a tidied conclusion.
pub fn decide(store: &Store, args: &Args) -> i32 {
    let text = args.text(1);
    if text.trim().is_empty() {
        eprintln!("usage: wsp decide <task|project> \"what was settled, and why\"");
        return 2;
    }
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp decide <task|project> \"what was settled, and why\"");
        return 2;
    };

    if store.find_task(&needle).is_some() {
        return mutate(store, args, "decided", |t| {
            crate::model::append_dated(&mut t.body, "Decisions", text.trim())
        });
    }

    let index = Index::new(store.projects());
    let Some(mut p) = index.find(&needle).cloned() else {
        eprintln!("wsp: no task or project matching `{needle}`");
        return 1;
    };
    crate::model::append_dated(&mut p.body, "Decisions", text.trim());
    if let Err(e) = store.save_project(&p) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("project-decided", json!({ "id": p.id, "text": text.trim() }));
    store.git_commit(&format!("wsp: decide {} — {}", p.id, util::truncate(text.trim(), 60)));

    if args.json() {
        println!("{}", json!({ "project": p.id, "decisions": crate::model::decisions(&p.body).len() }));
    } else {
        let paint = Paint::new();
        println!("{} {}  {}", paint.cyan("◆"), paint.bold(&p.id), util::truncate(text.trim(), 60));
    }
    0
}

/// `mv` reassigns a task's project, its place in the sub-task tree, or both.
///
/// The one rule the whole command exists to keep is the one `add --parent`
/// already keeps: a sub-task lives where its parent lives. Re-parenting is the
/// obvious back door around it, so here the project *follows* the parent —
/// naming a different one is refused, exactly as it is on `add` — and the move
/// carries the sub-tree, because otherwise the invariant survives at the task
/// that moved and breaks one level below it.
pub fn mv(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let tasks = store.tasks();

    if !args.has("project") && !args.has("parent") {
        eprintln!("usage: wsp mv <id> -p <project> | --parent <id> | --parent none");
        return 2;
    }

    // Resolve the task before anything is written: the sub-tree carry below
    // touches several files, and none of them should be touched at all if the
    // id the user typed does not name a task.
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp mv <id> -p <project> | --parent <id> | --parent none");
        return 2;
    };
    let Some(subject) = store.find_task(&needle) else {
        eprintln!("wsp: no task matching `{needle}`");
        return 1;
    };

    // `--parent` unset, `--parent none` and `--parent <id>` are three different
    // instructions, so the absent case has to stay distinguishable from the
    // detaching one.
    let new_parent: Option<Option<Task>> = match args.get("parent") {
        None => None,
        Some(p) if p == "none" || p == "root" || p == "top" => Some(None),
        Some(needle) => match store.find_task(&needle) {
            Some(p) => Some(Some(p)),
            None => {
                eprintln!("wsp: no such parent task `{needle}`");
                return 1;
            }
        },
    };

    // A cycle resolves at every step, so nothing downstream notices one: the
    // tree hangs a row beneath itself and `nest` draws the loop flat for ever.
    // `add` cannot reach this because a task being created has no children;
    // re-parenting is the only verb that can, which makes the check this
    // command's own to make.
    if let Some(Some(p)) = &new_parent {
        if p.id == subject.id {
            eprintln!("wsp: {} cannot be its own parent", subject.id);
            return 1;
        }
        if crate::resolve::descendants_of(&tasks, &subject.id).contains(&p.id) {
            eprintln!("wsp: {} is beneath {} — that would make a cycle", p.id, subject.id);
            return 1;
        }
    }

    let named_project = match args.get("project") {
        Some(p) if p == "none" || p == "inbox" => Some(None),
        Some(p) => match index.find(&p) {
            Some(found) => Some(Some(found.id.clone())),
            None => {
                eprintln!("wsp: no such project `{p}`");
                return 1;
            }
        },
        None => None,
    };

    // Where the task lands. A parent decides it; `-p` may only agree.
    let target: Option<String> = match (&new_parent, &named_project) {
        (Some(Some(p)), Some(named)) => {
            if &p.project != named {
                eprintln!(
                    "wsp: parent {} is in {}, not {}",
                    p.id,
                    p.project.clone().unwrap_or_else(|| "the inbox".into()),
                    named.clone().unwrap_or_else(|| "the inbox".into())
                );
                return 1;
            }
            p.project.clone()
        }
        (Some(Some(p)), None) => p.project.clone(),
        (_, Some(named)) => named.clone(),
        (_, None) => subject.project.clone(),
    };

    // Moving a task out from under a parent it is still attached to is the same
    // violation seen from the other side. Refuse it, and name the way out
    // rather than leaving the user to guess that `--parent none` exists.
    if new_parent.is_none() {
        if let Some(pid) = &subject.parent {
            if let Some(p) = tasks.iter().find(|t| &t.id == pid) {
                if p.project != target {
                    eprintln!(
                        "wsp: {} sits under {} in {} — move the parent, or detach it with `--parent none`",
                        subject.id,
                        p.id,
                        p.project.clone().unwrap_or_else(|| "the inbox".into())
                    );
                    return 1;
                }
            }
        }
    }

    // The sub-tree comes too. These land before `mutate` writes the task
    // itself, so the single commit it makes on its way out holds the whole
    // move — a sub-tree left in the old project by a half-applied move is
    // exactly the state the invariant exists to prevent.
    let mut carried = 0usize;
    if target != subject.project {
        for id in crate::resolve::descendants_of(&tasks, &subject.id) {
            let Some(mut kid) = tasks.iter().find(|t| t.id == id).cloned() else { continue };
            if kid.project == target {
                continue;
            }
            kid.project = target.clone();
            if kid.project.is_some() && kid.status() == Status::Inbox {
                kid.set_status(Status::Todo);
            }
            kid.log(&format!(
                "carried to {} with {}",
                target.clone().unwrap_or_else(|| "the inbox".into()),
                subject.id
            ));
            kid.touch();
            if let Err(e) = store.save_task(&kid) {
                eprintln!("wsp: write failed for {}: {e}", kid.id);
                return 1;
            }
            carried += 1;
        }
    }

    let verb = if new_parent.is_some() { "re-parented" } else { "moved" };
    mutate(store, args, verb, |t| {
        let was_project = t.project.clone();
        t.project = target.clone();
        if let Some(p) = &new_parent {
            t.parent = p.as_ref().map(|p| p.id.clone());
        }
        if t.project.is_some() && t.status() == Status::Inbox {
            t.set_status(Status::Todo);
        }
        match &new_parent {
            Some(Some(p)) => t.log(&format!("moved under {} in {}", p.id, label(&target))),
            Some(None) => t.log(&format!("detached to the top level of {}", label(&target))),
            None => t.log(&format!("moved to {}", label(&target))),
        }
        if carried > 0 {
            t.log(&format!(
                "{carried} sub-task{} carried from {}",
                if carried == 1 { "" } else { "s" },
                label(&was_project)
            ));
        }
    })
}

/// A project id, or the name we give the absence of one in prose.
fn label(project: &Option<String>) -> String {
    project.clone().unwrap_or_else(|| "the inbox".into())
}

pub fn tag(store: &Store, args: &Args) -> i32 {
    let changes: Vec<String> = args.rest.iter().skip(1).cloned().collect();
    if changes.is_empty() {
        eprintln!("usage: wsp tag <id> +dsp -ui");
        return 2;
    }
    mutate(store, args, "tagged", |t| {
        for c in &changes {
            if let Some(add) = c.strip_prefix('+') {
                if !add.is_empty() && !t.tags.iter().any(|x| x == add) {
                    t.tags.push(add.to_string());
                }
            } else if let Some(rm) = c.strip_prefix('-') {
                t.tags.retain(|x| x != rm);
            } else if !t.tags.iter().any(|x| x == c) {
                t.tags.push(c.clone());
            }
        }
    })
}

pub fn next(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let scope = match current_project(store, args, &index) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let scope_ids: Option<Vec<String>> = scope.as_ref().map(|s| index.subtree(s));

    let mut candidates: Vec<Task> = store
        .tasks()
        .into_iter()
        .filter(|t| matches!(t.status(), Status::Doing | Status::Todo | Status::Review))
        .filter(|t| match &scope_ids {
            Some(ids) => t.project.as_ref().map(|p| ids.contains(p)).unwrap_or(false),
            None => true,
        })
        .collect();

    candidates.sort_by(|a, b| {
        a.status()
            .rank()
            .cmp(&b.status().rank())
            .then(a.priority().rank().cmp(&b.priority().rank()))
            .then(a.id.cmp(&b.id))
    });

    let Some(t) = candidates.first() else {
        if args.json() {
            println!("null");
        } else {
            println!("nothing actionable");
        }
        return 0;
    };

    if args.json() {
        println!("{}", t.json());
    } else {
        let p = Paint::new();
        println!("{}  {}", p.bold(&t.id), t.title);
        println!(
            "{}",
            p.dim(&format!(
                "{}  {}  {}",
                t.project.clone().unwrap_or_else(|| "(inbox)".into()),
                t.status().as_str(),
                t.priority().as_str()
            ))
        );
    }
    0
}

/// Change a task's title without opening an editor. `edit` shells out to
/// `$EDITOR`, which is fine from a terminal and useless from anything that is
/// already drawing on the screen.
pub fn rename(store: &Store, args: &Args) -> i32 {
    let title = args.rest.get(1..).map(|r| r.join(" ")).unwrap_or_default();
    if title.trim().is_empty() {
        eprintln!("usage: wsp rename <id> \"new title\"");
        return 2;
    }
    mutate(store, args, "rename", |t| {
        t.log(&format!("renamed from \"{}\"", t.title));
        t.title = title.trim().to_string();
    })
}

/// Retire a task. The file moves to the archive rather than being deleted:
/// the store is a git repo, and a task carrying a decision log is worth more
/// recoverable than gone.
pub fn rm(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp rm <id>");
        return 2;
    };
    let Some(t) = store.find_task(&needle) else {
        eprintln!("wsp: no task matching `{needle}`");
        return 1;
    };
    let filed = match store.archive_task(&t) {
        Ok(name) => name,
        Err(e) => {
            eprintln!("wsp: archive failed: {e}");
            return 1;
        }
    };
    // A retired task must not go on holding a pane or a workspace. Nothing is
    // recorded on the way out: `worked` is a trace kept on a task, and after
    // this there is no task to keep it on.
    for pane in store.panes_for_task(&t.id) {
        store.clear_binding(&pane);
    }
    store.clear_claim(&t.id);
    store.log_event("task-removed", json!({ "id": t.id, "project": t.project, "title": t.title }));
    store.git_commit(&format!("wsp: rm {} — {}", t.id, t.title));
    if args.json() {
        println!("{}", json!({ "removed": t.id, "archived": true, "filed_as": filed }));
    } else if filed == t.id {
        println!("removed {} (archived)", t.id);
    } else {
        // Say it rather than let it pass: the archive already held that id,
        // which means two tasks have worn it.
        println!("removed {} (archived as {filed} — that id was already in the archive)", t.id);
    }
    0
}

/// Edit a task's prose, without ever showing anyone the frontmatter.
///
/// The frontmatter is a contract — `id`, `status`, `schema` — and every field
/// in it already has a command that sets it correctly. Handing the raw file to
/// an editor puts a typo in `status:` one keystroke away from a task the tools
/// can no longer read, for no benefit, because the part worth writing by hand
/// is the prose. So: the body only, and only the parts meant to be written.
///
/// `## Log` is excluded deliberately. It is dated and append-only; `wsp note`
/// is how you add to it, and editing history in place is how history stops
/// being evidence.
/// What `edit_prose` needs to know about the thing being edited. Tasks and
/// projects differ only in where the body lives and how it is written back.
pub struct Prose {
    pub what: &'static str,
    pub id: String,
    pub body: String,
    pub path: std::path::PathBuf,
}

pub fn edit(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp edit <id> [--overview | --details | --raw]");
        return 2;
    };
    let Some(t) = store.find_task(&needle) else {
        eprintln!("wsp: no task matching `{needle}`");
        return 1;
    };
    edit_prose(
        store,
        args,
        Prose {
            what: "task",
            id: t.id.clone(),
            body: t.body.clone(),
            path: store.task_path(&t.id),
        },
    )
}

/// Where prose comes from when nobody is typing it: `--from <path>`, or a bare
/// `-` for stdin, spelled the way every other tool spells it.
///
/// `-` is a positional rather than `--from`'s value because a lone dash is not
/// a path — it is the conventional name for the stream, and taking it as an
/// argument is what `cat -` and `git apply -` have always done.
fn prose_source(args: &Args) -> Option<String> {
    if args.rest.iter().any(|a| a == "-") {
        return Some("-".into());
    }
    match args.get("from") {
        // `--from` with nothing usable after it. A missing path is a mistake
        // worth reporting rather than an editor session nobody asked for.
        Some(v) if v == "true" => Some("-".into()),
        other => other,
    }
}

fn read_source(src: &str) -> std::io::Result<String> {
    if src == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(s);
    }
    std::fs::read_to_string(util::expand(src))
}

/// Edit prose without ever showing anyone the frontmatter.
///
/// The frontmatter is a contract — `id`, `status`, `schema` — and every field
/// in it already has a command that sets it correctly. Handing the raw file to
/// an editor puts a typo one keystroke away from something the tools can no
/// longer read, for no benefit, because the part worth writing by hand is the
/// prose. So: the body only, and only the parts meant to be written.
///
/// `## Log` is excluded deliberately. It is dated and append-only; `wsp note`
/// is how you add to it, and editing history in place is how history stops
/// being evidence.
pub fn edit_prose(store: &Store, args: &Args, item: Prose) -> i32 {
    // The escape hatch, for when the frontmatter itself is what is wrong.
    if args.has("raw") {
        if prose_source(args).is_some() {
            // `--raw` is the one path that reaches the frontmatter, and the
            // whole point of `--from` is that nobody is reading what goes by.
            // A generated file landing on `status:` is not an edit, it is a
            // task the tools can no longer read.
            eprintln!("wsp: --from writes prose; --raw is for a person and a frontmatter that is wrong");
            return 2;
        }
        return edit_file(store, &item.path, &format!("edit {}", item.id));
    }

    let one = if args.has("overview") {
        Some("Overview")
    } else if args.has("details") {
        Some("Details")
    } else {
        None
    };

    let before = match one {
        Some(sec) => crate::model::section_of(&item.body, sec).unwrap_or_default(),
        None => {
            let mut b = String::new();
            for sec in ["Overview", "Details"] {
                b.push_str(&format!("## {sec}\n"));
                let text = crate::model::section_of(&item.body, sec).unwrap_or_default();
                if !text.trim().is_empty() {
                    b.push_str(text.trim_end());
                    b.push('\n');
                }
                b.push('\n');
            }
            b
        }
    };

    // Where the new prose comes from. `$EDITOR` is the answer for a person and
    // no answer at all for anything else: an agent that can only ever append a
    // log line writes tasks with a title and nothing under it, which is most of
    // what makes a decomposed task unreadable a day later.
    let after = match prose_source(args) {
        Some(src) => match read_source(&src) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("wsp: cannot read {}: {e}", if src == "-" { "stdin" } else { &src });
                return 1;
            }
        },
        None => {
            // A directory per edit, so the file inside can be named for the
            // section. Every terminal editor puts the filename in its status
            // line, which makes that the one label needing no cooperation from
            // the editor at all.
            let dir =
                std::env::temp_dir().join(format!("wsp-{}-{}", item.id, util::epoch_nanos()));
            let _ = std::fs::create_dir_all(&dir);
            let tmp = dir.join(format!(
                "{}.md",
                one.map(|s| s.to_lowercase()).unwrap_or_else(|| "body".into())
            ));
            if let Err(e) = crate::store::write_atomic(&tmp, &before) {
                eprintln!("wsp: cannot stage the edit: {e}");
                return 1;
            }
            let code = launch_editor(&tmp);
            let text = std::fs::read_to_string(&tmp).unwrap_or_default();
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_dir(&dir);
            if code != 0 {
                eprintln!("wsp: editor exited {code} — nothing written");
                return 1;
            }
            text
        }
    };
    // Trailing whitespace is not an edit. It matters more than it sounds:
    // prose arriving from a file rather than an editor differs by a newline
    // often enough that an agent rewriting the same brief would otherwise
    // touch the task and make a commit every time.
    if after.trim_end() == before.trim_end() {
        if !args.json() {
            println!("unchanged");
        }
        return 0;
    }

    // Re-read before writing. An edit lasts as long as someone is typing and
    // the file is shared — a note, a status change, a claim — so writing back
    // the copy we opened with would silently undo whatever happened between.
    let mut body = match item.what {
        "project" => match store.project(&item.id) {
            Some(p) => p.body,
            None => {
                eprintln!("wsp: {} disappeared while you were editing", item.id);
                return 1;
            }
        },
        _ => match store.task(&item.id) {
            Some(t) => t.body,
            None => {
                eprintln!("wsp: {} disappeared while you were editing", item.id);
                return 1;
            }
        },
    };

    match one {
        Some(sec) => crate::model::set_section_in(&mut body, sec, &after),
        None => {
            let ov = crate::model::section_of(&after, "Overview");
            let de = crate::model::section_of(&after, "Details");
            if ov.is_none() && de.is_none() {
                // Both headings were deleted and something was typed anyway.
                // Keeping it under Overview is a guess; discarding it is not a
                // guess, it is losing the only thing the user actually wrote.
                crate::model::set_section_in(&mut body, "Overview", after.trim());
            } else {
                let loose = crate::model::section_of(&after, "").unwrap_or_default();
                let overview = match (loose.trim().is_empty(), ov) {
                    (true, o) => o.unwrap_or_default(),
                    (false, Some(o)) => format!("{}\n\n{}", loose.trim(), o),
                    (false, None) => loose.trim().to_string(),
                };
                crate::model::set_section_in(&mut body, "Overview", &overview);
                crate::model::set_section_in(&mut body, "Details", &de.unwrap_or_default());
            }
        }
    }

    let saved = match item.what {
        "project" => store.project(&item.id).map(|mut p| {
            p.body = body;
            store.save_project(&p)
        }),
        _ => store.task(&item.id).map(|mut t| {
            t.body = body;
            t.touch();
            store.save_task(&t)
        }),
    };
    match saved {
        Some(Ok(())) => {}
        Some(Err(e)) => {
            eprintln!("wsp: write failed: {e}");
            return 1;
        }
        None => {
            eprintln!("wsp: {} disappeared while you were editing", item.id);
            return 1;
        }
    }

    store.log_event(
        &format!("{}-edited", item.what),
        json!({ "id": item.id, "section": one.unwrap_or("body") }),
    );
    store.git_commit(&format!("wsp: edit {} {}", item.what, item.id));

    if args.json() {
        println!("{}", json!({ "id": item.id, "edited": one.unwrap_or("body") }));
    } else {
        println!("edited {}", item.id);
    }
    0
}

fn launch_editor(path: &std::path::Path) -> i32 {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    match std::process::Command::new(&editor).arg(path).status() {
        Ok(s) => s.code().unwrap_or(0),
        Err(e) => {
            eprintln!("wsp: cannot launch {editor}: {e}");
            127
        }
    }
}

fn edit_file(store: &Store, path: &std::path::Path, msg: &str) -> i32 {
    let code = launch_editor(path);
    if code == 0 {
        store.git_commit(&format!("wsp: {msg}"));
    }
    code
}

pub fn archive(store: &Store, args: &Args) -> i32 {
    let cutoff: i64 = args.get("days").and_then(|d| d.parse().ok()).unwrap_or(30);
    let all = args.has("all");
    let mut moved = 0;
    for t in store.tasks() {
        if t.status() != Status::Done {
            continue;
        }
        if !all && util::age_days(&t.updated) < cutoff {
            continue;
        }
        if store.archive_task(&t).is_ok() {
            moved += 1;
        }
    }
    if moved > 0 {
        store.git_commit(&format!("wsp: archive {moved} tasks"));
    }
    if args.json() {
        println!("{}", json!({ "archived": moved }));
    } else {
        println!("archived {moved} task(s)");
    }
    0
}
