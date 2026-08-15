//! Task commands.

use serde_json::json;

use crate::cmd_agent::current_project;
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
    let project = if args.has("inbox") {
        None
    } else {
        match current_project(store, args, &index) {
            Ok(p) => p,
            Err(code) => return code,
        }
    };

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
    if let Some(parent) = args.get("parent") {
        match store.find_task(&parent) {
            Some(p) => t.parent = Some(p.id),
            None => {
                eprintln!("wsp: no such parent task `{parent}`");
                return 1;
            }
        }
    }

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
    for t in tasks {
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
            "  {} {} {} {}{}",
            p.dim(&util::pad(&t.id, idw)),
            st,
            prio,
            util::truncate(&t.title, 62),
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

    for (pane, b) in store.bindings() {
        if b.get("task_id").and_then(|x| x.as_str()) == Some(t.id.as_str()) {
            println!("{} {}", p.dim(&util::pad("agent", 9)), pane);
        }
    }
    if !t.body.trim().is_empty() {
        println!("\n{}", t.body.trim());
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
    let code = mutate(store, args, "done", |t| {
        t.set_status(Status::Done);
        t.log("→ done");
    });
    if code == 0 {
        // Free any pane still holding it.
        if let Some(needle) = args.rest.first() {
            if let Some(t) = store.find_task(needle) {
                let panes: Vec<String> = store
                    .bindings()
                    .iter()
                    .filter(|(_, b)| b.get("task_id").and_then(|x| x.as_str()) == Some(t.id.as_str()))
                    .map(|(pane, _)| pane.clone())
                    .collect();
                for pane in panes {
                    store.clear_binding(&pane);
                }
            }
        }
    }
    code
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

pub fn mv(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let target = match args.get("project") {
        Some(p) if p == "none" || p == "inbox" => None,
        Some(p) => match index.find(&p) {
            Some(found) => Some(found.id.clone()),
            None => {
                eprintln!("wsp: no such project `{p}`");
                return 1;
            }
        },
        None => {
            eprintln!("usage: wsp mv <id> -p <project>");
            return 2;
        }
    };
    mutate(store, args, "moved", |t| {
        t.project = target.clone();
        if t.project.is_some() && t.status() == Status::Inbox {
            t.set_status(Status::Todo);
        }
        t.log(&format!("moved to {}", target.clone().unwrap_or_else(|| "inbox".into())));
    })
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
    if let Err(e) = store.archive_task(&t) {
        eprintln!("wsp: archive failed: {e}");
        return 1;
    }
    store.log_event("task-removed", json!({ "id": t.id, "project": t.project, "title": t.title }));
    store.git_commit(&format!("wsp: rm {} — {}", t.id, t.title));
    if args.json() {
        println!("{}", json!({ "removed": t.id, "archived": true }));
    } else {
        println!("removed {} (archived)", t.id);
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

    // A directory per edit, so the file inside can be named for the section.
    // Every terminal editor puts the filename in its status line, which makes
    // that the one label needing no cooperation from the editor at all.
    let dir = std::env::temp_dir().join(format!("wsp-{}-{}", item.id, util::epoch_nanos()));
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
    let after = std::fs::read_to_string(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_dir(&dir);

    if code != 0 {
        eprintln!("wsp: editor exited {code} — nothing written");
        return 1;
    }
    if after == before {
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
