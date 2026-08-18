//! Task commands.

use serde_json::json;

use crate::cmd_agent::{claim_line, current_project, worked_line};
use crate::herdr;
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

    let id = match store.alloc_task_id(project.as_deref()) {
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

/// `wsp find <text>` — the store's finding aid.
///
/// Ed, 2026-08-17: *"I'm struggling to find issues in this list now we have
/// literally hundreds."* Two hundred and seventy-six tasks across thirty-one
/// projects, and until this the only way to look for one you half-remembered
/// was `wsp ls` with a project filter, and reading.
///
/// A `grep` over the tasks the store has already read, and deliberately
/// nothing more. No ranking, no scoring, no index: at this size an index would
/// buy nothing measurable and cost a second source of truth to keep current,
/// which is the one thing this store must not grow. If it ever needs one it
/// has stopped being basic.
///
/// Scoped the way [`list`] is — the project you are standing in, `--all` to
/// widen — because a search from inside `robustness` that answers with `vst`
/// is noise. The half that makes a scope safe is the line at the bottom: when
/// the scope has nothing and the store does, it says how many and what to
/// type. A default scope you cannot see past is a dead end, and a dead end is
/// what sends somebody back to reading the whole list.
pub fn find(store: &Store, args: &Args) -> i32 {
    let needle = args.text(0);
    let needle = needle.trim();
    if needle.is_empty() {
        eprintln!("usage: wsp find <text> [-p project] [-s status] [--all]");
        return 2;
    }

    let index = Index::new(store.projects());
    let scope = if args.has("all") && !args.has("project") {
        None
    } else {
        match current_project(store, args, &index) {
            Ok(p) => p,
            Err(code) => return code,
        }
    };

    let hits: Vec<Task> = filtered(store, args, &index, scope.clone())
        .into_iter()
        .filter(|t| t.matches(needle))
        .collect();

    if args.json() {
        let out: Vec<_> = hits.iter().map(|t| t.json()).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }

    let p = Paint::new();
    if hits.is_empty() {
        println!("{}", p.dim(&nothing(store, &index, args, scope.as_deref(), needle)));
        return 0;
    }

    let where_ = match &scope {
        Some(s) => format!(" in {s}"),
        None => String::new(),
    };
    println!("{}", p.dim(&format!("{} matching \"{needle}\"{where_}", hits.len())));

    // A short word over a store this size answers with a hundred rows, and
    // most of what runs this is an agent that pays for every one of them in
    // context on every request afterwards. So the list stops, and says it has.
    // The order is the same one `ls` uses — what is in flight first — so what
    // is cut is the tail of the backlog rather than an arbitrary slice.
    //
    // The count above is the *whole* answer either way, which is what makes a
    // cut list honest: you can see that the phrase was too broad before you
    // have read a line of it. `--json` is uncapped, because a caller parsing
    // this is not reading it.
    let shown = match args.has("full") {
        true => hits.len(),
        false => hits.len().min(FIND_MAX),
    };
    print_hits(&hits[..shown], needle, &p);
    if shown < hits.len() {
        println!(
            "{}",
            p.dim(&format!("  … {} more · wsp find {} --full", hits.len() - shown, quoted(needle)))
        );
    }
    0
}

/// Hits printed before the list says how many it is not printing. Twenty is
/// most of a terminal once the prose lines are counted, and past that a search
/// is telling you to narrow the phrase rather than to scroll.
const FIND_MAX: usize = 20;

/// The phrase as it would have to be typed back. A suggested command that has
/// to be edited before it runs is one nobody runs.
fn quoted(needle: &str) -> String {
    match needle.contains(char::is_whitespace) {
        true => format!("\"{needle}\""),
        false => needle.to_string(),
    }
}

/// What to say when the search found nothing, which is the answer a scoped
/// search most often owes an explanation for.
///
/// The scope is a default nobody typed, so "no" on its own is a dead end: it
/// reads as "that task does not exist" when what it means is "not here". So
/// this looks once more, without the scope and without the status filter, and
/// says which of the two was in the way — and then, when the store genuinely
/// has nothing, whether the archive does. `--all` widens both at once, so both
/// halves point at the same key.
///
/// Only when nothing was found: three passes over the tasks already in memory,
/// on a search that has otherwise printed a blank.
///
/// An explicit `-s` or `-t` is left alone. Those are filters somebody typed and
/// can see; guessing at them would produce a sentence about `--all` that is not
/// true of what `--all` would do with the flags still on.
fn nothing(store: &Store, index: &Index, args: &Args, scope: Option<&str>, needle: &str) -> String {
    let matched: Vec<Task> = store.tasks().into_iter().filter(|t| t.matches(needle)).collect();
    let narrowed = args.has("status") || args.has("tag");
    let typed = quoted(needle);
    if !narrowed {
        let ids = scope.map(|s| index.subtree(s));
        let inside = |t: &Task| match &ids {
            Some(ids) => t.project.as_ref().map(|p| ids.contains(p)).unwrap_or(false),
            None => true,
        };
        let elsewhere = matched.iter().filter(|t| !inside(t)).count();
        if let (Some(s), 1..) = (scope, elsewhere) {
            return format!(
                "nothing matching \"{needle}\" in {s} — {elsewhere} elsewhere · wsp find {typed} --all"
            );
        }
        let finished = matched.iter().filter(|t| inside(t) && !t.status().is_open()).count();
        if finished > 0 {
            return format!(
                "nothing open matching \"{needle}\" — {finished} finished · wsp find {typed} --all"
            );
        }
    }
    // Nothing live at all. A task can still be in the archive, which is where
    // `done` work goes after thirty days — and "I know I wrote that down" is
    // exactly the search that ends there.
    match store.archived_tasks().iter().filter(|t| t.matches(needle)).count() {
        0 => format!("nothing matching \"{needle}\""),
        n => format!("nothing matching \"{needle}\" — {n} in the archive"),
    }
}

/// A hit list is not a tree: the rows come from wherever they come from, so
/// each carries its project, and none is indented under a parent that may not
/// be in the list at all.
///
/// The second line is the whole reason prose is searched. A row whose title
/// says nothing about what you typed looks like a mistake until it shows the
/// line that put it there — and that line is usually the answer to "which of
/// these is the one", which is the actual question a search is asked.
fn print_hits(hits: &[Task], needle: &str, p: &Paint) {
    let idw = hits.iter().map(|t| t.id.chars().count()).max().unwrap_or(12);
    for t in hits {
        let st = match t.status() {
            Status::Doing => p.cyan(&util::pad("doing", 8)),
            Status::Blocked => p.red(&util::pad("blocked", 8)),
            Status::Review => p.yellow(&util::pad("review", 8)),
            other => p.dim(&util::pad(other.as_str(), 8)),
        };
        println!(
            "  {} {} {} {}{}",
            p.dim(&util::pad(&t.id, idw)),
            st,
            paint_prio(p, t.priority()),
            util::truncate(&t.title, 62),
            p.dim(&format!("  [{}]", t.project.clone().unwrap_or_else(|| "inbox".into())))
        );
        if !t.title.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()) {
            if let Some(line) = t.prose_line(needle, 70) {
                println!("  {}{}", " ".repeat(idw + 1), p.dim(&line));
            }
        }
    }
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

/// The priority column, in the colours a terminal list uses: the one that
/// means "a decision is owed here" for `high`, and structure for `low`, which
/// is a task saying it is content to wait.
pub fn paint_prio(p: &Paint, prio: Priority) -> String {
    match prio {
        Priority::High => p.yellow(prio.mark()),
        Priority::Low => p.dim(prio.mark()),
        Priority::Normal => prio.mark().to_string(),
    }
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
        let prio = paint_prio(p, t.priority());
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
    let t = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
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
    //
    // Marked the way `project show` marks them, and for the same reason: an
    // entry a later one supersedes still states its rule, and a reader who
    // stops at the first line that answers their question stops at the wrong
    // one. The id column is what makes `superseded by d2` a row you can find.
    let decided = crate::model::decisions_of(&t.body);
    let over = crate::model::supersessions(&decided);
    if !decided.is_empty() {
        println!("\n{}", p.dim("DECISIONS"));
        for (d, superseded) in decided.iter().zip(&over) {
            let head = format!("{}  {}", p.dim(&util::pad(&d.when, 10)), p.dim(&util::pad(&d.id, 3)));
            match superseded {
                Some(by) => println!(
                    "  {head}  {}{}",
                    p.strike(&d.text),
                    p.dim(&format!("  · superseded by {by}"))
                ),
                None => println!("  {head}  {}", d.text),
            }
        }
    }
    // `## Log` goes out with the rest of the body, so the stamps in it are
    // converted here — the same date the DECISIONS block above just showed,
    // arrived at the same way.
    let mut rest = crate::model::localise_dates(&t.body);
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
    mutate_saying(store, args, verb, None, f)
}

/// `mutate`, with a word about what changed on the end of the receipt.
///
/// The line `mutate` prints is the task's status, which is the whole receipt
/// for the six verbs that change it and says nothing at all for one whose job
/// is a different field — `wsp prio 047 low` would answer `· t-260815-047
/// todo`, which is true and is not what was asked about.
fn mutate_saying<F>(store: &Store, args: &Args, verb: &str, said: Option<&str>, f: F) -> i32
where
    F: FnOnce(&mut Task),
{
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp {verb} <id>");
        return 2;
    };
    let mut t = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
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
            Status::Parked => p.dim("▪"),
            Status::Doing => p.cyan("▸"),
            _ => p.dim("·"),
        };
        println!("{} {}  {}  {}", mark, p.bold(&t.id), p.dim(t.status().as_str()), t.title);
        if let Some(s) = said {
            println!("  {}", p.dim(s));
        }
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
            if store.clear_binding(&pane) {
                // And the name with it. This is the commonest way work is put
                // down — an agent finishing and going spare is exactly the row
                // the agents panel exists to draw — and until now it was the
                // one door that left the pane wearing the task, so the panel
                // showed the free agent still holding what it had just
                // finished. While the task is still readable: the name only
                // comes off where it is one we wrote, which is a question
                // about the title.
                crate::cmd_agent::unname_after_task(store, &pane, &t.id);
            }
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

/// Deliberately not yet — the other half of what `block` used to mean.
///
/// Demands a reason for the same procedural reason `block` does, and wants a
/// different kind of sentence: not the question somebody owes you, but the
/// condition that should bring the task back. Writing that down was already
/// the habit before there was a status for it — t-260816-022 recorded its
/// three revisit conditions unprompted, and then nothing read them, because to
/// every list on the machine it was one more red row. It is a log line here
/// too, and the status is what makes the log line findable: `wsp ls -s parked`
/// is now a question you can ask.
pub fn park(store: &Store, args: &Args) -> i32 {
    let reason = args.text(1);
    if reason.trim().is_empty() {
        eprintln!("usage: wsp park <id> \"what would bring it back\"");
        return 2;
    }
    mutate(store, args, "parked", |t| {
        t.set_status(Status::Parked);
        t.log(&format!("parked: {}", reason.trim()));
    })
}

/// Is this payload naming a stream to read the prose from, rather than being
/// the prose?
///
/// `wsp edit <id> --overview -` is the idiom this store teaches for prose: it
/// is in the handbook, in the brief's rules, and in most tasks written here. So
/// an agent with a paragraph to log generalises it and writes `wsp note <id>
/// --from -`. Until this, `note` had no stdin form and took the whole line as
/// its text — it exited 0, printed the ordinary receipt, and recorded a log
/// entry whose entire body was `-`. On 2026-08-17 that destroyed
/// `robustness-068`'s review note in full, and the agent found out by re-reading
/// its own log. A verb that refuses is a verb you learn; this one succeeded.
///
/// So the idiom is answered rather than refused, in both spellings `edit`
/// accepts. `--from` arrives here as a *positional* because `note` and `decide`
/// stop parsing flags after their subject (`LITERAL_AFTER` in `main.rs`) — the
/// rule that keeps `wsp note 028 "--parent only exists on add"` intact. The two
/// rules pull against each other and this is where they meet.
///
/// Only a payload that is *entirely* a source counts. Prose here is mostly
/// about the CLI, so a note that merely begins with `--from` is a sentence
/// somebody meant, and `--` is still the escape hatch under everything.
fn payload_source(payload: &[String]) -> Option<String> {
    match payload {
        // A lone `-`, and `--from` with nothing usable after it: both name the
        // stream, the second because `--from` is not worth an editor session
        // nobody asked for. Same reading as `prose_source` gives `edit`.
        [one] if one == "-" || one == "--from" => Some("-".into()),
        [one] => one.strip_prefix("--from=").map(str::to_string),
        [flag, path] if flag == "--from" => Some(path.clone()),
        _ => None,
    }
}

/// One entry is one line, so text that arrives on several becomes one.
///
/// Not tidying: the `## Log` section is read line-by-line by everything that
/// reads it — `brief` shows the last few *lines* as the last few entries,
/// `blocked_question` scans for one, `localise_dates` rewrites the stamp at the
/// head of each. A forty-line survey pasted in whole is forty entries to all of
/// them, and it would push every other entry out of the brief that every
/// session on the task pays for. Folding keeps every word and costs the
/// paragraph breaks; the alternative costs the reader.
fn fold(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The prose for a log entry or a decision: the rest of the line, or the stream
/// it names. `Err` is the exit code, with the reason already on stderr.
fn prose_payload(args: &Args, usage: &str) -> Result<(String, Option<String>), i32> {
    let Some(src) = payload_source(args.rest.get(1..).unwrap_or_default()) else {
        let text = fold(&args.text(1));
        if text.is_empty() {
            eprintln!("usage: {usage}");
            return Err(2);
        }
        return Ok((text, None));
    };
    // Named the way the rest of the CLI names a path — a receipt that says
    // `~/notes/survey.md` is one the reader recognises.
    let named = if src == "-" { "stdin".to_string() } else { util::contract(&util::expand(&src)) };
    if src == "-" && util::stdin_is_tty() {
        // Reading a terminal is not an empty note, it is a command that stops
        // and says nothing while it swallows the keys. The one failure worse
        // than the silent one this task is about.
        eprintln!("wsp: nothing is piped in — `-` reads the text from a stream");
        return Err(2);
    }
    let text = match read_source(&src) {
        Ok(raw) => fold(&raw),
        Err(e) => {
            eprintln!("wsp: cannot read {named}: {e}");
            return Err(1);
        }
    };
    if text.is_empty() {
        // The whole failure, in one condition: an empty stream is what a bare
        // `-` used to mean, and recording it is how the note went missing. An
        // error costs a retry; success costs the prose.
        eprintln!("wsp: nothing on {named} — nothing recorded");
        return Err(2);
    }
    Ok((text, Some(named)))
}

pub fn note(store: &Store, args: &Args) -> i32 {
    let (text, from) = match prose_payload(args, "wsp note <id> \"text\"   (or `-` to read it from stdin)") {
        Ok(v) => v,
        Err(code) => return code,
    };
    // What came off a stream nobody watched, said back. The receipt for the
    // typed form is the text itself, on the terminal above; for this one it is
    // this line or nothing at all.
    let said = from.map(|src| format!("{} characters from {src} — {}", text.len(), util::truncate(&text, 48)));
    mutate_saying(store, args, "note", said.as_deref(), |t| t.log(&text))
}

/// Refuse a `--supersedes` that names nothing on this body.
///
/// Numbers the section first, because the ids a caller is about to name are the
/// ones `show` printed — and `show` prints what is stored, so an unnumbered
/// file has none to name yet. Working on a copy: this is the check, and the
/// write that follows numbers the real body itself.
fn check_supersedes(body: &str, supersedes: &[String], id: &str) -> Result<(), i32> {
    if supersedes.is_empty() {
        return Ok(());
    }
    let mut copy = body.to_string();
    crate::model::number_decisions(&mut copy);
    let have: Vec<String> = crate::model::decisions_of(&copy).into_iter().map(|d| d.id).collect();
    for want in supersedes {
        if !have.iter().any(|h| h == want) {
            eprintln!(
                "wsp: no decision `{want}` on {id} — it has {}",
                if have.is_empty() { "none".to_string() } else { have.join(" ") }
            );
            return Err(1);
        }
    }
    Ok(())
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
///
/// It reads its prose the same way `note` does — see [`payload_source`]. The
/// two are one act at different heights and an agent that has learned `-` on
/// one will type it on the other; a verb that answered `-` with a decision
/// whose whole text was `-` would be the same defect one file along.
///
/// `--supersedes <id>` is how the later entry names the earlier one it
/// replaces, and it is the whole reason a decision has an id. Without it the
/// correction and the thing corrected are two dated bullets that only a reader
/// of the full text can join — and `project show` abridges to the first
/// sentence, so the withdrawn rule reads as the live one. Recorded in the line,
/// so the link is in the file rather than in a renderer.
///
/// The reference is checked against the entries actually there. A typo that
/// wrote itself into the record as `supersedes d9` would mark nothing, look
/// exactly like a decision that supersedes nothing, and be found by whoever
/// wondered why the old rule was still printed.
pub fn decide(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp decide <task|project> \"what was settled, and why\" [--supersedes <id>]";
    let (text, from) = match prose_payload(args, USAGE) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let mut supersedes: Vec<String> = Vec::new();
    for raw in args.all("supersedes") {
        match crate::model::decision_ref(&raw) {
            Some(id) => supersedes.push(id),
            None => {
                eprintln!("wsp: `{raw}` is not a decision id — they look like `d4`");
                return 2;
            }
        }
    }

    if let Some(t) = store.find_task(&needle) {
        if let Err(code) = check_supersedes(&t.body, &supersedes, &t.id) {
            return code;
        }
        let said = from.map(|src| format!("{} characters from {src}", text.len()));
        return mutate_saying(store, args, "decided", said.as_deref(), |t| {
            crate::model::append_decision(&mut t.body, &text, &supersedes);
        });
    }

    let index = Index::new(store.projects());
    let Some(mut p) = index.find(&needle).cloned() else {
        eprintln!("wsp: no task or project matching `{needle}`");
        return 1;
    };
    if let Err(code) = check_supersedes(&p.body, &supersedes, &p.id) {
        return code;
    }
    crate::model::append_decision(&mut p.body, &text, &supersedes);
    if let Err(e) = store.save_project(&p) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("project-decided", json!({ "id": p.id, "text": text }));
    store.git_commit(&format!("wsp: decide {} — {}", p.id, util::truncate(&text, 60)));

    if args.json() {
        println!("{}", json!({ "project": p.id, "decisions": crate::model::decisions(&p.body).len() }));
    } else {
        let paint = Paint::new();
        println!("{} {}  {}", paint.cyan("◆"), paint.bold(&p.id), util::truncate(&text, 60));
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
    let subject = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
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

    // Filing an inbox task is the one place an id is allowed to change, and
    // the reason is that until a task has a project it has no space to be
    // continuous in. The new ids are reserved *now*, before anything is
    // written, so that the log each task keeps can name the id it is about to
    // take — a rename recorded only in the file it renamed is a rename nobody
    // reading the old id can follow.
    let mut renumber: std::collections::BTreeMap<String, String> = Default::default();
    if let Some(into) = &target {
        let mut filing: Vec<String> = Vec::new();
        if is_inbox_id(&subject.id) {
            filing.push(subject.id.clone());
        }
        for id in crate::resolve::descendants_of(&tasks, &subject.id) {
            if is_inbox_id(&id) {
                filing.push(id);
            }
        }
        for old in filing {
            match store.alloc_task_id(Some(into)) {
                Ok(new) => {
                    renumber.insert(old, new);
                }
                Err(e) => {
                    eprintln!("wsp: cannot allocate an id in {into}: {e}");
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
            if let Some(new) = renumber.get(&kid.id) {
                kid.log(&format!("filed out of the inbox: {} is now {new}", kid.id));
            }
            kid.touch();
            if let Err(e) = store.save_task(&kid) {
                eprintln!("wsp: write failed for {}: {e}", kid.id);
                return 1;
            }
            carried += 1;
        }
    }

    // `--parent none` on work that was never a sub-task changed no parent, and
    // saying otherwise is not a detail: the panel sends it on every move to a
    // project, so that landing on a project row means the top level of it —
    // which makes "detached" the commonest thing in the log and the rarest
    // thing to have happened. The receipt, the event and the commit line all
    // come off this word, so it has to answer what the task did, not what the
    // flags said.
    let attaches = matches!(new_parent, Some(Some(_)));
    let detaches = matches!(new_parent, Some(None)) && subject.parent.is_some();
    let verb = if attaches || detaches { "re-parented" } else { "moved" };
    let subject_new = renumber.get(&subject.id).cloned();
    let code = mutate(store, args, verb, |t| {
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
            Some(None) if detaches => {
                t.log(&format!("detached to the top level of {}", label(&target)))
            }
            _ => t.log(&format!("moved to {}", label(&target))),
        }
        if let Some(new) = &subject_new {
            t.log(&format!("filed out of the inbox: {} is now {new}", t.id));
        }
        if carried > 0 {
            t.log(&format!(
                "{carried} sub-task{} carried from {}",
                if carried == 1 { "" } else { "s" },
                label(&was_project)
            ));
        }
    });
    if code != 0 || renumber.is_empty() {
        return code;
    }

    // Last, because everything above still refers to the old ids and the
    // rename is what makes those references stale. It rewrites them all, and
    // records what became what so the old id goes on resolving afterwards.
    match store.rename_tasks(&renumber) {
        Ok((files, refs)) => {
            store.git_commit(&format!(
                "wsp: file {} out of the inbox into {}",
                renumber.len(),
                label(&target)
            ));
            let p = Paint::new();
            for (old, new) in &renumber {
                println!("  {} {} → {}", p.dim("renumbered"), p.dim(old), p.bold(new));
            }
            if refs > 0 {
                println!("  {}", p.dim(&format!("{refs} reference(s) rewritten across {files} file(s)")));
            }
            0
        }
        Err(e) => {
            eprintln!("wsp: the move landed but the renumbering did not: {e}");
            1
        }
    }
}

/// Is this id in the inbox's numbering space — the one space whose ids are not
/// permanent, because a task with no project has no space to be continuous in?
fn is_inbox_id(id: &str) -> bool {
    id.strip_prefix(&format!("{}-", crate::store::INBOX_CODE))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// A project id, or the name we give the absence of one in prose.
fn label(project: &Option<String>) -> String {
    project.clone().unwrap_or_else(|| "the inbox".into())
}

/// `wsp tag <id> +dsp -ui` — adjust tags, in the vocabulary the help documents.
///
/// The `-ui` half of that line used to be eaten by the flag parser: it reached
/// `Args` as a flag named `ui`, never arrived in `rest`, and the `+dsp` in the
/// same line supplied the positional that made the command look like it had
/// been given everything. It added the tag, said so, and exited 0 having
/// silently dropped the removal. [`crate::LITERAL_AFTER`] is where that is
/// fixed, once, for this command and for the prose ones it shares the defect
/// with.
///
/// The second half is here: a removal that names a tag the task does not carry
/// leaves on stderr and exits 1. It is not a write error and nothing is rolled
/// back — the additions in the same line stand — but a tag that stays on a
/// task, on a command that reported success, is a tag nobody can see the
/// reason for. Saying so is cheap; the silence is what cost three attempts.
pub fn tag(store: &Store, args: &Args) -> i32 {
    let changes: Vec<String> = args.rest.iter().skip(1).cloned().collect();
    if changes.is_empty() {
        eprintln!("usage: wsp tag <id> +dsp -ui");
        return 2;
    }
    let mut absent: Vec<String> = Vec::new();
    let code = mutate(store, args, "tagged", |t| {
        for c in &changes {
            if let Some(add) = c.strip_prefix('+') {
                if !add.is_empty() && !t.tags.iter().any(|x| x == add) {
                    t.tags.push(add.to_string());
                }
            } else if let Some(rm) = c.strip_prefix('-') {
                if !t.tags.iter().any(|x| x == rm) {
                    absent.push(rm.to_string());
                }
                t.tags.retain(|x| x != rm);
            } else if !t.tags.iter().any(|x| x == c) {
                t.tags.push(c.clone());
            }
        }
    });
    if code != 0 {
        return code;
    }
    if !absent.is_empty() {
        eprintln!(
            "wsp: nothing to remove — no {} on this task",
            absent.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(" or ")
        );
        return 1;
    }
    0
}

/// Change a task's priority after it exists.
///
/// `--prio` on `add` was the only way to set this, which put the decision at
/// the one moment you know least about the work — and left it there. Nothing
/// could move it afterwards: no `set`, no flag on any other verb, and
/// `project set k=v` is projects-only. It bit a phase swap in
/// strata-prototype, where the two phases traded places and `high` stayed on
/// the one that had become later, pointing every agent asking `wsp next` at
/// the wrong end of the plan.
///
/// A verb of its own rather than `wsp set <id> priority=high`: every other
/// field here is changed by a verb that says what it means, and a second,
/// general spelling of them all is a worse thing to own than one more line of
/// help.
pub fn prio(store: &Store, args: &Args) -> i32 {
    let Some(level) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp prio <id> high|normal|low");
        return 2;
    };
    let Some(want) = Priority::parse(&level) else {
        eprintln!("wsp: priority must be high|normal|low");
        return 2;
    };
    // Setting the level it already has is not a change, and `mutate` cannot
    // tell: it would spend a log line, an event and a commit recording that
    // somebody typed what was already true. The panel's one key cycles, so
    // this is reachable by accident from there as well as by a script.
    if let Some(t) = args.rest.first().and_then(|needle| store.find_task(needle)) {
        if t.priority() == want {
            if args.json() {
                println!("{}", t.json());
            } else {
                println!("{} already {}", t.id, want.as_str());
            }
            return 0;
        }
    }
    mutate_saying(store, args, "prio", Some(&format!("priority {}", want.as_str())), |t| {
        t.log(&format!("priority {} → {}", t.priority().as_str(), want.as_str()));
        t.priority_raw = want.as_str().to_string();
    })
}

/// Everything the pick reads, gathered into one value.
///
/// The same bargain the panel's `Snapshot` makes. The interesting half of
/// `next` is a join — which open task is *not* already in another live agent's
/// hand — and while it read the store and asked herdr inline, checking the
/// answer meant standing up two agents, binding one of them, and asking the
/// other. Three idle agents set going at once and all handed the same task is
/// the failure this filter exists to stop; there was nowhere to write it down.
pub(crate) struct Backlog {
    pub tasks: Vec<Task>,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    /// Every pane herdr knows about — a binding to a pane that no longer
    /// exists is stale rather than a holder.
    pub panes: Vec<herdr::Pane>,
    /// This pane. A `doing` task in the caller's own hand is precisely the
    /// caller's next piece of work, so it is not "taken".
    pub me: Option<String>,
}

impl Backlog {
    pub(crate) fn live(store: &Store) -> Backlog {
        Backlog {
            tasks: store.tasks(),
            bindings: store.bindings(),
            panes: if herdr::available() { herdr::panes().unwrap_or_default() } else { Vec::new() },
            me: crate::herdr::Env::read().pane_id,
        }
    }
}

/// What `next` found, and — when it found nothing — what the backlog was doing
/// instead. "Nothing actionable" on its own reads as an empty backlog, and the
/// commonest reason for landing there is the opposite.
pub(crate) struct Pick {
    pub task: Option<Task>,
    /// Open work another live agent is already holding.
    pub held: usize,
    /// Work at review: finished, and waiting on a person rather than an agent.
    pub at_review: usize,
}

/// Choose the caller's next task.
///
/// `review` is deliberately absent from the candidates, and its absence is the
/// whole point: `Status::rank` puts it ahead of `doing` and `todo`, so it did
/// not merely appear here, it *won* — every agent asking what to do next was
/// handed work it had already finished and given back. `review` is where an
/// agent stops; only a person says `done`, and a person finds that pile through
/// `R` in the panel or `wsp wip`. `blocked` is absent for the neighbouring
/// reason: a decision is owed, and that is not an agent's to make either.
pub(crate) fn pick(b: &Backlog, scope_ids: Option<&[String]>) -> Pick {
    let in_scope = |t: &Task| match scope_ids {
        Some(ids) => t.project.as_ref().map(|p| ids.contains(p)).unwrap_or(false),
        None => true,
    };

    // Nothing another live agent is already holding. `claim` refuses those, so
    // naming one sends the caller into a guaranteed refusal — and three idle
    // agents set going at once would otherwise all be handed the same task and
    // all three bounce.
    let taken = |t: &Task| {
        !crate::cmd_agent::live_holders(&b.bindings, &b.panes, &t.id, b.me.as_deref()).is_empty()
    };

    let mut candidates: Vec<Task> = b
        .tasks
        .iter()
        .filter(|t| matches!(t.status(), Status::Doing | Status::Todo))
        .filter(|t| in_scope(t))
        .cloned()
        .collect();
    let held = candidates.iter().filter(|t| taken(t)).count();
    candidates.retain(|t| !taken(t));

    candidates.sort_by(|a, b| {
        a.status()
            .rank()
            .cmp(&b.status().rank())
            .then(a.priority().rank().cmp(&b.priority().rank()))
            .then(a.id.cmp(&b.id))
    });

    Pick {
        task: candidates.into_iter().next(),
        held,
        at_review: b
            .tasks
            .iter()
            .filter(|t| t.status() == Status::Review)
            .filter(|t| in_scope(t))
            .count(),
    }
}

/// Why there was nothing, in the words a person reads. Empty when the backlog
/// really is empty, which is the one case that needs no explaining.
pub(crate) fn nothing_because(p: &Pick) -> String {
    let mut why: Vec<String> = Vec::new();
    if p.at_review > 0 {
        why.push(format!("{} at review, waiting on a person", p.at_review));
    }
    if p.held > 0 {
        why.push(format!("{} held by other agents", p.held));
    }
    why.join(" · ")
}

pub fn next(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let scope = match current_project(store, args, &index) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let scope_ids: Option<Vec<String>> = scope.as_ref().map(|s| index.subtree(s));

    let b = Backlog::live(store);
    let found = pick(&b, scope_ids.as_deref());

    let Some(t) = found.task.as_ref() else {
        // Asked, and there was nothing — which is a state worth wearing rather
        // than a moment. This pane stays like this until somebody hands it
        // something, so of the two labels `say_looking` writes, this is the one
        // that is still on screen when a person comes looking.
        crate::cmd_agent::say_looking(store, &b.panes, scope.as_deref(), false);

        if args.json() {
            println!("null");
        } else {
            let why = nothing_because(&found);
            match why.is_empty() {
                true => println!("nothing actionable"),
                false => println!("nothing actionable — {why}"),
            }
        }
        return 0;
    };

    // Named, but not yet taken up: the claim is the agent's next move and the
    // reading of the task comes in between. The claim renames over this.
    crate::cmd_agent::say_looking(store, &b.panes, scope.as_deref(), true);

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
    let t = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
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
    /// Which sections this kind offers an editor — [`crate::model::PROSE`] for a
    /// task, [`crate::model::PROJECT_PROSE`] for a project. Carried on the item
    /// rather than looked up from `what`, so the one function that writes prose
    /// never has to know which kinds exist.
    pub sections: &'static [&'static str],
}

pub fn edit(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp edit <id> [--overview | --details | --decisions | --raw]");
        return 2;
    };
    let t = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    edit_prose(
        store,
        args,
        Prose {
            what: "task",
            id: t.id.clone(),
            body: t.body.clone(),
            path: store.task_path(&t.id),
            sections: &crate::model::PROSE,
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

pub(crate) fn read_source(src: &str) -> std::io::Result<String> {
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

    // A flag this command does not know is a typo, and the cost of guessing is
    // the prose that was already there: an unrecognised `--<section>` used to
    // read as "no section given", take the combined-buffer path, and — because
    // the payload carried no headings — land whole on `Overview`. Refusing is
    // the only answer that cannot lose anything.
    const KNOWN: [&str; 5] = ["raw", "from", "json", "no-commit", "help"];
    let unknown: Vec<&str> = args
        .flag_names()
        .into_iter()
        .filter(|f| {
            !KNOWN.contains(f) && !item.sections.iter().any(|s| s.to_lowercase() == *f)
        })
        .collect();
    if let Some(f) = unknown.first() {
        eprintln!(
            "wsp: unknown flag `--{f}` — sections are {}",
            item.sections
                .iter()
                .map(|s| format!("--{}", s.to_lowercase()))
                .collect::<Vec<_>>()
                .join(", ")
        );
        return 2;
    }

    // Two sections at once has no meaning: each names a different buffer, and
    // silently preferring the first would write one and drop the other.
    let named: Vec<&str> = item.sections
        .iter()
        .copied()
        .filter(|s| args.has(&s.to_lowercase()))
        .collect();
    if named.len() > 1 {
        eprintln!(
            "wsp: {} name different sections — edit one at a time",
            named.iter().map(|s| format!("--{}", s.to_lowercase())).collect::<Vec<_>>().join(" and ")
        );
        return 2;
    }
    let one = named.first().copied();

    let before = match one {
        Some(sec) => crate::model::section_of(&item.body, sec).unwrap_or_default(),
        None => {
            let mut b = String::new();
            for sec in item.sections {
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
    // A `## ` heading in a *section's* payload is not a heading in that
    // section — the next read takes it for a sibling section, and the rewrite
    // after that adds a second copy of it. `set_section_in` demotes it either
    // way; doing it here as well is what lets the comparison below see the
    // second write of the same brief as the no-op it is, and what lets the
    // writer be told rather than left to find out from the file.
    //
    // The combined buffer is exempt: there `## ` is the section separator, and
    // demoting it would merge every section into the first.
    let demoted = match one {
        Some(_) => crate::model::headings(&after),
        None => Vec::new(),
    };
    let after = match one {
        Some(_) => crate::model::demote_headings(&after),
        None => after,
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

    let decisions_before = crate::model::section_of(&body, "Decisions").unwrap_or_default();

    match one {
        Some(sec) => crate::model::set_section_in(&mut body, sec, &after),
        None => {
            // Every editable section, not the two that happened to exist when
            // this was written. A `## Decisions` block typed into the combined
            // buffer used to be read by nobody and written back by nothing —
            // the save reported success and the text was simply gone.
            //
            // Presence, not content, decides what gets written. A heading still
            // on screen with nothing under it is someone clearing that section;
            // a heading the buffer never carried is a section this edit was not
            // about, and writing it back empty would delete prose nobody
            // touched. Only `--from`/stdin can produce the second case, and it
            // is the one an agent hits.
            let loose = crate::model::section_of(&after, "").unwrap_or_default();
            let present: Vec<&str> = item.sections
                .iter()
                .copied()
                .filter(|s| crate::model::has_section(&after, s))
                .collect();
            if present.is_empty() {
                // Every heading was deleted and something was typed anyway.
                // Keeping it under Overview is a guess; discarding it is not a
                // guess, it is losing the only thing the user actually wrote.
                crate::model::set_section_in(&mut body, "Overview", after.trim());
            } else {
                for name in item.sections.iter().copied() {
                    let mut text = crate::model::section_of(&after, name).unwrap_or_default();
                    // Prose above the first heading belongs to Overview — that
                    // is where someone who deleted the heading and kept typing
                    // meant it to go, and it is the first section either way.
                    if name == "Overview" && !loose.trim().is_empty() {
                        text = if text.trim().is_empty() {
                            loose.trim().to_string()
                        } else {
                            format!("{}\n\n{}", loose.trim(), text)
                        };
                    } else if !present.contains(&name) {
                        continue;
                    }
                    crate::model::set_section_in(&mut body, name, &text);
                }
            }
        }
    }

    // `wsp decide` is how a decision is written and there is no `wsp undecide`,
    // so an editor that can reach `## Decisions` needs an answer to the rule it
    // appears to break. The rule is that a decision cannot be *quietly*
    // rewritten — not that the text is immutable, which no file on disk ever
    // is. A typo is worth fixing; a change nobody can see afterwards is not.
    // So the edit is allowed and it leaves a mark: the log says the section was
    // edited by hand, which is exactly what the record was protecting.
    if crate::model::section_of(&body, "Decisions").unwrap_or_default() != decisions_before {
        crate::model::append_dated(&mut body, "Log", "decisions edited by hand");
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
        println!(
            "{}",
            json!({ "id": item.id, "edited": one.unwrap_or("body"), "demoted": demoted })
        );
    } else {
        println!("edited {}", item.id);
        if !demoted.is_empty() {
            eprintln!(
                "wsp: {} `##` heading{} demoted to `###` — the payload is one section's text: {}",
                demoted.len(),
                if demoted.len() == 1 { "" } else { "s" },
                demoted.join(", ")
            );
        }
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
        // `--raw` hands the file straight to an editor, so nothing in the
        // store wrote it and nothing else would name it in the commit.
        store.wrote(path);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    fn task_with(store: &Store, id: &str, level: &str) -> Task {
        let mut t = Task::new("Retune the early reflections", id);
        t.project = Some("verb".into());
        t.priority_raw = level.into();
        store.save_task(&t).unwrap();
        t
    }

    /// A scope nobody typed has to be able to say what it hid.
    ///
    /// `find` defaults to the project you are standing in, which is right —
    /// hits from `vst` while you are working in `robustness` are noise. But the
    /// person searching did not choose that scope and cannot see it, so "no"
    /// on its own reads as "that task does not exist" when it means "not here".
    /// A finding aid that can send you away empty from a store which holds the
    /// answer is the dead end this whole task was filed about.
    #[test]
    fn a_search_that_found_nothing_here_says_where_it_is() {
        let store = scratch("find-nothing");
        for id in ["wsp", "verb"] {
            store.save_project(&crate::model::Project::new(id)).unwrap();
        }
        let mut t = Task::new("Retune the early reflections", "verb-001");
        t.project = Some("verb".into());
        t.status_raw = "done".into();
        t.body = "## Overview\nThe reverb tail is right and the first 40 ms is not.\n".into();
        store.save_task(&t).unwrap();

        let index = Index::new(store.projects());
        let args = Args::synth("find", &["reverb"], &[]);

        let said = nothing(&store, &index, &args, Some("wsp"), "reverb");
        assert!(said.contains("1 elsewhere"), "{said}");
        assert!(said.contains("--all"), "it has to say which key widens it: {said}");

        // In scope but finished. `--all` is the same answer, so it is the same
        // sentence — what changes is which of the two defaults was in the way.
        let said = nothing(&store, &index, &args, Some("verb"), "reverb");
        assert!(said.contains("nothing open"), "{said}");
        assert!(said.contains("1 finished") && said.contains("--all"), "{said}");
        // …and it is only a guess worth making with no `-s` or `-t` on the
        // line. Those are filters somebody typed and can see, and `--all` does
        // not take them off, so a sentence about `--all` would not be true.
        let narrowed = Args::synth("find", &["reverb"], &[("status", "doing")]);
        assert!(!nothing(&store, &index, &narrowed, Some("verb"), "reverb").contains("finished"));

        // Gone from the list entirely, which is where a task you half-remember
        // most often is: retired, and still the one you meant.
        store.archive_task(&t).unwrap();
        let said = nothing(&store, &index, &args, None, "reverb");
        assert!(said.contains("1 in the archive"), "{said}");
    }

    /// The floor this verb exists for: a level set at `add` used to be the
    /// level for ever. A phase swap in strata-prototype left `high` on the
    /// phase that had become later, and every agent asking `wsp next` was sent
    /// to the wrong end of the plan, because nothing could move it.
    #[test]
    fn a_priority_can_be_changed_after_the_task_is_made() {
        let store = scratch("prio-set");
        task_with(&store, "t-260815-047", "normal");

        let code = prio(&store, &Args::synth("prio", &["047", "high"], &[]));
        assert_eq!(code, 0);

        let t = store.find_task("047").expect("the task");
        assert_eq!(t.priority(), Priority::High);
        // Written down, because a backlog whose order changes with no record
        // is one nobody can read a week later.
        assert!(t.body.contains("priority normal → high"), "the log should carry it: {}", t.body);
    }

    /// A tag that was asked to go and did not, on a command that exited 0, is
    /// a tag nobody can see the reason for. The parser is what used to drop
    /// the removal; this is the floor under it — the command itself refuses to
    /// report success for a removal it could not make.
    #[test]
    fn a_removal_that_removed_nothing_does_not_exit_zero() {
        let store = scratch("tag-absent");
        let mut t = task_with(&store, "verb-012", "normal");
        t.tags = vec!["rust".into(), "tmp".into()];
        store.save_task(&t).unwrap();

        // The tag is there: removal is a removal, and quiet.
        assert_eq!(tag(&store, &Args::synth("tag", &["012", "-tmp"], &[])), 0);
        assert_eq!(store.find_task("012").expect("the task").tags, vec!["rust".to_string()]);

        // It is not there any more, and now the same line is a lie if it says
        // nothing. The additions beside it still stand — this is a report, not
        // a rollback.
        assert_eq!(tag(&store, &Args::synth("tag", &["012", "+dsp", "-tmp"], &[])), 1);
        let t = store.find_task("012").expect("the task");
        assert!(t.tags.iter().any(|x| x == "dsp"), "the add in the same line should hold: {:?}", t.tags);
    }

    /// Setting the level it already has is not a change. `mutate` cannot tell
    /// the difference, and the panel's one key cycles — so this is reachable
    /// by a fumbled keystroke, and it would spend a log line, an event and a
    /// commit on recording that nothing happened.
    #[test]
    fn setting_the_level_it_already_has_writes_nothing() {
        let store = scratch("prio-same");
        task_with(&store, "t-260815-047", "high");

        assert_eq!(prio(&store, &Args::synth("prio", &["047", "high"], &[])), 0);
        let t = store.find_task("047").expect("the task");
        assert_eq!(t.priority(), Priority::High);
        assert!(!t.body.contains("priority"), "nothing should have been logged: {}", t.body);
    }

    /// A level it does not know is refused rather than rounded to `normal`:
    /// `wsp prio 047 urgent` silently demoting the task it was meant to raise
    /// is the worst answer available.
    #[test]
    fn an_unknown_level_is_refused_and_changes_nothing() {
        let store = scratch("prio-bad");
        task_with(&store, "t-260815-047", "high");

        assert_eq!(prio(&store, &Args::synth("prio", &["047", "urgent"], &[])), 2);
        assert_eq!(prio(&store, &Args::synth("prio", &["047"], &[])), 2);
        assert_eq!(store.find_task("047").expect("the task").priority(), Priority::High);
    }

    /// The move that makes a sub-tree, which until now only `add --parent`
    /// could: work decomposes after it is written down at least as often as
    /// before, and the piece that turns out to belong inside another one is
    /// already a task with a body and a log.
    #[test]
    fn a_task_becomes_a_sub_task_of_another() {
        let store = scratch("mv-parent");
        task_with(&store, "t-260815-047", "normal");
        task_with(&store, "t-260815-048", "normal");

        let code = mv(&store, &Args::synth("mv", &["048"], &[("parent", "047")]));
        assert_eq!(code, 0);

        let t = store.find_task("048").expect("the task");
        assert_eq!(t.parent.as_deref(), Some("t-260815-047"));
        assert!(t.body.contains("moved under t-260815-047"), "the log should carry it: {}", t.body);
    }

    /// `--parent none` on work that was never a sub-task moved it and detached
    /// nothing, and the receipt has to say the true one of the two.
    ///
    /// It is not a corner: the panel sends `--parent none` on *every* move to a
    /// project, so that landing on a project row means the top level of it —
    /// which makes a task that had no parent the commonest thing this is asked
    /// about, and "detached" the wrong word for nearly all of them.
    #[test]
    fn detaching_a_task_with_no_parent_says_it_moved() {
        let store = scratch("mv-detach");
        task_with(&store, "t-260815-047", "normal");
        task_with(&store, "t-260815-048", "normal");

        assert_eq!(mv(&store, &Args::synth("mv", &["048"], &[("parent", "none")])), 0);
        let t = store.find_task("048").expect("the task");
        assert!(t.parent.is_none());
        assert!(t.body.contains("moved to verb"), "the log should carry it: {}", t.body);
        assert!(!t.body.contains("detached"), "nothing was detached: {}", t.body);

        // …and when there *is* one, the same flag is a detachment and says so.
        assert_eq!(mv(&store, &Args::synth("mv", &["048"], &[("parent", "047")])), 0);
        assert_eq!(mv(&store, &Args::synth("mv", &["048"], &[("parent", "none")])), 0);
        let t = store.find_task("048").expect("the task");
        assert!(t.parent.is_none());
        assert!(t.body.contains("detached to the top level of verb"), "{}", t.body);
    }

    // ---- what `next` picks, offline ---------------------------------------

    fn open_task(id: &str, title: &str, status: &str, prio: &str) -> Task {
        let mut t = Task::new(title, id);
        t.project = Some("wsp".into());
        t.status_raw = status.to_string();
        t.priority_raw = prio.to_string();
        t
    }

    fn live_pane(id: &str) -> herdr::Pane {
        herdr::Pane { pane_id: id.to_string(), agent: "claude".into(), ..Default::default() }
    }

    /// Four open tasks, one of them in another agent's hand.
    fn backlog() -> Backlog {
        let mut bindings = std::collections::BTreeMap::new();
        bindings.insert("w2:p1".to_string(), json!({ "task_id": "t-002" }));
        Backlog {
            tasks: vec![
                open_task("t-001", "a todo, high", "todo", "high"),
                open_task("t-002", "held by somebody else", "doing", "high"),
                open_task("t-003", "a todo, normal", "todo", "normal"),
                open_task("t-004", "finished, waiting on a person", "review", "high"),
                open_task("t-005", "waiting on a decision", "blocked", "high"),
                open_task("t-006", "not yet, deliberately", "parked", "high"),
            ],
            bindings,
            panes: vec![live_pane("w1:p1"), live_pane("w2:p1")],
            me: Some("w1:p1".into()),
        }
    }

    /// `next` is the question an agent with nothing to do asks, and parked
    /// work is the one answer that can never be right: somebody has already
    /// decided the moment is wrong, and an agent is not who un-decides that.
    /// High priority in the fixture on purpose — priority orders the
    /// candidates and does not choose them, so a `high` parked task is the
    /// shape that would win if the status were not filtering first.
    #[test]
    fn next_never_hands_out_work_somebody_has_parked() {
        let mut alone = backlog();
        alone.tasks.retain(|t| matches!(t.status_raw.as_str(), "parked"));
        let p = pick(&alone, None);
        assert!(p.task.is_none(), "parked work is not a candidate, whatever its priority");
        // And it is not one of the excuses either: `nothing actionable` names
        // what it stepped over, and a parked task was stepped over by nobody.
        assert_eq!(nothing_because(&p), "");
    }

    /// The failure this filter exists to stop: three idle agents set going at
    /// once, all handed the same task, all three bounced off `claim`. Writable
    /// now because the pick takes its inputs in — before this it needed two
    /// live agents with a binding between them.
    #[test]
    fn next_does_not_name_work_another_live_agent_is_holding() {
        let p = pick(&backlog(), None);
        assert_eq!(p.task.map(|t| t.id), Some("t-001".to_string()));
        assert_eq!(p.held, 1, "and it says how many it stepped over");

        // The holder's own pane asking gets it back: a `doing` task in the
        // caller's hand is precisely the caller's next piece of work.
        let mut mine = backlog();
        mine.me = Some("w2:p1".into());
        let p = pick(&mine, None);
        assert_eq!(p.task.map(|t| t.id), Some("t-002".to_string()), "doing outranks todo, and it is mine");
        assert_eq!(p.held, 0);
    }

    /// A binding to a pane herdr no longer reports is stale, not a holder —
    /// which is the state a re-claim exists to clear. A pane that is a person
    /// at a terminal is not a holder either; they can be asked to move.
    #[test]
    fn a_dead_pane_is_not_holding_anything() {
        let mut gone = backlog();
        gone.panes.retain(|p| p.pane_id != "w2:p1");
        let p = pick(&gone, None);
        assert_eq!(p.task.map(|t| t.id), Some("t-002".to_string()), "the binding outlived the pane");
        assert_eq!(p.held, 0);

        let mut shell = backlog();
        shell.panes[1].agent = String::new();
        assert_eq!(pick(&shell, None).held, 0, "a shell is a person, who can be asked");
    }

    /// `review` outranks `doing` and `todo`, so leaving it in the candidates
    /// did not merely offer it — it won, every time. Every agent asking what to
    /// do next was handed work it had already finished and given back.
    #[test]
    fn work_already_finished_is_never_the_next_thing_to_do() {
        let mut only_review = backlog();
        only_review.tasks.retain(|t| matches!(t.status_raw.as_str(), "review" | "blocked"));
        let p = pick(&only_review, None);
        assert!(p.task.is_none(), "nothing an agent can pick up");
        assert_eq!(p.at_review, 1);

        // And "nothing actionable" alone reads as an empty backlog, which is
        // the opposite of what this is.
        assert_eq!(nothing_because(&p), "1 at review, waiting on a person");
        let empty = pick(&Backlog { tasks: Vec::new(), ..backlog() }, None);
        assert_eq!(nothing_because(&empty), "", "an empty backlog needs no explaining");
    }

    /// Both reasons at once, in the order a person wants them: what is waiting
    /// on you first, what is waiting on somebody else after.
    #[test]
    fn nothing_actionable_says_which_kind_of_nothing() {
        let mut b = backlog();
        b.tasks.retain(|t| t.id != "t-001" && t.id != "t-003");
        let p = pick(&b, None);
        assert!(p.task.is_none());
        assert_eq!(nothing_because(&p), "1 at review, waiting on a person · 1 held by other agents");
    }

    /// Scope filters the candidates and the count of what is waiting with them:
    /// an agent told to work in one project is not told about another's queue.
    #[test]
    fn a_scoped_next_counts_only_its_own_backlog() {
        let mut b = backlog();
        b.tasks.push({
            let mut t = open_task("t-006", "somebody else's project", "todo", "high");
            t.project = Some("strata".into());
            t
        });
        let ids = ["strata".to_string()];
        let p = pick(&b, Some(&ids));
        assert_eq!(p.task.map(|t| t.id), Some("t-006".to_string()));
        assert_eq!(p.at_review, 0, "wsp's review pile is not strata's business");
        assert_eq!(p.held, 0);
    }

    fn parse(line: &[&str]) -> Args {
        Args::parse(line.iter().map(|s| (*s).to_string()).collect())
    }

    fn scratch_file(name: &str, text: &str) -> String {
        let p = std::env::temp_dir().join(format!("wsp-{name}-{}.md", std::process::id()));
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().to_string()
    }

    fn log_of(store: &Store, id: &str) -> String {
        store.find_task(id).expect("the task").section("Log").unwrap_or_default()
    }

    /// The whole defect, from the line that caused it.
    ///
    /// `wsp note <id> --from -` is what an agent writes after learning
    /// `wsp edit <id> --overview -`, and it used to record a log entry whose
    /// entire body was `-`. Asserted on the recogniser rather than through
    /// `note`, because the failing spellings include stdin and a test that
    /// reads stdin either blocks or reads the harness's.
    #[test]
    fn the_payload_that_ate_a_review_note_is_read_as_a_stream() {
        let one = |s: &str| vec![s.to_string()];
        for shape in ["-", "--from"] {
            assert_eq!(payload_source(&one(shape)), Some("-".into()), "`{shape}` names stdin");
        }
        // `--from` reaches `note` as a positional — it stops parsing flags
        // after its subject — so both halves arrive as payload.
        let a = parse(&["note", "047", "--from", "-"]);
        assert!(!a.has("from"), "the parser must leave the payload alone");
        assert_eq!(payload_source(&a.rest[1..]), Some("-".into()));
        assert_eq!(payload_source(&a.rest[..]), None, "the id is not a source");

        for from in [vec!["--from".to_string(), "notes.md".to_string()], one("--from=notes.md")] {
            assert_eq!(payload_source(&from), Some("notes.md".into()));
        }
    }

    /// …and the other half: prose that begins like a source is prose.
    ///
    /// Notes in this store are mostly *about* the CLI, so they begin with a
    /// flag about as often as not. A payload counts as a source only when it is
    /// entirely one.
    #[test]
    fn a_note_that_only_begins_like_a_source_is_still_recorded_whole() {
        let store = scratch("note-prose");
        task_with(&store, "t-260815-047", "normal");

        let said = "--from - is the shape that ate a review note";
        assert_eq!(note(&store, &parse(&["note", "047", said])), 0);
        assert!(log_of(&store, "047").contains(said), "{}", log_of(&store, "047"));
    }

    /// A note read from a stream is the note, not the dash — and it is one
    /// entry, because every reader of `## Log` reads it a line at a time.
    #[test]
    fn a_note_read_from_a_stream_lands_whole_and_on_one_line() {
        let store = scratch("note-stream");
        task_with(&store, "t-260815-047", "normal");

        let path = scratch_file("survey", "Eighteen tests against the live store.\n\nThree renamed this machine's w1.\n");
        assert_eq!(note(&store, &parse(&["note", "047", "--from", &path])), 0);

        let log = log_of(&store, "047");
        assert_eq!(log.lines().filter(|l| !l.trim().is_empty()).count(), 1, "one note, one entry: {log}");
        assert!(log.contains("Eighteen tests") && log.contains("renamed this machine's w1"), "{log}");
        assert!(!log.trim_end().ends_with(" -"), "the dash is what this task is about: {log}");
    }

    /// Nothing to record is refused, out loud.
    ///
    /// This is the condition that turns the silent loss into an error: an empty
    /// stream is exactly what a bare `-` used to mean, and a log entry saying
    /// nothing is indistinguishable from the note that went missing.
    #[test]
    fn a_note_from_an_empty_stream_is_refused_rather_than_recorded() {
        let store = scratch("note-empty");
        task_with(&store, "t-260815-047", "normal");

        let path = scratch_file("empty", "\n  \n");
        assert_eq!(note(&store, &parse(&["note", "047", "--from", &path])), 2);
        assert!(log_of(&store, "047").trim().is_empty(), "nothing may be recorded");

        // A source that is not there is a mistake worth reporting, not an
        // empty note either.
        assert_eq!(note(&store, &parse(&["note", "047", "--from", "/nowhere/at/all.md"])), 1);
        assert!(log_of(&store, "047").trim().is_empty());
    }

    /// `decide` takes prose the same way, for the same reason: an agent that
    /// has learned `-` on one of them will type it on the other.
    #[test]
    fn a_decision_can_be_read_from_a_stream_too() {
        let store = scratch("decide-stream");
        task_with(&store, "t-260815-047", "normal");

        let path = scratch_file("decision", "Store UTC, render local.\nThe instant is the record.\n");
        assert_eq!(decide(&store, &parse(&["decide", "047", "--from", &path])), 0);

        let t = store.find_task("047").expect("the task");
        let d = t.section("Decisions").unwrap_or_default();
        assert_eq!(d.lines().filter(|l| !l.trim().is_empty()).count(), 1, "{d}");
        assert!(d.contains("Store UTC, render local. The instant is the record."), "{d}");
    }

    /// The link is written into the record, not worked out by a renderer — so
    /// it has to survive being read back out of the file, and a reference to
    /// something that is not there has to be refused at the point somebody can
    /// still fix it. `supersedes d9` in the file would mark nothing, look
    /// exactly like a decision that supersedes nothing, and be found by
    /// whoever wondered why the old rule was still printed.
    #[test]
    fn a_decision_names_the_one_it_replaces_and_a_reference_to_nothing_is_refused() {
        let store = scratch("decide-supersedes");
        task_with(&store, "t-260815-047", "normal");

        assert_eq!(decide(&store, &parse(&["decide", "047", "worktrees are not for now"])), 0);
        // Before it exists, and in the shape a typo takes.
        assert_eq!(
            decide(&store, &parse(&["decide", "047", "…", "--supersedes", "d9"])),
            1,
            "a reference to nothing is not written"
        );
        assert_eq!(
            decide(&store, &parse(&["decide", "047", "…", "--supersedes", "later"])),
            2,
            "and neither is one that is not an id"
        );
        assert_eq!(
            decide(&store, &parse(&["decide", "047", "one tree per task", "--supersedes", "1"])),
            0,
            "`1` and `d1` are the same reference"
        );

        let t = store.find_task("047").expect("the task");
        let all = crate::model::decisions_of(&t.body);
        assert_eq!(all.len(), 2, "the refusals wrote nothing: {}", t.body);
        assert_eq!(all[1].supersedes, vec!["d1"]);
        assert_eq!(
            crate::model::supersessions(&all)[0],
            Some("d2".to_string()),
            "and the older entry can be marked from what is in the file"
        );
    }
}
