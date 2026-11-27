//! Task commands.

use serde_json::{json, Value};

use crate::claims::{claim_line, worked_line};
use crate::cycle::Seats;
use crate::message::{self, Ask, Message, Shape};
use crate::model::{Priority, Status, Task};
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, fold, read_source, Paint};
use crate::base::args::Args;

pub fn add(store: &Store, args: &Args) -> i32 {
    // Asked only of a sub-task: the seat matters to nothing else `add` does.
    // And of a row that asks for the whole suite (`wsp-538`), which only a seat is refused.
    let asks = || std::iter::once(args.text(0)).chain(args.get("caption")).any(|t| crate::throttle::asks_full_suite(&t).is_some());
    let pane = (args.has("parent") || asks()).then(crate::place::my_pane).flatten();
    add_from(store, args, pane.as_deref())
}

/// [`add`] from the seat `pane`, so a test says where it stands rather than
/// reading this process's environment.
pub fn add_from(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let title = args.text(0);
    if title.trim().is_empty() {
        eprintln!("usage: wsp add \"title\" [-p project] [-t tag]… [--prio high] [--parent id | --from id] [--after id,…]");
        // A title that begins with a dash is read as a flag, and then there is
        // no title — the usage line alone says nothing about why.
        if !args.flag_names().is_empty() {
            eprintln!("       a title that starts with a dash goes after `--`: wsp add -p proj -- \"-title\"");
        }
        return 2;
    }
    // A title has no `-` form and does not need one — one line, and single
    // quotes stop a shell dead. What it does need is the same refusal every
    // other intake makes, because a title is the most-quoted string in the
    // store: it is in `ls`, in the brief, in the pane's own name, and in every
    // spawn on the task afterwards.
    if let Some(why) = util::terminal_output(&title) {
        eprintln!("wsp: {why}");
        return 2;
    }
    if let Some(tail) = (!args.escaped).then(|| flag_in_title(&title)).flatten() {
        eprintln!("wsp: the title ends in `{tail}`, which is one of add's own flags — inside the quotes it would be filed as words of the title");
        eprintln!("     put it after the closing quote; a title that means those words goes after `--`");
        return 2;
    }
    let caption = match crate::caption::flag(args, true) {
        Ok(c) => c.filter(|c| !c.is_empty()),
        Err(code) => return code,
    };

    // From a seat, a row does not require the whole suite of its member
    // (`wsp-538`): the barrier owns that run. A person at a shell may write one.
    if pane.is_some() {
        let asks = std::iter::once(title.as_str()).chain(args.get("caption").iter().map(String::as_str)).find_map(crate::throttle::asks_full_suite);
        if let Some(found) = asks {
            eprintln!("{}", crate::throttle::full_suite_ask_refusal(&found));
            return 1;
        }
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
    // A follow-up (`wsp-345`): the row this was found while doing. Resolved
    // before an id exists for the reason the parent is, and never made a
    // parent: a follow-up records where it came from and holds nothing open,
    // so `wsp done` on its origin is not refused for it. The archive is asked
    // too, since work is found on a row after it closed as often as before.
    let found_by = match args.get("from") {
        Some(needle) => match store.find_task(&needle).or_else(|| store.archived_task(&needle).map(|(t, _)| t)) {
            Some(o) => Some(o),
            None => {
                eprintln!("wsp: no such task `{needle}` to be a follow-up from");
                return 1;
            }
        },
        None => None,
    };
    if let (Some(p), Some(o)) = (&parent, &found_by) {
        eprintln!(
            "wsp: --parent {} and --from {} are two different things: a sub-task is part of its parent's done-when, \
             a follow-up is work found while doing a row and holds nothing open — give one",
            p.id, o.id
        );
        return 2;
    }
    // A member's own seat splitting its row into sub-tasks no run will start
    // (`wsp-311`, compound-758). Read whether or not it applies, so the flag is
    // never refused as unknown on an ordinary add.
    let anyway = args.has("anyway");
    let unrun = match parent.as_ref().and_then(|p| {
        crate::split::refusal(p, &store.bindings(), &crate::worklist::Running::read(store), pane)
    }) {
        Some(why) if !anyway => {
            eprintln!("wsp: {why}");
            eprintln!("     `--anyway` files it regardless, as a row nothing runs");
            return 1;
        }
        Some(_) => parent.as_ref().map(|p| p.id.clone()),
        None => None,
    };
    // And what it rests on, for the same reason: resolved while there is no id
    // for a typo to match. A task nobody has filed yet is on no list and
    // nothing rests on it, so of `wsp after`'s refusals only "no such task"
    // can apply here.
    let after = match crate::after::resolve(store, "the new task", &args.all("after")) {
        Ok(on) => on,
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    };

    // A sub-task belongs where its parent belongs. Filing it anywhere else
    // splits a piece of work across two places in the tree, and the tree is
    // the only thing that shows the two are related.
    let project = if args.has("inbox") {
        None
    } else if let (Some(p), false) = (&parent, args.has("project")) {
        p.project.clone()
    } else if let (Some(o), false) = (&found_by, args.has("project")) {
        // Where it was found, unless said otherwise: `-p` may differ, since a
        // follow-up is not part of its origin and may belong elsewhere.
        o.project.clone()
    } else {
        match Seats::current_project(crate::cycle::seats(), store, args, &index) {
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

    // Private unless said otherwise; a sub-task is where its parent is (wsp-172).
    // Before an id exists, so a refusal leaves nothing reserved.
    let beside = parent.as_ref().or(found_by.as_ref()).map(|p| &p.workspace);
    let workspace = match crate::record_workspace::of_new(store, args.get("workspace").as_deref(), beside) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    };

    // A shared row's id is the shared side's to give (its counter is one for
    // everybody filing into the project); a private row's is the local
    // counter's. Before anything is written, so a refusal leaves nothing.
    let id = match new_row_id(store, project.as_deref(), &workspace, parent.as_ref().map(|p| p.id.as_str())) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("wsp: cannot allocate id: {e}");
            return 1;
        }
    };

    let mut t = Task::new(title.trim(), &id);
    t.caption = caption;
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
    t.found_by = found_by.as_ref().map(|o| o.id.clone());
    t.after = after;
    t.workspace = workspace;

    if let Err(e) = file_new(store, &mut t, &crate::cmd_message::Caller::here(store)) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }

    if args.json() {
        // In full: `json` carries what the index does not.
        let full = store.task(&t.id).unwrap_or_else(|| t.clone());
        println!("{}", full.json());
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
        if let Some(parent) = &unrun {
            println!("  {}", p.yellow(&crate::split::filed_anyway(parent, &t.id)));
        }
        if let Some(o) = &found_by {
            println!("  {}", p.dim(&format!("a follow-up from {}: it does not hold {} open", o.id, o.id)));
        }
    }
    0
}

/// The write `add` ends in, for a task already made: its file, the event and
/// the commit, for `caller`. A caller from the hub (wsp-175) also has the row's
/// log say so, `added by <user> via the hub`; one here writes what it always has.
/// The id a new row in `workspace` takes: the shared-work add-on's when it
/// gives one (a shared row's comes from the hub's counter), else the local
/// counter's. Reserved either way; `Err` is why not, with nothing written.
pub fn new_row_id(store: &Store, project: Option<&str>, workspace: &crate::model::WorkspaceId, parent: Option<&str>) -> Result<String, String> {
    match crate::addon::get().new_row_id(store, project, workspace, parent)? {
        Some(id) => Ok(id),
        None => store.alloc_task_id(project).map_err(|e| e.to_string()),
    }
}

pub fn file_new(store: &Store, t: &mut Task, caller: &crate::cmd_message::Caller) -> std::io::Result<()> {
    if caller.via_hub {
        t.log(&format!("added {}", caller.byline()));
    }
    store.save_task(t)?;
    store.log_event("task-added", json!({ "id": t.id, "project": t.project, "title": t.title, "by": caller.by }));
    store.git_commit(&format!("wsp: add {} — {}", t.id, t.title));
    Ok(())
}

/// One of `add`'s own flags, with its value, on the end of a title.
///
/// Found while measuring for `wsp-240`: compound-404 to -407 were filed with
/// titles ending in a literal ` --parent compound-403`, so the flag went into
/// the title and the parent was never set — four rows at the top level that
/// meant to be children, and four titles carrying the bookkeeping. Only the
/// tail, and only `add`'s own names: a title that *mentions* a flag mid-way is
/// about the CLI, which titles here usually are.
fn flag_in_title(title: &str) -> Option<String> {
    const VALUED: [&str; 12] =
        ["parent", "from", "project", "p", "tag", "t", "ref", "prio", "priority", "status", "s", "caption"];
    let name = |tok: &str| tok.strip_prefix("--").or_else(|| tok.strip_prefix('-')).map(str::to_string);
    let toks: Vec<&str> = title.split_whitespace().collect();
    let last = *toks.last()?;
    if let Some((n, v)) = last.strip_prefix("--").and_then(|t| t.split_once('=')) {
        return (VALUED.contains(&n) && !v.is_empty()).then(|| last.to_string());
    }
    if last == "--inbox" {
        return Some(last.to_string());
    }
    let [.., flag, value] = toks.as_slice() else { return None };
    let n = name(flag)?;
    (VALUED.contains(&n.as_str()) && !value.starts_with('-')).then(|| format!("{flag} {value}"))
}

fn filtered(store: &Store, args: &Args, index: &Index, scope: Option<String>) -> Vec<Task> {
    keep(store.tasks(), args, index, scope)
}

/// The same filters over a set of tasks somebody else chose.
///
/// Split out of [`filtered`] because `find --all` reads the archive as well as
/// the live store and the two halves have to be filtered by one rule: a hit
/// that `-p` or `-t` would hide is hidden wherever the file sits.
fn keep(tasks: Vec<Task>, args: &Args, index: &Index, scope: Option<String>) -> Vec<Task> {
    let want_status = args.get("status").and_then(|s| Status::parse(&s));
    let want_tag = args.get("tag");
    let show_all = args.has("all");

    let scope_ids: Option<Vec<String>> = scope.map(|s| index.subtree(&s));

    let mut out: Vec<Task> = tasks
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
        match Seats::current_project(crate::cycle::seats(), store, args, &index) {
            Ok(p) => p,
            Err(code) => return code,
        }
    };

    if args.json() {
        if let Some(out) = ls_all_api(store, args) {
            println!("{out}");
            return 0;
        }
        let tasks = filtered(store, args, &index, scope.clone());
        println!("{}", rows_json(store, &tasks));
        return 0;
    }
    // **The rows are front matter, so they are read from the index**
    // (`wsp-320`): status, priority, title, project and parent are all a row
    // draws, and opening all 1800 task files to draw them was most of what a
    // `wsp ls` cost. `--json` above still reads the tasks, for what it prints.
    let all: Vec<Task> = store.task_heads().iter().map(|h| h.bodiless()).collect();
    let tasks = keep(all.clone(), args, &index, scope.clone());

    let p = Paint::new();
    if tasks.is_empty() {
        match &scope {
            Some(s) => println!("{}", p.dim(&format!("no open tasks in {s} — wsp add \"…\""))),
            None => println!("{}", p.dim("no open tasks — wsp add \"…\"")),
        }
        if let Some(l) = crate::follow_up::line(&crate::follow_up::unheld(store)) {
            println!("{}", p.yellow(&l));
        }
        return 0;
    }

    if let Some(s) = &scope {
        println!("{}", p.dim(&scope_line(s, &tasks)));
    }
    print_tasks(&tasks, &all, &p, scope.is_none(), &crate::addon::get().marks(store));
    // Follow-ups nothing will run (`wsp-345`): found by a closed row, on no
    // list and under no seat. Named under the rows, whatever the scope, since
    // a row nobody runs is lost wherever it is filed.
    if let Some(l) = crate::follow_up::line(&crate::follow_up::unheld(store)) {
        println!();
        println!("{}", p.yellow(&l));
    }
    0
}

/// The dim scope line above a listing.
///
/// The word over the number follows what the filter admitted, not what the
/// verb usually shows. With `--all` the list holds finished work too, and
/// `fork  (27 open)` over twenty-seven done rows is how this line came to tell
/// a governor the fork project still had rows outstanding minutes after Ed had
/// closed the lot — his panel was right and the CLI lied (ui-001). The
/// arithmetic was never wrong; a total was printed as the answer to *how much
/// is left*, the pattern of robustness-079 and wsp-093: a count computed for
/// one question and read as the answer to another.
///
/// So both halves are stated whenever any of it is finished — `0 open · 27
/// done` keeps the half that answers *is anything left* without dropping the
/// half that explains why the row is long. All open, the one word is enough,
/// because then it is the truth.
fn scope_line(scope: &str, shown: &[Task]) -> String {
    let open = shown.iter().filter(|t| t.status().is_open()).count();
    match shown.len() - open {
        0 => format!("{scope}  ({open} open)"),
        done => format!("{scope}  ({open} open · {done} done)"),
    }
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
///
/// `--all` reaches the archive too, and that is the same argument one boundary
/// out. The sentence at the bottom could already see over it — "1 in the
/// archive" — which is honest and was still a dead end, because it withheld
/// the id, and the id is the whole of what a finding aid owes: `wsp show`
/// reads an archived record in full, so the id is a working reference and not
/// a consolation. Widening the verb rather than naming the ids in the sentence
/// is what keeps working at three hits.
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
        match Seats::current_project(crate::cycle::seats(), store, args, &index) {
            Ok(p) => p,
            Err(code) => return code,
        }
    };

    let hits: Vec<Task> = filtered(store, args, &index, scope.clone())
        .into_iter()
        .filter(|t| t.matches(needle))
        .collect();

    // The third thing `--all` widens past. It already takes off the status
    // filter and the project scope; the live set was the one boundary it left
    // in place, and `nothing` below has always been able to see over it — it
    // would say the archive held one and stop, which is a dead end with the
    // id, the one thing you needed, on the far side of it. Naming those ids in
    // the sentence was the cheaper fix and it stops working at three.
    let archived = archived_hits(store, args, &index, scope.clone(), needle);

    if args.json() {
        // On every row, not just the archived ones: a caller that has to know
        // the key is sometimes absent is being asked to guess at the half of
        // the answer that matters most.
        // `wsp-242`: one read of each source the stages stand on, for every
        // row at once, rather than a walk of the store per row.
        let asked: Vec<&Task> = hits.iter().chain(archived.iter()).collect();
        let stages = crate::stage::with_history(store, &asked);
        let claims = store.claims();
        let out: Vec<_> = hits
            .iter()
            .map(|t| (t, false))
            .chain(archived.iter().map(|t| (t, true)))
            .map(|(t, filed)| {
                let stage = stages.get(&t.id).map(|(s, h)| crate::stage::view(s, h));
                wsp_view::task::ls_row(&row_of(t, &claims), filed, stage.as_ref())
            })
            .collect();
        println!("{}", wsp_view::task::ls(&out));
        return 0;
    }

    let p = Paint::new();
    if hits.is_empty() && archived.is_empty() {
        println!("{}", p.dim(&nothing(store, &index, args, scope.as_deref(), needle)));
        return 0;
    }

    let where_ = match &scope {
        Some(s) => format!(" in {s}"),
        None => String::new(),
    };
    // How many of the total are not live, said in the line that is the whole
    // answer whether or not the list below it is cut. Archived rows sort last,
    // so they are the first thing the cap takes; this is what keeps the fact
    // that the archive answered from going with them.
    let filed = match archived.len() {
        0 => String::new(),
        n => format!(" — {n} archived"),
    };
    let total = hits.len() + archived.len();
    println!("{}", p.dim(&format!("{total} matching \"{needle}\"{where_}{filed}")));

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
    let rows: Vec<(Task, bool)> = hits
        .into_iter()
        .map(|t| (t, false))
        .chain(archived.into_iter().map(|t| (t, true)))
        .collect();
    let shown = match args.has("full") {
        true => rows.len(),
        false => rows.len().min(FIND_MAX),
    };
    print_hits(&rows[..shown], needle, &p);
    if shown < rows.len() {
        println!(
            "{}",
            p.dim(&format!("  … {} more · wsp find {} --full", rows.len() - shown, quoted(needle)))
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

/// The archive half of a `find`, which is empty unless `--all` was typed.
///
/// Separate from [`find`] so a test can ask what the archive answered without
/// reading printed output, and so the one condition that gates the walk is in
/// one place.
fn archived_hits(
    store: &Store,
    args: &Args,
    index: &Index,
    scope: Option<String>,
    needle: &str,
) -> Vec<Task> {
    if !args.has("all") {
        return Vec::new();
    }
    let mut out: Vec<Task> = keep(store.archived_tasks(), args, index, scope)
        .into_iter()
        .filter(|t| t.matches(needle))
        .collect();
    // These rows print `archived` where a status would be, so ordering them by
    // a rank they do not show reads as no order at all. Id order groups them
    // by project, which is what the eye is using to read the column anyway.
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// What to say when the search found nothing, which is the answer a scoped
/// search most often owes an explanation for.
///
/// The scope is a default nobody typed, so "no" on its own is a dead end: it
/// reads as "that task does not exist" when what it means is "not here". So
/// this looks once more, without the scope and without the status filter, and
/// says which of the two was in the way — and then, when the store genuinely
/// has nothing, whether the archive does. `--all` widens all three at once, so
/// every half points at the same key.
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
    //
    // The key goes on the end for the same reason it does above: this sentence
    // used to be the end of the road, and `--all` now prints those rows rather
    // than counting them. Withheld under the rule this whole function follows
    // — an explicit `-s` or `-t` is left alone — and withheld again when
    // `--all` is already on the line, since that is how the archive came to be
    // searched and it still answered nothing. What it can see from here are
    // hits some visible filter hid, and naming a key already typed says
    // nothing about which one to take off.
    match store.archived_tasks().iter().filter(|t| t.matches(needle)).count() {
        0 => format!("nothing matching \"{needle}\""),
        n if narrowed || args.has("all") => {
            format!("nothing matching \"{needle}\" — {n} in the archive")
        }
        n => format!("nothing matching \"{needle}\" — {n} in the archive · wsp find {typed} --all"),
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
fn print_hits(hits: &[(Task, bool)], needle: &str, p: &Paint) {
    for l in hit_lines(hits, needle, p) {
        println!("{l}");
    }
}

/// The rows, as text — the same split as [`task_lines`], and here for the same
/// reason: what an archived row says about itself is the behaviour, so a test
/// has to be able to read it rather than infer it from an exit code.
fn hit_lines(hits: &[(Task, bool)], needle: &str, p: &Paint) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let idw = hits.iter().map(|(t, _)| t.id.chars().count()).max().unwrap_or(12);
    for (t, filed) in hits {
        // An archived row says so in the column that says what state a row is
        // in, which is the one thing a reader needs before acting on it. The
        // status underneath is not worth the width: a swept task is `done`, a
        // removed one kept whatever it had on the way out, and neither is a
        // fact about now. `wsp show <id>` reads the record in full and prints
        // both, which is where the row is pointing anyway.
        let st = match filed {
            true => p.dim(&util::pad("archived", 8)),
            false => match t.status() {
                Status::Doing => p.cyan(&util::pad("doing", 8)),
                Status::Blocked => p.red(&util::pad("blocked", 8)),
                Status::Review => p.yellow(&util::pad("review", 8)),
                other => p.dim(&util::pad(other.as_str(), 8)),
            },
        };
        out.push(format!(
            "  {} {} {} {}{}",
            p.dim(&util::pad(&t.id, idw)),
            st,
            paint_prio(p, t.priority()),
            util::truncate(&t.title, 62),
            p.dim(&format!("  [{}]", t.project.clone().unwrap_or_else(|| "inbox".into())))
        ));
        if !t.title.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()) {
            if let Some(line) = t.prose_line(needle, 70) {
                out.push(format!("  {}{}", " ".repeat(idw + 1), p.dim(&line)));
            }
        }
    }
    out
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
    print_tasks(&tasks, &store.tasks(), &p, false, &crate::addon::get().marks(store));
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

/// `shown` is what the filters left; `all` is the store. Two arguments because
/// the count on a parent is not a fact about the list — see
/// [`crate::resolve::open_under_review`], where `wsp ls -s review` printing
/// `(1 open)` over four is the case they were one argument for.
fn print_tasks(shown: &[Task], all: &[Task], p: &Paint, show_project: bool, marks: &std::collections::BTreeMap<String, String>) {
    for l in task_lines(shown, all, p, show_project, marks) {
        println!("{l}");
    }
}

/// The rows, as text. Separate from printing them so the count on a parent is
/// something a test can read — which is what this exists for.
fn task_lines(shown: &[Task], all: &[Task], p: &Paint, show_project: bool, marks: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let idw = shown.iter().map(|t| t.id.chars().count()).max().unwrap_or(12);
    // Children under their parent, indented. The parent carries what is still
    // open beneath it, because a one-line parent that says nothing about its
    // children is a task you would tick off without looking.
    let mut out: Vec<String> = Vec::new();
    for (t, depth) in crate::resolve::nest(shown) {
        let t = &t;
        let indent = "  ".repeat(depth);
        let under = crate::resolve::counts_under(all, &t.id);
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
        let mut project = if show_project {
            p.dim(&format!("  [{}]", t.project.clone().unwrap_or_else(|| "inbox".into())))
        } else {
            String::new()
        };
        // What the shared-work add-on wants seen at a glance (a shared row
        // still on a local id): one dim word after the rest.
        if let Some(m) = marks.get(&t.id) {
            project.push_str(&p.dim(&format!("  ({m})")));
        }
        out.push(format!(
            "  {} {} {} {}{}{}{}",
            p.dim(&util::pad(&t.id, idw)),
            st,
            prio,
            indent,
            util::truncate(&t.title, 62usize.saturating_sub(indent.len())),
            kids,
            project
        ));
    }
    out
}

/// `wsp show <piece-id> --json`'s document.
fn addressed_json(a: &crate::pieces::Addressed) -> Value {
    let mut v = match &a.piece {
        Some(p) => crate::pieces::piece_json(&a.task.id, p),
        None => json!({ "n": a.n, "id": a.id(), "folded": a.folded }),
    };
    v["row"] = json!(a.task.id);
    if let Some(path) = &a.archived {
        v["archived"] = json!(util::contract(path));
    }
    v
}

/// `wsp show <id> --json`'s document: a piece's entry when `needle` names
/// one, else the row it resolves to, live or archived. `Err` is what the
/// verb says on stderr instead. The CLI prints this, and the API's `show`
/// answers it (wsp-307), so the two cannot drift.
pub fn show_json(store: &Store, needle: &str) -> Result<Value, String> {
    match crate::pieces::addressed(store, needle) {
        Ok(Some(a)) => return Ok(addressed_json(&a)),
        Ok(None) => {}
        Err(why) => return Err(format!("wsp: {why}")),
    }
    let (t, archived) = match store.resolve_task(needle) {
        crate::store::Found::One(t) => (*t, None),
        crate::store::Found::Archived(t, path) => (*t, Some(path)),
        other => return Err(other.why(needle).unwrap_or_default()),
    };
    // Each key's rule is `wsp_view::task::show_json`'s: this reads what it
    // prints and hands it over.
    let claims = store.claims();
    let staged = crate::stage::with_history(store, &[&t]);
    let running = crate::worklist::Running::read(store);
    let shown = wsp_view::task::Show {
        body: &t.body,
        archived: archived.as_ref().map(|path| util::contract(path)),
        claim: claims.get(&t.id).cloned(),
        worked: store.worked().get(&t.id).cloned(),
        list: running.of(&t.id).map(|l| wsp_view::task::Running { list: l.list.clone(), group: l.group, of: l.of }),
        raised: raised_about(store, &t.id).iter().map(|m| m.to_json()).collect(),
        verification: crate::verification::view(&t),
        legs: crate::legs::view(&t),
        pieces: crate::pieces::view(&t),
        stage: staged.get(&t.id).map(|(s, h)| crate::stage::view(s, h)),
    };
    let mut v = wsp_view::task::show_json(&row_of(&t, &claims), &shown);
    // Only where allocated (`wsp allocate`), as the file carries it.
    if let Some(u) = &t.for_user {
        v["for"] = Value::String(u.clone());
    }
    // A shared row: its workspace (`shared:<name>`, as `ls --json` and the
    // feed's row name it), and what the shared-work add-on knows of it here
    // (who holds it, whether it is a head only, whether it is mine), as the
    // feed's row has it.
    if !t.workspace.is_private() {
        v[wsp_view::common::key::WORKSPACE] = Value::String(t.workspace.to_string());
        let addon = crate::addon::get();
        let f = addon.row_facts(store).remove(&t.id).unwrap_or_default();
        let mine = crate::addon::mine(&f, t.for_user.as_deref(), addon.user().as_deref());
        for (k, val) in crate::addon::json_of(&f, mine) {
            v[k] = val;
        }
    }
    // Both ends of a follow-up (`wsp-345`), each only where there is one, so a
    // row that has neither prints what it did.
    if let Some(f) = &t.found_by {
        v["found_by"] = Value::String(f.clone());
    }
    let all = store.tasks();
    let found = crate::follow_up::of(&all, &t.id);
    if !found.is_empty() {
        v["follow_ups"] = json!(found.iter().map(|k| json!({ "id": k.id, "status": k.status().as_str(), "title": k.title })).collect::<Vec<_>>());
    }
    Ok(v)
}

/// `wsp show <piece-id>`: the parent's header line, then the entry whole
/// (under `--json`, [`show_json`] answers instead).
fn show_piece(a: &crate::pieces::Addressed) -> i32 {
    let p = Paint::new();
    println!("{}  {}", p.bold(&a.task.id), a.task.title);
    match (&a.piece, &a.folded) {
        (Some(piece), _) => println!("{}", piece.lines()),
        (None, Some(line)) => println!("{} (folded at land)", line.trim_end()),
        (None, None) => {}
    }
    0
}

pub fn show(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp show <id> [--log] [--fetch]");
        return 2;
    };
    // The one reader that opts in to an archived record rather than settling
    // for the sentence about it. Every other verb here resolves through
    // `task_or_why` and gets `Found::Archived`'s message, which is right for
    // them — `claim`, `edit` and `rm` have nothing sane to do with a task that
    // has left the live store. `show` does: displaying the record *is* the
    // verb, and a reader who followed `render-026`'s citation of `render-018`
    // wants the prose it points at, not the path it is filed under.
    if args.json() {
        return match show_json(store, &needle) {
            Ok(v) => {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                0
            }
            Err(why) => {
                eprintln!("{why}");
                1
            }
        };
    }
    // A piece id (`docs/pieces.md`, *Seen and told*): the row's header line and
    // the entry whole, or the line it folded into once the row has landed.
    match crate::pieces::addressed(store, &needle) {
        Ok(Some(a)) => return show_piece(&a),
        Ok(None) => {}
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    }
    let (mut t, archived) = match store.resolve_task(&needle) {
        crate::store::Found::One(t) => (*t, None),
        crate::store::Found::Archived(t, path) => (*t, Some(path)),
        other => {
            eprintln!("{}", other.why(&needle).unwrap_or_default());
            return 1;
        }
    };
    // A row the shared-work add-on keeps as a stub only (wsp-465), brought whole.
    if args.has("fetch") {
        match crate::addon::get().fetch(store, &t.id) {
            Some(Err(why)) => {
                eprintln!("wsp: {why}");
                return 1;
            }
            Some(Ok(())) => {
                if let Some(whole) = store.task(&t.id) {
                    t = whole;
                }
            }
            None => {}
        }
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
    if let Some(u) = &t.for_user {
        println!("{} {}", p.dim(&util::pad("for", 9)), u);
    }
    if !t.refs.is_empty() {
        println!("{} {}", p.dim(&util::pad("refs", 9)), t.refs.join("  "));
    }
    println!("{} {}", p.dim(&util::pad("created", 9)), t.created);
    println!("{} {}", p.dim(&util::pad("updated", 9)), t.updated);
    // What the shared-work add-on says of the row: a stub says so, above
    // everything it does not have, and a side chain (wsp-469) says where this
    // row's offline work went.
    for line in crate::addon::get().show_lines(store, &t.id) {
        println!("{} {}", p.dim(&util::pad("hub", 9)), line);
    }
    // Above the prose rather than after it. Everything below this point reads
    // as a live record — a status, a parent, a log that stops — and a reader
    // who learns at the bottom that none of it is current has already acted on
    // the top.
    if let Some(path) = &archived {
        println!("{} {}", p.dim(&util::pad("archived", 9)), util::contract(path));
    }

    // Where it sits in the work, both ways. A sub-task read on its own is
    // missing the thing that says why it exists.
    let all = store.tasks();
    // Read from the archive only when the live store does not have it, which
    // on nearly every task is never — `show` is run constantly and the walk is
    // a directory read per archive month. What it buys is that the line stops
    // *disappearing*: a parent that gets swept took the whole `under` row with
    // it, so a sub-task read on its own lost the one thing that says why it
    // exists, and said nothing about having lost it. Silence about what was
    // not looked at is the defect this row is named for, and `show` had its
    // own copy.
    if let Some((parent, filed)) = t.parent.as_ref().and_then(|id| {
        all.iter()
            .find(|x| &x.id == id)
            .map(|x| (x.clone(), false))
            .or_else(|| store.archived_task(id).map(|(x, _)| (x, true)))
    }) {
        println!(
            "{} {}  {}{}",
            p.dim(&util::pad("under", 9)),
            p.dim(&parent.id),
            util::truncate(&parent.title, 56),
            if filed { p.dim("  · archived") } else { String::new() }
        );
    }
    // What it rests on, beside what it is under: the two are the two edges a
    // task has, and `after` is the one a run waits on.
    if !t.after.is_empty() {
        println!("{} {}", p.dim(&util::pad("after", 9)), t.after.join("  "));
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
    // The other way a row comes from another (`wsp-345`): found while doing
    // it. Beside `under` and `sub`, and never one of them, because a follow-up
    // holds nothing open; both ends say so, as both ends of a parent do.
    if let Some((origin, filed)) = t.found_by.as_ref().and_then(|id| {
        all.iter()
            .find(|x| &x.id == id)
            .map(|x| (x.clone(), false))
            .or_else(|| store.archived_task(id).map(|(x, _)| (x, true)))
    }) {
        println!(
            "{} {}  {}{}",
            p.dim(&util::pad("from", 9)),
            p.dim(&origin.id),
            util::truncate(&origin.title, 56),
            if filed { p.dim("  · archived") } else { String::new() }
        );
    } else if let Some(id) = &t.found_by {
        println!("{} {}", p.dim(&util::pad("from", 9)), p.dim(id));
    }
    for (i, k) in crate::follow_up::of(&all, &t.id).iter().enumerate() {
        println!(
            "{} {} {} {}",
            p.dim(&util::pad(if i == 0 { "follow-up" } else { "" }, 9)),
            p.dim(&k.id),
            p.dim(&util::pad(k.status().as_str(), 8)),
            util::truncate(&k.title, 56)
        );
    }

    // And where it sits in the *run*, which is the other structure a task can
    // be part of and the one the backlog cannot show. `under` says what this
    // work is for; this says what is waiting on it — a group, a barrier behind
    // it, and the seat that answers for a hand raised here. One line, and only
    // while a list is actually running: a plan somebody wrote is not a fact
    // about this task, and `wsp show` is read often enough that a line costing
    // nothing when it says nothing is the whole of the bargain.
    if let Some(l) = crate::worklist::Running::read(store).of(&t.id) {
        println!(
            "{} {}  {}",
            p.dim(&util::pad("list", 9)),
            l.list,
            p.dim(&format!("group {} of {}", l.group, l.of))
        );
    }

    for (pane, b) in store.bindings() {
        if b.get("task_id").and_then(|x| x.as_str()) == Some(t.id.as_str()) {
            println!("{} {}", p.dim(&util::pad("agent", 9)), pane);
        }
    }
    if let Some(c) = store.claims().get(&t.id) {
        println!("{} {}", p.dim(&util::pad("claimed", 9)), claim_line(c));
        if let Some(line) = claim_expires_line(c, util::epoch_secs()) {
            println!("{}", p.dim(&line));
        }
    } else if let Some(w) = store.worked().get(&t.id) {
        // The last agent on it, once there is no current one — the trace a
        // migration leaves, so a task never loses every sign it was worked.
        println!("{} {}", p.dim(&util::pad("worked", 9)), worked_line(w));
    }
    // What is up about this task, above the prose. `worklist-018`: a hand
    // raised with a paragraph and no sentence drew as a blank row at the seat,
    // and the seat's next move — `wsp show <id>` — said nothing about it
    // either, so the only surface that held the words was `flags.json`. It
    // costs nothing on a task with nothing raised, which is nearly all of them,
    // and it is the whole page on the few where it is not.
    let raised = raised_about(store, &t.id);
    if !raised.is_empty() {
        println!("\n{}", p.dim("RAISED"));
        for m in &raised {
            println!("  {} {}", p.red(crate::base::glyph::FLAG), p.bold(&headline(m)));
            let rest = m.body().trim();
            if !rest.is_empty() {
                for l in rest.lines() {
                    println!("    {l}");
                }
            }
            println!("    {}", p.dim(&asking(m)));
        }
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
    // The legs as a table, the tier and what each waits on printed in full — the
    // entry on disk leaves both implicit (`docs/legs.md`, *The record*).
    if let Some(table) = crate::legs::section_for_show(&t) {
        crate::model::set_section_in(&mut rest, crate::legs::SECTION, &table);
    }
    // The pieces the same way: a table, or every entry whole with `--pieces`.
    if let Some(table) = crate::pieces::section_for_show(&t, args.has("pieces")) {
        crate::model::set_section_in(&mut rest, crate::pieces::SECTION, &table);
    }
    // Lifted out and printed below rather than with the body, because it is the
    // one section that is capped. It is last in a body whatever order the file
    // was in — `set_section_in` above has just re-sorted the sections — so
    // printing it after the rest is the same page in the same order.
    let log = crate::model::section_of(&rest, "Log").unwrap_or_default();
    crate::model::set_section_in(&mut rest, "Log", "");
    if !rest.trim().is_empty() {
        println!("\n{}", rest.trim());
    }
    if !log.trim().is_empty() {
        println!("\n## Log");
        for line in log_lines(&p, &log, &t.id, args.has("log")) {
            println!("{line}");
        }
    }
    0
}

/// Log entries `wsp show` prints, most recent last.
///
/// A count of entries and never a count of characters. `core-005`: *"A brief
/// cut off at N characters loses the end, and the end is where conclusions
/// are."* Every entry printed here is printed whole; what the cap costs is
/// older entries, counted and one flag away, and never the second half of a
/// sentence.
///
/// Six, measured over this store's 421 logged tasks. It is quiet on an ordinary
/// task and bites on the fat ones, which is the shape a cap wants: 168 of the
/// 421 are abridged at all — 37 of the 156 still open — and 41% of every
/// character in every log goes. The two hold together because a task with many
/// entries is also a task with long ones.
///
/// Six rather than four, which is what the session brief keeps
/// ([`crate::cmd_brief`]), because the machine-written lines cluster at exactly
/// the end a tail keeps: `claimed by pane`, `moved under`, `renamed from` are
/// the last three things written to a task that was just picked up. At four,
/// 38% of tasks would show no written entry at all — the cap spent entirely on
/// bookkeeping. At six it is 28%, and eight buys one point more, so this is the
/// knee and not a preference.
const MAX_LOG: usize = 6;

/// The log as a page: the recent entries whole, and a line saying how many
/// earlier ones are not on it and what prints them.
///
/// The line is `wsp project show --decisions`'s, in the same words and for the
/// same reason — a log that stops after six entries reads exactly like a log
/// that has six entries, and the difference is the history somebody is about to
/// ask an agent to re-derive. Nothing is dropped from the record: this is what
/// is handed over, and `--log` is the whole of it.
fn log_lines(p: &Paint, log: &str, id: &str, full: bool) -> Vec<String> {
    let entries = log_entries(log);
    // One over is printed rather than announced. The line that would hide a
    // single entry is 40 characters and a second command; the entry it hides is
    // a transition of about 50, on the 45 tasks here where this fires. Neither
    // of those is worth a round trip, and a cap that fires for no gain is the
    // one a reader learns to type `--log` past every time.
    if full || entries.len() <= MAX_LOG + 1 {
        return entries;
    }
    let cut = entries.len() - MAX_LOG;
    let mut out =
        vec![p.dim(&format!("{cut} of {} earlier · wsp show {id} --log", entries.len()))];
    out.extend(entries.into_iter().skip(cut));
    out
}

/// A log split into entries, one string each.
///
/// An entry is a `- ` bullet and every line beneath it. Logs here are mostly one
/// line per entry — `wsp note` folds what it is given — but 287 lines in this
/// store are the continuation of the bullet above them, an indented block of
/// output or a paragraph somebody wrote by hand, and a tail counted in lines
/// would start mid-entry and print a fragment with no date on it. Anything
/// before the first bullet is kept as an entry of its own, so prose that
/// arrived in a shape this did not anticipate is counted rather than dropped.
fn log_entries(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in log.trim().lines() {
        if line.starts_with("- ") || out.is_empty() {
            out.push(line.to_string());
        } else if let Some(prev) = out.last_mut() {
            prev.push('\n');
            prev.push_str(line);
        }
    }
    out
}

/// The hands up about one task: open, or in words this build cannot read.
///
/// [`Message::needs_attention`] rather than `is_open`, because *"I cannot read
/// this"* is a reason to put something in front of a person and not a reason to
/// go quiet — the installed binary is routinely older than the tree. Answers
/// are left out: they are written to this task's log before they are delivered,
/// so they are already further down this page.
fn raised_about(store: &Store, task: &str) -> Vec<Message> {
    message::about_task(store, task)
        .into_iter()
        .filter(|m| m.reply_to.is_none() && m.needs_attention())
        .collect()
}

/// The one line a raised hand is read by.
///
/// [`Message::title`] and nothing else — one text, first line. `worklist-018`
/// is what a second field costs: `wsp flag` took a sentence, a `--title` and a
/// `--body`, every surface preferred a different one of the three, and a hand
/// carrying only the third drew as a task title and an age. The empty case is
/// `wsp flag <id>` on its own, which is a complete thing to say, so it is said
/// rather than drawn blank.
fn headline(m: &Message) -> String {
    match m.title() {
        "" => "raised this, with nothing written on it".to_string(),
        t => t.to_string(),
    }
}

/// Who is asking, how long they have been, and the verb that ends it.
///
/// The verb is named beside the words for `worklist-013`'s reason: that hand
/// was answered within minutes down another channel and stayed up for 2h14m,
/// because answering and recording were two acts and only one of them was on
/// the path of getting the work moving.
fn asking(m: &Message) -> String {
    let mut parts = vec![m.from.byline()];
    if m.ask() == Some(Ask::Claim) {
        parts.push("asks to take it".into());
    }
    let held = util::since(&m.at);
    if held > 0 {
        parts.push(util::duration_human(held));
    }
    parts.push(match m.shape() {
        Some(Shape::Question) => format!("wsp answer {} \"…\"", m.id),
        _ => format!("wsp ack {}", m.id),
    });
    parts.join(" · ")
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
/// is a different field — `wsp prio 047 low` would answer `· wsp-019
/// todo`, which is true and is not what was asked about.
fn mutate_saying<F>(store: &Store, args: &Args, verb: &str, said: Option<&str>, f: F) -> i32
where
    F: FnOnce(&mut Task),
{
    mutate_saying_at(store, args, verb, said, None, f)
}

/// [`mutate_saying`] on a row the caller has already named: `row` is the id to
/// write to in place of the first word, for a verb that took a piece id.
fn mutate_saying_at<F>(store: &Store, args: &Args, verb: &str, said: Option<&str>, row: Option<String>, f: F) -> i32
where
    F: FnOnce(&mut Task),
{
    let Some(needle) = row.or_else(|| args.rest.first().cloned()) else {
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
    if let Err(e) = write_task(store, &t, verb, &crate::cmd_message::Caller::here(store)) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }

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

/// The write a verb that changes a task ends in, for `caller`: the file, the
/// event, the commit, and the poke a review or a block owes a run.
pub fn write_task(store: &Store, t: &Task, verb: &str, caller: &crate::cmd_message::Caller) -> std::io::Result<()> {
    store.save_task(t)?;
    store.log_event(
        &format!("task-{verb}"),
        json!({ "id": t.id, "project": t.project, "status": t.status().as_str(), "title": t.title, "by": caller.by }),
    );
    store.git_commit(&format!("wsp: {verb} {} — {}", t.id, t.title));
    // A shared row's lease that no claim holds (`wsp start`'s) goes back once
    // its work is over: the add-on decides.
    crate::addon::get().row_moved(store, &t.id);
    // `wsp-134`: a review or a block may be the step a run was waiting on.
    if matches!(verb, "review" | "blocked") {
        crate::cycle::poke_task(store, &t.id, verb);
    }
    Ok(())
}

/// A status move, logged: `→ <status>`, and for a caller from the hub
/// `→ <status> · by <user> via the hub`.
pub fn move_status(t: &mut Task, status: Status, caller: &crate::cmd_message::Caller) {
    t.set_status(status);
    match caller.via_hub {
        true => t.log(&format!("→ {} · {}", status.as_str(), caller.byline())),
        false => t.log(&format!("→ {}", status.as_str())),
    }
}

pub fn set_status(store: &Store, args: &Args, status: Status) -> i32 {
    let verb = status.as_str().to_string();
    // `wsp start` on a row in a shared workspace takes the hub's lease first
    // (wsp-466): refused, naming the holder, with nothing changed.
    if status == Status::Doing {
        if let Some(t) = args.rest.first().and_then(|needle| store.find_task(needle)) {
            if let Err(why) = crate::addon::get().before_claim(store, &t.id) {
                eprintln!("wsp: {verb}: {why}");
                return 1;
            }
        }
    }
    let caller = crate::cmd_message::Caller::here(store);
    mutate(store, args, &verb, |t| move_status(t, status, &caller))
}

pub fn done(store: &Store, args: &Args, live: &dyn Fn(&str) -> bool, seats: &dyn crate::cycle::Seats) -> i32 {
    done_with(store, args, live, seats)
}

/// What a seat standing on a row is doing, as `wsp done` judges it (`wsp-344`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// At its prompt, or holding no agent: nothing a close takes from it.
    Idle,
    /// A turn in progress.
    Working,
    /// Anything else — starting, on a prompt only a person answers, unreadable.
    Unsettled,
}

pub fn standing_as(seats: &dyn crate::cycle::Seats, seat: &str) -> Standing {
    use crate::place::State;
    use std::collections::BTreeMap;
    match seats.state(seat) {
        Some(State::Idle | State::Gone | State::Empty) => Standing::Idle,
        Some(State::Working) => Standing::Working,
        None if seats.absent(seat) => Standing::Idle,
        _ => Standing::Unsettled,
    }
}

/// Who stands on a row, from its seats' readings (`wsp-315`): nobody; a seat
/// in a turn (or one nothing can read, on this seat); every one idle or gone,
/// so finished with the claim still held; or the ones nothing can read all on
/// a far machine (`far`), so lost with it.
fn holder_from(read: &[(String, Standing)], far: &dyn Fn(&str) -> bool) -> crate::classify::Holder {
    use crate::classify::Holder;
    if read.is_empty() {
        Holder::Nobody
    } else if read.iter().any(|(_, r)| *r == Standing::Working) {
        Holder::Working
    } else if read.iter().all(|(_, r)| *r == Standing::Idle) {
        Holder::Finished
    } else if read.iter().all(|(s, r)| *r == Standing::Idle || far(s)) {
        Holder::Lost
    } else {
        Holder::Working
    }
}

/// [`holder_from`] for `id`, its seats read through `live` and `reader` as
/// `wsp done` reads them. main wires it, with the live-seat check from above
/// this layer, into the mirror's [`crate::mirror::ROW_HOLDER`].
pub fn holder_with(store: &Store, id: &str, live: &dyn Fn(&str) -> bool, reader: &dyn crate::cycle::Seats) -> crate::classify::Holder {
    let read: Vec<(String, Standing)> = standing_on(store, id, live).into_iter().map(|s| { let r = standing_as(reader, &s); (s, r) }).collect();
    holder_from(&read, &|s| far_seat(store, s))
}

/// A seat whose record names a machine (`place_compound`), as `wsp despawn
/// --lost` asks.
fn far_seat(store: &Store, seat: &str) -> bool {
    !crate::place_compound::seat_machine(store, seat).is_empty()
}

/// `wsp done` refused: the prose on stderr for the terminal, and under
/// `--json` the reason and the buttons that would get it closed
/// ([`crate::classify::next_steps`]) as one object on stdout (`wsp-315`).
/// Exits 1, as a refusal always has. `open`, the row's open sub-tasks where
/// they are in the way, is carried as `open_subtasks`.
fn refuse_done(args: &Args, id: &str, why: &str, open: usize, buttons: Vec<crate::classify::Button>) -> i32 {
    eprintln!("{why}");
    let mut v = refusal_json(id, why, &buttons);
    if open > 0 {
        v["open_subtasks"] = open.into();
    }
    if args.json() {
        println!("{v}");
    }
    #[cfg(any(test, feature = "test-support"))]
    tests::REFUSED.with(|r| *r.borrow_mut() = Some(v));
    1
}

/// The shape `wsp done <id> --json` prints when it refuses.
fn refusal_json(id: &str, why: &str, buttons: &[crate::classify::Button]) -> serde_json::Value {
    json!({
        "id": id,
        "done": false,
        "reason": why.strip_prefix("wsp: ").unwrap_or(why),
        "buttons": buttons.iter().map(crate::classify::Button::json).collect::<Vec<_>>(),
    })
}

/// The seats still standing on `id`, named: a row's bindings, leaving out a
/// leg's (the run ends those on a tick) and a seat `live` cannot find.
pub fn standing_on(store: &Store, id: &str, live: &dyn Fn(&str) -> bool) -> Vec<String> {
    let bindings = store.bindings();
    store
        .panes_for_task(id)
        .into_iter()
        .filter(|pane| bindings.get(pane).is_some_and(|b| b.get("leg").is_none()))
        .filter(|pane| live(pane))
        .collect()
}

/// [`done`], with the question "is this seat standing" passed in.
///
/// **`wsp done` refuses a row whose seat is still up (`wsp-582`), and ends nothing.**
/// It used to release the claim and clear the binding and leave the seat running:
/// two hours on, `wsp despawn <row>` found nothing bound and exited 2, the idle
/// seat sat in `wip` and held the cap, and a far one went on holding its lease.
/// Refusing, not ending it, because ending a seat is `despawn`'s and carries its
/// gate (the primary seat gives a why and not mid-turn; a governor ends its own
/// list's) — a close that ended seats would be a way round that. The refusal
/// names the seat and the command; the row is untouched.
///
/// **Except a verified row whose seats are idle or gone (`wsp-344`).** Its work
/// is done and agreed, so the close ends each idle seat as `despawn` does
/// (its gate, a why, the tree kept for the close's own sweep below), releases
/// the claim and closes the row in the one call. A seat in a turn still
/// refuses it, and the refusal says a turn is in progress; so does one that
/// is starting, on a person's prompt, or that nothing can read.
pub fn done_with(store: &Store, args: &Args, live: &dyn Fn(&str) -> bool, reader: &dyn crate::cycle::Seats) -> i32 {
    if let Some(t) = args.rest.first().and_then(|needle| store.find_task(needle)) {
        // Another user leased it while this spoke was offline (wsp-469).
        if let Some(why) = crate::addon::get().refusal(store, &t.id, "done") {
            return refuse_done(args, &t.id, &why, 0, Vec::new());
        }
        let seats = standing_on(store, &t.id, live);
        let read: Vec<(String, Standing)> = seats.iter().map(|s| (s.clone(), standing_as(reader, s))).collect();
        let working: Vec<&str> = read.iter().filter(|(_, r)| *r == Standing::Working).map(|(s, _)| s.as_str()).collect();
        let verified = !seats.is_empty() && crate::verification::verified(&store.tasks(), &t.id);
        if verified && !working.is_empty() {
            let why = format!(
                "wsp: {} is verified, but a turn is in progress at {} — so it is NOT done and nothing was closed. \
                 `wsp done {}` again once the turn ends closes it and ends the seat",
                t.id,
                working.join(", "),
                t.id
            );
            return refuse_done(args, &t.id, &why, 0, Vec::new());
        }
        if verified && read.iter().all(|(_, r)| *r == Standing::Idle) {
            let why = format!("wsp done {}: its verification holds and the seat is idle", t.id);
            for (seat, _) in &read {
                if let Err(e) = crate::cycle::despawn_seat_because(seat, &why) {
                    eprintln!("wsp: could not end {seat} ({}) — its claim is released and {} closed; `wsp despawn --pane {seat}` ends it", util::truncate(e.trim(), 200), t.id);
                }
            }
            // Read the row again: a real despawn runs in its own process, and
            // its release writes "released after …" on this row's log
            // (`wsp-509`). Saving the copy read before it would drop that line.
            let mut t = store.task(&t.id).unwrap_or_else(|| t.clone());
            t.log(&format!("wsp: verification holds and {} sat idle — `wsp done` ended it and released the claim", seats.join(", ")));
            let _ = store.save_task(&t);
        } else if !seats.is_empty() {
            let turn = match working.is_empty() {
                true => String::new(),
                false => format!(" (a turn is in progress at {})", working.join(", ")),
            };
            let why = format!(
                "wsp: {} still has a seat running — {}{turn} — so it is NOT done and nothing was closed. \
                 `wsp despawn {}` ends the seat (and says if it kept the tree), then `wsp done {}` again; \
                 a far seat on a machine that is gone is `wsp despawn {} --lost`",
                t.id,
                seats.join(", "),
                t.id,
                t.id,
                t.id
            );
            let holder = holder_from(&read, &|s| far_seat(store, s));
            let open = match args.has("force") {
                true => 0,
                false => crate::resolve::counts_under(&store.tasks(), &t.id).open,
            };
            return refuse_done(args, &t.id, &why, open, crate::classify::next_steps(&t.id, holder, open));
        }
    }
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
            let why = format!("wsp: {} has {open} open sub-task(s) — finish them, or `wsp done {} --force`", t.id, t.id);
            return refuse_done(args, &t.id, &why, open, crate::classify::next_steps(&t.id, crate::classify::Holder::Nobody, open));
        }
        for pane in store.panes_for_task(&t.id) {
            store.clear_binding(&pane);
        }
        Seats::hand_off(crate::cycle::seats(), store, &t.id, None, "done");
    }
    let id = args.rest.first().and_then(|needle| store.find_task(needle)).map(|t| t.id);
    let code = mutate(store, args, "done", |t| {
        t.set_status(Status::Done);
        t.log("→ done");
    });
    // And its tree, where the work is on the trunk (`wsp-305`). After the
    // claim is let go, so a claim this verb has just ended does not keep it.
    if let Some(id) = id.filter(|_| code == 0 && !args.has("keep-tree")) {
        let out = crate::cmd_checkout::sweep_closed(store, &[id], false);
        for line in crate::cmd_checkout::closed_lines(&out) {
            // Beside the receipt, and off stdout under --json, which is one
            // object a caller parses.
            match args.json() {
                true => eprintln!("wsp: {line}"),
                false => println!("  {}", Paint::new().dim(&line)),
            }
        }
    }
    code
}

/// `wsp reopen <id> "what is owed"` — sending work back, as one act.
///
/// **Sending work back and reopening the row were two acts, and only one of
/// them moved the run.** `wsp-142` again, from the other side: on 2026-10-04 a
/// member landed and reviewed, its verifier found the work wanted installing
/// and driving live, the governor `wsp tell`'d it that — and a tell does not
/// touch status. So the row stayed at `review`, every member read as settled,
/// and `cycle.rs` opened a barrier at 16:51 with cpd-244 still holding work
/// outstanding.
///
/// **The predicate was already right.** `Standing::finished` reads
/// `settled()`, so a member back at `doing` holds the group and `at_barrier`
/// would have been false. The governor could have prevented all of it with
/// this verb. What was missing is that the verb did not exist and sending work
/// back was a thing you typed into a pane — a conversation, which may be a
/// question, a thanks, or a status change, and nothing downstream can tell the
/// difference.
///
/// **A reason is required, not optional.** It goes on the row, and it is the
/// sentence delivered to the member's pane. A reopen with no reason would put
/// work back into a group with nothing to act on and nothing to read at the
/// other end — which is the state this verb exists to end.
///
/// **Not this row:** inferring a reopen from a bare `wsp tell`. The row's own
/// decision stands, and it is the right one: a status change has to be stated,
/// because a tell is prose and prose is ambiguous.
pub fn reopen(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp reopen <id> \"what is owed\"   (or `-` to read it from stdin)";
    let (reason, _) = match prose_payload(args, USAGE) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let code = mutate_saying(store, args, "reopen", Some(&reason), |t| {
        t.set_status(Status::Doing);
        t.log(&format!("sent back: {}", reason.trim()));
    });
    if code == 0 {
        deliver_the_reason(store, args, reason.trim());
        let run = took_back_held_run(store, args, reason.trim());
        reseat(store, args, run);
    }
    code
}

/// Who picks the work up, when nobody is holding the row. `wsp-304`.
///
/// **A reopen whose agent was gone left the row at `doing` with nobody on
/// it.** compound-581 landed, was verified, its agent was released after 28m,
/// and a regression sent it back: the row sat at `doing` and Ed had to ask
/// for it to be forced open. The reason was on the row and nothing was going
/// to read it.
///
/// **The run seats its own, and this seats the rest — never both.** A member
/// of the group its run stands at ([`crate::worklist::Running::in_front`]) is
/// the cycle's: its step reseats a member sent back with nobody on it
/// (`sent_back_unseated`, `wsp-286`) under the group's policy and the seat's
/// cap, so here it is only poked. So is a member of a group not yet reached,
/// whose step comes when the run gets there, and a member of a held or parked
/// list starts nothing, as `hold` and `park` promise. Any other row — on no
/// list, or in a group already crossed — has no step to take it, so the reopen
/// starts one itself: `wsp spawn <id> --agent` with the kind, model and effort
/// it last ran with, the line its sender would otherwise have typed. Its work
/// order is the brief, whose log ends in the `sent back:` line.
///
/// **Nobody holding it** is no binding and no claim, as for the run. A claim
/// whose seat went is [`crate::repair`]'s. A row run as legs is sent back to a
/// leg, which a reopen does not write, and a row no agent ever claimed was
/// somebody's own work; both are said, with the verb that would seat one,
/// rather than guessed at. `--no-seat` is the reopen that wants nobody yet.
fn reseat(store: &Store, args: &Args, run: Option<String>) {
    let Some(t) = args.rest.first().and_then(|n| store.task(n)) else { return };
    let id = t.id.clone();
    if t.status() != Status::Doing || !store.panes_for_task(&id).is_empty() || store.claims().contains_key(&id) {
        return;
    }
    let p = Paint::new();
    let say = |s: &str| {
        if !args.json() {
            println!("  {}", p.dim(s));
        }
    };
    if crate::pieces::staged(&t) {
        say(&format!(
            "no agent holds {id}, and it is run as pieces: a reopen opens no piece — `wsp piece add {id} …` puts what is owed on one, and wsp starts it"
        ));
        return;
    }
    if crate::legs::staged(&t) {
        say(&format!(
            "no agent holds {id}, and it is run as legs: a reopen opens no leg — `wsp leg add {id} …` puts what is owed on one, and wsp starts it"
        ));
        return;
    }
    let running = crate::worklist::Running::read(store);
    if let Some(p) = running.of(&id) {
        if running.in_front(&id) {
            // A held run taken back was poked by `reopened`, and said so.
            if run.is_none() {
                crate::cycle::poke_task(store, &id, "reopen");
                say(&format!("no agent holds {id}: {} stands at its group, and the run seats it", p.list));
            }
            return;
        }
        if running.front(&p.list).is_some_and(|at| p.group > at) {
            say(&format!(
                "no agent holds {id}: it is in group {} of {}, which the run has not reached, and the run seats it there",
                p.group, p.list
            ));
            return;
        }
    }
    let stopped = store.worklists().into_iter().find(|w| {
        matches!(w.status(), crate::model::WorklistStatus::Held | crate::model::WorklistStatus::Parked)
            && w.groups().iter().any(|g| g.members.iter().any(|m| *m == id))
    });
    if let Some(w) = stopped {
        say(&format!(
            "no agent holds {id}, and {} is {}: nothing starts until it runs again — `wsp worklist show {}` says why",
            w.id,
            w.status().as_str(),
            w.id
        ));
        return;
    }
    let ran = ran_with(store, &t);
    if args.has("no-seat") {
        let line = ran.map(|a| a.join(" ")).unwrap_or_else(|| format!("spawn {id} --agent"));
        say(&format!("no agent holds {id} and nothing will pick it up — `wsp {line}` seats one"));
        return;
    }
    let Some(argv) = ran else {
        say(&format!(
            "no agent holds {id}, and none ever claimed it — nothing will pick it up; `wsp spawn {id} --agent` seats one"
        ));
        return;
    };
    say(&format!("no agent holds {id} — seating one as it ran: wsp {}", argv.join(" ")));
    let (said, code) = respawn(&argv, args.json());
    if code != 0 {
        eprintln!("wsp: {id} is at doing and nobody is on it — the spawn did not start ({said}); `wsp {}` retries", argv.join(" "));
    }
    let line = match code {
        0 => format!("reseated after reopen: wsp {}", argv.join(" ")),
        _ => format!("reseat after reopen failed: {said}"),
    };
    crate::store::log_line(store, &format!("{id}: {line}"));
}

/// `spawn <id> --agent` with the kind, model and effort the row last ran with,
/// or `None` for a row no agent ever claimed.
///
/// **The tier is the claim line's `spawned at` clause** — what was asked for,
/// in the words `--model` and `--effort` take — and not the release line's
/// `ran`, which names what served (`sonnet-5-5`) and is not something to type.
/// The effort is the last `/` segment, as [`crate::cmd_attempts`] reads it,
/// because an opencode model has a `/` of its own. `default` is a half that
/// was never stated, so it is left unstated again.
///
/// **The kind is the session wsp last learned for the row**
/// ([`crate::cmd_resume::thread_for_task`], the record and then the event
/// log), and the default kind where none was learned.
fn ran_with(store: &Store, t: &Task) -> Option<Vec<String>> {
    let log = t.section("Log").unwrap_or_default();
    let claim = log.lines().filter(|l| l.contains("claimed by pane ")).last()?;
    let kind = Seats::thread_kind(crate::cycle::seats(), store, &t.id).unwrap_or_default();
    let mut argv = vec!["spawn".to_string(), t.id.clone(), "--agent".to_string()];
    argv.extend(["--kind".to_string(), crate::agent_kind::kind_or_default(&kind)]);
    let tier = claim.split(" · spawned at ").nth(1).and_then(|r| r.split(" · ").next()).map(str::trim);
    if let Some((model, effort)) = tier.and_then(|s| s.rsplit_once('/')) {
        for (flag, v) in [("--model", model), ("--effort", effort)] {
            if !v.is_empty() && v != "default" {
                argv.extend([flag.to_string(), v.to_string()]);
            }
        }
    }
    Some(argv)
}

/// Run `wsp <argv>` as the sender would have, in their environment, so the
/// spawn records them as its spawner. Its own output is the receipt.
fn respawn(argv: &[String], json: bool) -> (String, i32) {
    if crate::base::under_test() {
        #[cfg(any(test, feature = "test-support"))]
        tests::RESEATED.with(|r| r.borrow_mut().push(argv.to_vec()));
        return (String::new(), 0);
    }
    let Ok(exe) = wsp_base::util::self_exe() else { return ("no wsp binary to run".to_string(), 1) };
    let mut cmd = wsp_base::calls::Command::new(exe);
    cmd.args(argv).stdin(std::process::Stdio::null());
    if json {
        cmd.stdout(std::process::Stdio::null());
    }
    match cmd.status() {
        Ok(s) if s.success() => (String::new(), 0),
        Ok(s) => (format!("exit {}", s.code().unwrap_or(-1)), 1),
        Err(e) => (e.to_string(), 1),
    }
}

/// A member of a held run, sent back, takes the run back to its group —
/// [`crate::cycle::reopened`] says when and why. Said here, after the row's own
/// line, because the person who typed this is the one who should know their
/// reopen also un-held a run. Returns the list, so [`reseat`] does not say it
/// twice.
fn took_back_held_run(store: &Store, args: &Args, reason: &str) -> Option<String> {
    let id = args.rest.first().and_then(|n| store.task(n)).map(|t| t.id)?;
    let (list, at) = crate::cycle::reopened(store, &id, reason)?;
    if !args.json() {
        let p = Paint::new();
        println!(
            "{}",
            p.dim(&format!(
                "{list} was held at barrier {at}: it runs group {at} again, no verdict recorded — wsp seats {id} and checks the barrier afresh once it is back at review"
            ))
        );
    }
    Some(list)
}

/// Put the reason in the member's own pane, through the one delivery path.
///
/// **A refusal is said, not hidden, and does not undo the move.** The row is at
/// `doing` now whatever the pane can take — that is the record the run reads,
/// and leaving it at `review` because a sentence would not type is the failure
/// this verb was filed for. So the move stands, the reason is on the row, and
/// the governor is told the pane did not get it.
///
/// `wsp tell` is run as a process rather than called, because its refusals are
/// the answer worth having — "it is stopped on a prompt only a person can
/// answer", "it cannot say whether it is at a prompt" — and reimplementing them
/// here would be a second answer that could disagree with the verb an agent
/// would otherwise have run.
fn deliver_the_reason(store: &Store, args: &Args, reason: &str) {
    let Some(needle) = args.rest.first().cloned() else { return };
    let Some(id) = store.task(&needle).map(|t| t.id) else { return };
    deliver_reason(store, &id, reason);
}

/// The delivery itself, by id: `wsp verified --blocks` sends a member back the
/// way `reopen` does and says it through the same one path.
pub fn deliver_reason(store: &Store, id: &str, reason: &str) {
    if matches!(deliver_as(store, id, &format!("wsp sent {id} back: {reason}")), Told::Pending | Told::No) {
        crate::repair::note_tell_pending(store, id);
    }
}

/// A sentence for a member's pane that is not a send-back: a line to fix, say.
/// Said once; not retried.
pub fn deliver_notice(store: &Store, id: &str, text: &str) {
    deliver(store, id, &format!("wsp: {text}"));
}

/// Types `line` to the member's pane. `false` only when there was a pane and it
/// did not get it; no pane is nothing owed.
pub fn deliver(store: &Store, id: &str, line: &str) -> bool {
    deliver_as(store, id, line) == Told::Yes
}

/// What typing a sentence to a member's pane came to (`wsp-469`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Told {
    /// The pane has the sentence, or there was no pane and nothing is owed.
    Yes,
    /// The backend did not finish in time and will be asked again under the
    /// same id: not a refusal, and the seat is not at fault.
    Pending,
    /// The pane refused, or `wsp tell` could not run.
    No,
    /// `wsp tell`'s prose check refused wsp's own sentence. The same sentence
    /// would be refused again, so nothing retries it: the governor was told.
    Unsayable,
}

/// [`deliver`] saying which way it did not deliver. **A pending delivery is
/// not a failed one**: compound's `deliver` answers a seat that is busy for
/// longer than the tick waits with `delivery still pending after 30s, asked
/// again next tick under the same id`, and a caller that reads that as a refusal
/// ends a seat that was never asked twice (the verdict path of the same
/// mistake `cmd_message`'s held answers made, `wsp-469`).
pub fn deliver_as(store: &Store, id: &str, line: &str) -> Told {
    deliver_with(store, id, line, false)
}

/// [`deliver_as`] for a caller that has itself read the seat as idle at a
/// prompt nobody is answering, and so says `--anyway` as a person who has looked
/// would (`wsp-658`): the run's retry of a send-back. Without it, a seat the run
/// reads idle and `wsp tell` reads as unable to vouch for itself (a compound
/// seat whose last hook is stale and whose screen did not answer) was refused
/// every idle tick and sat at an empty prompt until somebody typed `--anyway`.
pub fn deliver_insisting(store: &Store, id: &str, line: &str) -> Told {
    deliver_with(store, id, line, true)
}

fn deliver_with(store: &Store, id: &str, line: &str, anyway: bool) -> Told {
    // Nothing to tell: a row nobody is holding has no pane, and saying so
    // would be noise on every reopen of finished work.
    if store.panes_for_task(id).is_empty() {
        return Told::Yes;
    }
    // A reason built from a command's output (a rebase, a land, a check) carries
    // what `wsp tell` refuses (`wsp-846`).
    let line = util::said_plainly(line);
    let line = line.as_ref();
    if cfg!(test) {
        #[cfg(test)]
        {
            if crate::cycle::tests::TELL_PENDING_BACKEND.with(|r| r.get()) {
                return Told::Pending;
            }
            if crate::cycle::tests::TELL_REFUSED.with(|r| r.get()) {
                return Told::No;
            }
            if !anyway && crate::cycle::tests::TELL_UNVOUCHED.with(|r| r.get()) {
                return Told::No;
            }
            // `wsp tell`'s own check of what it reads on stdin.
            let refused = util::terminal_output(line).or_else(|| {
                crate::cycle::tests::TELL_PROSE_REFUSED.with(|r| r.get()).then(|| util::TERMINAL_OUTPUT.to_string())
            });
            if let Some(why) = refused {
                return unsayable(store, id, &why);
            }
            crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().push((id.to_string(), line.to_string())));
        }
        return Told::Yes;
    }
    let Ok(exe) = wsp_base::util::self_exe() else { return Told::No };
    let mut argv = vec!["tell", id, "-"];
    if anyway {
        argv.push("--anyway");
    }
    let mut child = match wsp_base::calls::Command::new(exe)
        .args(&argv)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            crate::store::log_line(store, &format!("tell {id}: could not run `wsp tell` ({e})"));
            return Told::No;
        }
    };
    if let Some(mut to) = child.stdin.take() {
        use std::io::Write;
        let _ = writeln!(to, "{line}");
    }
    let (told, said) = match child.wait_with_output() {
        Ok(o) if o.status.success() => (Told::Yes, "it has the sentence".to_string()),
        Ok(o) if String::from_utf8_lossy(&o.stderr).contains(util::TERMINAL_OUTPUT) => {
            return unsayable(store, id, util::fold(&String::from_utf8_lossy(&o.stderr)).as_str());
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let told = if err.contains(crate::place_compound::DELIVERY_PENDING) { Told::Pending } else { Told::No };
            (told, format!("not delivered — {}", util::truncate(err.trim(), 120)))
        }
        Err(e) => (Told::No, format!("not delivered — {e}")),
    };
    crate::store::log_line(store, &format!("tell {id}: {said}"));
    if told != Told::Yes {
        crate::store::log_line(store, &format!(
            "{id} is at doing with the reason on its row, but its pane did not get it — wsp retries when its seat reads idle, or `wsp tell {id} --anyway -`"
        ));
    }
    told
}

/// `wsp tell` refused wsp's own sentence as terminal output. `--anyway` does not
/// pass that check and the next idle tick would type the same words, so it is
/// said once: on the row, in the run's log, and to whoever governs the list.
fn unsayable(store: &Store, id: &str, why: &str) -> Told {
    let w = store.find_task(id).and_then(|t| crate::cycle::list_of(store, &t));
    let then = format!("`wsp show {id}` has the reason; say it to the seat with `wsp tell {id} -`.");
    report_unsayable(store, &Unsaid { to: id, row: Some(id), list: w.as_ref(), mark: "", then: &then }, why);
    Told::Unsayable
}

/// A sentence of wsp's that `wsp tell` refused: who it was for, and where that
/// is recorded.
pub(crate) struct Unsaid<'a> {
    /// What `wsp tell` was given: a row or a seat.
    pub to: &'a str,
    /// The row that carries the line; a barrier check's seat has none, and
    /// `list`'s log carries it.
    pub row: Option<&'a str>,
    pub list: Option<&'a crate::model::Worklist>,
    /// Ends the line, for a caller that looks for it before saying the same again.
    pub mark: &'a str,
    /// What the governor can do about it.
    pub then: &'a str,
}

pub(crate) fn report_unsayable(store: &Store, u: &Unsaid, why: &str) {
    let why = util::truncate(&util::fold(&util::said_plainly(why)), 160);
    let to = u.to;
    crate::store::log_line(store, &format!("tell {to}: not delivered, and not retried — {why}"));
    let line = format!("{}: {why}{}", crate::repair::TELL_UNSAYABLE, u.mark);
    store.locked(|| {
        if let Some(mut t) = u.row.and_then(|id| store.find_task(id)) {
            let _ = crate::log::note(store, &mut t, &line);
        } else if let Some(mut w) = u.list.and_then(|w| store.worklist(&w.id)) {
            let _ = crate::log::note_to_list(store, &mut w, &line);
        }
    });
    let seat = if u.row == Some(to) { format!("{to}'s seat") } else { format!("seat {to}") };
    let text = format!(
        "{seat} was NOT told: `wsp tell` refused wsp's own sentence ({why}), and wsp will not retry it. {}",
        u.then
    );
    match u.list {
        Some(w) => crate::cycle::tell(store, w, &text),
        None => {
            crate::cycle::notice_on_row(store, u.row.unwrap_or(to), &text);
        }
    }
}

/// Reads its prose the same way `note` does — see [`payload_source`].
///
/// The reason a block wants a stream is not length, it is that the sentence is
/// nearly always *a question about the code*: which of two shapes, whether a
/// flag means what it looks like, what `wsp verify --alone` is supposed to do
/// when the tree is dirty. That is backtick-shaped text going through a shell,
/// which is the fault this whole survey is about.
pub fn block(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp block <id> \"the question\"   (or `-` to read it from stdin)";
    let (reason, from) = match prose_payload(args, USAGE) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let said = from.map(|src| format!("{} characters from {src}", reason.len()));
    mutate_saying(store, args, "blocked", said.as_deref(), |t| {
        t.set_status(Status::Blocked);
        t.log(&format!("blocked: {reason}"));
    })
}

/// The account, and the gate that is the whole of `robustness-101`.
///
/// `review` was [`set_status`] with no argument. That is the shape of a verb
/// you type at the end, and twelve of the thirteen opencode rows wsp had run
/// by 2026-08-25 typed it and needed a governor to ask for the rest:
/// `render-033` committed and never landed, `render-014` left two modified
/// files in its tree with nothing on its branch, `worklist-ui-003` landed
/// itself and wrote not a word, `ui-007` had nothing to land — which was
/// correct — and still wrote its account only when it was asked. That asking
/// is a cost `wsp attempts` cannot see, because it is spent by the seat and
/// not by the agent.
///
/// # Why the account is the thing refused, and not the commit
///
/// The obvious gate is the title's: refuse when nothing is ahead of the trunk.
/// It is wrong twice. It **passes** `worklist-ui-003` and `-006`, which
/// committed and landed and wrote nothing, and it **fails** `ui-007`, which was
/// genuinely finished with no diff — a row with nothing to land is correct to
/// sit at review, and a verb that refuses it is a verb people learn to force.
/// What is missing in all four is the same thing, and it is not a commit: it
/// is the row's own prose.
///
/// And prose is worth refusing over on its own evidence. `compound-002`, asked
/// for an account of work already at review, wrote: *"CLOEXEC on the master —
/// writing this note found it missing and 6495b78 adds it."* Writing the
/// account found a real defect in finished work. It is not bookkeeping and it
/// is not a report for the governor's convenience; it is the one review step in
/// the flow that is not a machine check, and it is the agent performing it on
/// itself.
///
/// # Why it is taken here rather than looked for
///
/// A gate that *searches* for an account has to know where one lives, and on
/// 2026-08-24 a governor and an agent disagreed about that in good faith:
/// `compound-006` had appended its account to the row's **overview**, the
/// governor was reading the **log**, and nothing in wsp said which was meant.
/// Taking the account as the payload settles it by construction — the answer to
/// "where does an account go" is "into `wsp review`, which puts it in the log",
/// and there is no heuristic left to be wrong.
///
/// It also survives the thing habit does not. One agent wrote accounts
/// unprompted on two consecutive rows and stopped the moment its window was
/// cleared, because the habit lived in accumulated context. A gate on the verb
/// is in the verb.
///
/// Same payload as [`block`] and [`park`], down to the stream form, because it
/// is the same kind of sentence and this store already teaches `-`.
pub fn review(store: &Store, args: &Args) -> i32 {
    const USAGE: &str =
        "wsp review <id> \"what you did, and what you left\"   (or `-` to read it from stdin)";
    // `wsp review --auto-close off|<hours> -p <project>`: a project's setting, no row.
    if let Some(word) = args.get("auto-close") {
        let project = args.get("project").or_else(|| args.get("p"));
        return match crate::auto_close::set_setting(store, &word, project.as_deref()) {
            Ok(said) => {
                println!("{said}");
                0
            }
            Err(why) => {
                eprintln!("wsp: {why}");
                2
            }
        };
    }
    // A seat whose claim expired does not review the row over its next holder
    // (`wsp-412`): refused before anything is read or written.
    if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
        if let Some(why) = crate::cmd_submit::expired_claim_refusal(store, &t, crate::place::my_pane().as_deref()) {
            eprintln!("{why}");
            return 1;
        }
        if let Some(why) = crate::addon::get().refusal(store, &t.id, "review") {
            eprintln!("{why}");
            return 1;
        }
    }
    // Given no account: the one written at the place wsp keeps for it (`wsp-374`,
    // `$WSP_VERDICT`), when it was written in this run of the row.
    if let Some((from_file, written)) = crate::closing::member_args(store, args) {
        let mut from_file = from_file;
        if let Some(why) = args.get("anyway") {
            from_file.flags.insert("anyway".into(), vec![why]);
        }
        let code = review(store, &from_file);
        if code == 0 {
            crate::closing::note_close(store, &args.rest[0], "review", written, None);
        }
        return code;
    }
    // No id at all is a different mistake and gets the plain usage. The
    // handbook's line is `Finished? wsp review`, *without* one, so this is a
    // sentence agents type — and answering it with a lecture about accounts
    // buries the word it is actually missing.
    if args.rest.is_empty() {
        eprintln!("usage: {USAGE}");
        return 2;
    }
    // An id and nothing after it. Handled before `prose_payload`, which would
    // print `usage:` and stop: this is the refusal the whole task is about and
    // the one moment an agent is certainly reading, so it says what an account
    // is, where it goes, and how to get prose past a shell — and then names
    // what it can see of the work, which is where `render-014` would have
    // learnt about the two files it had left in its tree.
    if args.rest.len() < 2 {
        let p = Paint::new();
        eprintln!("wsp: review takes the account — what you did, what you left undone, and what the next reader needs");
        eprintln!("usage: {USAGE}");
        eprintln!(
            "     {}",
            p.dim("it goes in the log, which is what `wsp show` prints last and what a barrier reads")
        );
        if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
            for line in outstanding_for(store, &t.id) {
                eprintln!("     {}", p.dim(&line));
            }
        }
        return 2;
    }
    if let Some(lines) = args.rest.first().and_then(|n| store.find_task(n)).as_ref().and_then(uncaptioned) {
        for line in lines {
            eprintln!("{line}");
        }
        return 2;
    }
    let (account, kept, from) = match review_payload(args, USAGE) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let said = from.map(|src| format!("{} characters from {src}", account.len()));
    for line in exit_warnings(store, args, &kept) {
        eprintln!("wsp: warning (a run seat's review is read as an exit; `wsp submit` will refuse this) — {line}");
    }
    accept(store, args, &account, said.as_deref())
}

/// What the submit validator would refuse in a bare `wsp review`'s words, from
/// a run seat only (plan D2: a warning, never a refusal). The summary is the
/// first line, the account the rest. Outside a run: nothing.
pub fn exit_warnings(store: &Store, args: &Args, account: &str) -> Vec<String> {
    let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) else { return Vec::new() };
    if !crate::worklist::verified_by_a_run(store, &t.id) {
        return Vec::new();
    }
    let text = crate::cmd_submit::exit_of_review(account);
    let c = crate::cmd_submit::validate(store, &t, &text, args.get("anyway").as_deref());
    crate::cmd_submit::refusal_lines(&c)
}

/// A review's account twice, from one read: folded to the one line the log
/// keeps, and with its lines kept for [`exit_warnings`], which reads its
/// sections by their headings (`wsp-137`: folded, a stream's `done-when:` was
/// gone, and every review from a run seat warned that it had none — at the
/// line after the five a one-line account wraps to). Then where it came from.
pub fn review_payload(args: &Args, usage: &str) -> Result<(String, String, Option<String>), i32> {
    match payload_source(args.rest.get(1..).unwrap_or_default()) {
        Some(src) => {
            let (kept, named) = prose_read(&src, |raw| raw.trim().to_string())?;
            Ok((fold(&kept), kept, Some(named)))
        }
        None => {
            let (folded, _) = prose_payload(args, usage)?;
            Ok((folded, args.text(1).trim().to_string(), None))
        }
    }
}

/// **The review core**: what `wsp review` does once it holds an account, and
/// what `wsp submit` runs once a member's exit is accepted (`wsp-613`, the
/// exit plan's "acceptance is the same core"). Every gate a review has — the
/// treecopy match, an owed land, the ready commit of a clean tree, wsp-529's
/// blocked commit, `## Checks` that cannot start, a leg's siblings — and then
/// the row to `review` with `account` as its `review:` line. `args` names the
/// row as its first word and may carry `--anyway` and `--hold-review`.
pub fn accept(store: &Store, args: &Args, account: &str, said: Option<&str>) -> i32 {
    let code = accept_now(store, args, account, said);
    // `wsp-691`: what the row left running in its tree goes with its submit,
    // all but this seat's agent and the chain this command runs on.
    if code == 0 {
        if let Some(id) = args.rest.first().and_then(|n| store.find_task(n)).map(|t| t.id) {
            crate::ending::stop_in_tree(store, &id, true, &format!("{id} submitted"));
        }
    }
    code
}

/// [`accept`] before its sweep.
fn accept_now(store: &Store, args: &Args, account: &str, said: Option<&str>) -> i32 {
    // A row copied to an executor is reviewed only at the tree `verify` ran on.
    if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
        if let Some(why) = crate::addon::get().refusal(store, &t.id, "submit") {
            eprintln!("{why}");
            return 1;
        }
        if let Some(tree) = crate::cmd_checkout::member_tree(store, &t.id) {
            if let Err(why) = crate::treecopy::review_refusal(store, &t.id, &tree) {
                eprintln!("wsp: {why}");
                return 1;
            }
        }
    }
    // A hold that owes its land (`wsp-605`): reviewed again at the commit it
    // read, the land is tried with the recorded hold and no new verifier.
    // Landed, the review goes on as for any landed row; refused, it is said
    // here, on the row, and to the governor.
    if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
        match crate::verification::retry_owed(store, &t.id, "reviewed again") {
            Some(crate::verification::Owed::Landed(sha)) => {
                let at = sha.map(|s| format!(" at {}", &s[..s.len().min(12)])).unwrap_or_default();
                println!("✓ {}: the verifier's hold was owed its land, and it landed{at} — no new verifier", t.id);
            }
            Some(crate::verification::Owed::ByMerge(why)) => {
                println!("{why}");
            }
            Some(crate::verification::Owed::Still(why)) => {
                eprintln!("{why}");
                eprintln!("wsp: {} stays at review with its land owed; its governor is told", t.id);
                return 1;
            }
            Some(crate::verification::Owed::Blocked(why)) => {
                eprintln!("{why}");
                eprintln!("wsp: {}'s owed land was refused, so the hold is recorded as a block and the row is sent back", t.id);
                return 1;
            }
            None => {}
        }
    }
    // A member of a group wsp runs is not landed by its agent: it is marked
    // ready at its tree's commit, and the verifier that reads that tree lands
    // it. Refused before the row moves when the tree holds uncommitted work.
    let ready_line = match args.rest.first().and_then(|n| store.find_task(n)) {
        Some(t) if crate::worklist::verified_by_a_run(store, &t.id) => {
            match crate::cmd_checkout::ready_at(store, &t.id) {
                Ok(at) => at.map(|(sha, branch)| format!("{} {sha} on {branch}", crate::repair::READY)),
                Err(why) => {
                    eprintln!("wsp: {why}");
                    return 1;
                }
            }
        }
        _ => None,
    };
    // The commit a verifier has already blocked, put up again unchanged, is
    // refused: a second verifier would spend a turn reaching the same verdict
    // (`wsp-529`). `--anyway "why"` is for a seat that disagrees, logged.
    let mut anyway_line = None;
    let _ = args.has("anyway");
    if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
        let tip = match crate::cmd_checkout::ready_at(store, &t.id) {
            Ok(Some((sha, _))) => Some(sha),
            _ => None,
        };
        if let Some(pass) = tip.as_deref().and_then(|tip| blocked_at(&t, tip)) {
            match args.get("anyway").filter(|w| w != "true" && !w.trim().is_empty()) {
                Some(why) => {
                    anyway_line = Some(format!(
                        "anyway: reviewed again at {} over the verifier's block ({}): {}",
                        pass.read.as_deref().unwrap_or_default(),
                        pass.decided.as_deref().unwrap_or(pass.at.as_str()),
                        fold(&why)
                    ));
                }
                None => {
                    for line in blocked_refusal(&t.id, &pass) {
                        eprintln!("{line}");
                    }
                    return 1;
                }
            }
        }
    }
    // A `## Checks` line that cannot start would be replayed at the landing and
    // fail there; it is refused here, where its author is.
    if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
        let bad = crate::check::malformed_in(&crate::check::task_steps(&t));
        if !bad.is_empty() {
            eprintln!("wsp: {} not moved to review — its ## Checks has line(s) that cannot start under `sh`:", t.id);
            for b in &bad {
                eprintln!("  - {b}");
            }
            eprintln!("  fix them with `wsp check {} --set --why \"…\" -` (a note after the command goes outside its backticks)", t.id);
            return 2;
        }
        let calls = crate::check::calls_itself_in(&t.id, &crate::check::task_steps(&t));
        if !calls.is_empty() {
            eprintln!("wsp: {} not moved to review — its ## Checks has line(s) that call its own check:", t.id);
            for b in &calls {
                eprintln!("  - {b}");
            }
            eprintln!("  fix them with `wsp check {} --set --why \"…\" -`", t.id);
            return 2;
        }
    }
    // The rule steps the row's own diff calls for (layering, readonly, help),
    // run in the member's seat at the commit it is ready at (`wsp-858`): a red
    // one keeps the row from review and from its verifier.
    if ready_line.is_some() {
        if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
            let anyway = args.get("anyway").filter(|w| w != "true" && !w.trim().is_empty());
            match rule_gate(store, &t, anyway.as_deref()) {
                Ok(said) => {
                    for line in said {
                        eprintln!("wsp: {line}");
                    }
                }
                Err(lines) => {
                    for line in lines {
                        eprintln!("{line}");
                    }
                    return 1;
                }
            }
        }
    }
    // From a leg's seat the row is not finished while a sibling runs, and from
    // the last running leg the rest are skipped (`docs/legs.md`, *A leg is a
    // seat*). Refused before the row moves.
    if let Some(needle) = args.rest.first() {
        if let Err(why) = crate::leg_seat::on_review(store, needle, account, crate::place::my_pane().as_deref()) {
            eprintln!("wsp: {why}");
            return 1;
        }
    }
    let hold = args.has("hold-review");
    let code = mutate_saying(store, args, "review", said, |t| {
        t.set_status(Status::Review);
        if hold && !t.tags.iter().any(|g| g == crate::auto_close::HOLD) {
            t.tags.push(crate::auto_close::HOLD.into());
        }

        // `review: …`, the shape `blocked:` and `parked:` already have — the
        // transition is implied by the prefix and the sentence is the entry.
        // `wsp attempts` reads both spellings; rows logged before this say
        // `→ review`.
        t.log(&format!("review: {account}"));
        if let Some(line) = &ready_line {
            t.log(line);
        }
        if let Some(line) = &anyway_line {
            t.log(line);
        }
    });
    if code != 0 {
        return code;
    }
    // The project's `at_review:` (`wsp-592`), now the review is recorded:
    // bounded, said in the logs, and never a reason to fail the review.
    if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
        let _ = crate::at_stop::after_review(store, &t.id);
    }
    // A warning and never a refusal, because this is exactly where `ui-007`
    // sits: nothing ahead of the trunk and right to be here. So it names what
    // it saw and leaves the judgement — and says nothing at all when the tree
    // is clean and the branch is level, which is both the row that landed
    // itself and the row that had nothing to land.
    let p = Paint::new();
    if let Some(t) = args.rest.first().and_then(|n| store.find_task(n)) {
        for line in outstanding_for(store, &t.id) {
            eprintln!("  {}", p.yellow(&line));
        }
        if let Some(line) = crate::check::review_hint(store, &t) {
            eprintln!("  {}", p.yellow(&line));
        }
        if let Some(line) = done_when_refused(&t) {
            eprintln!("  {}", p.yellow(&line));
        }
    }
    0
}

/// What a review says, once, when the row's done-when needs a verb its seat is
/// refused (`wsp-312`): wsp-303's rested on `wsp mv`, and the seat could only
/// ask and hold. A warning and never a refusal: the item may be the person's
/// to finish, and the verifier reads the exit's `not held:` for it.
fn done_when_refused(t: &Task) -> Option<String> {
    crate::split::warn_done_when(&done_when_of(t), &t.id)
}

/// The row's done-when: its `## Done when`, or the overview from the line that
/// opens one (`### Done when`, `Done when:`) on.
fn done_when_of(t: &Task) -> String {
    if let Some(dw) = t.section("Done when").filter(|s| !s.trim().is_empty()) {
        return dw;
    }
    let overview = t.section("Overview").unwrap_or_default();
    let at = overview.lines().position(|l| l.trim_start_matches(['#', ' ']).to_lowercase().starts_with("done when"));
    at.map(|n| overview.lines().skip(n).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

/// The newest pass on the row when it is a block read at `tip`: the commit a
/// seat is putting up again, unchanged (`wsp-529`).
pub fn blocked_at(t: &Task, tip: &str) -> Option<crate::verification::Pass> {
    let p = crate::verification::passes(t).pop()?;
    let read = p.own()?;
    (p.state == crate::verification::State::Blocks && crate::verification::same_commit(read, tip)).then_some(p)
}

/// What `wsp review` says to a seat putting up a commit a verifier blocked: the
/// verdict's first lines, where the whole of it is, and the two ways on.
pub fn blocked_refusal(id: &str, pass: &crate::verification::Pass) -> Vec<String> {
    let mut out = vec![format!(
        "wsp: {id} is at {}, which a verifier already blocked, and nothing has changed since — a second verifier would read the same commit",
        pass.read.as_deref().unwrap_or_default()
    )];
    out.extend(crate::cycle::verdict_head(&pass.text).lines().map(|l| format!("  | {l}")));
    out.push(format!("  the whole verdict is under ## Verification (`wsp show {id}`)"));
    out.push("  fix what it found and commit (`wsp commit-help`), then `wsp submit` again".into());
    out.push(format!(
        "  if you disagree with it: `wsp submit {id} --anyway \"why\"` puts it up as it is, logged, and the governor hears of it"
    ));
    out
}

/// The log line a review refused on a red rule step writes (`wsp-858`),
/// followed by the commit and the steps: `<this> <sha> (layering)`.
pub const RULE_RED: &str = "review refused, a rule step red at";

/// The commit the newest rule-step refusal on the row was read at.
fn rule_red_at(t: &Task) -> Option<String> {
    let log = t.section("Log")?;
    log.lines()
        .rev()
        .find_map(|l| l.split_once(RULE_RED).and_then(|(_, r)| r.split_whitespace().next().map(str::to_string)))
}

/// The rule steps (layering, readonly, help) the row's diff calls for, run in
/// its seat at the commit its tree stands at (`wsp-858`). `Ok` is what to say
/// and the review goes on: the steps were green, skipped (a cold `target/`, a
/// limit), or put up over with `--anyway`. `Err` is the refusal: a step red
/// now, or the same commit a red one already refused, unchanged.
fn rule_gate(store: &Store, t: &Task, anyway: Option<&str>) -> Result<Vec<String>, Vec<String>> {
    use crate::cmd_verify::git;
    let Some(tree) = crate::cmd_checkout::member_tree(store, &t.id) else { return Ok(Vec::new()) };
    let Some(tip) = git(&tree, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()) else { return Ok(Vec::new()) };
    let short: String = tip.chars().take(12).collect();
    let note = |line: String| {
        store.locked(|| {
            if let Some(mut m) = store.find_task(&t.id) {
                m.log(&line);
                let _ = store.save_task(&m);
            }
        });
    };
    // Put up again unchanged after a red: refused from the record, not run again.
    if rule_red_at(t).is_some_and(|at| crate::verification::same_commit(&at, &tip)) {
        return match anyway {
            Some(why) => {
                note(format!("anyway: reviewed at {short} over a red rule step: {}", fold(why)));
                Ok(vec![format!("rule steps not run again at {short}, put up over their red: {}", fold(why))])
            }
            None => Err(vec![
                format!("wsp: {} is at {short}, where its last review was refused on a red rule step, and nothing has changed since", t.id),
                format!("  the step's output is in the row's log (`wsp show {}`)", t.id),
                "  fix what it found and commit (`wsp commit-help`), then `wsp submit` again".into(),
                format!("  if the red is the tree's and not the row's: `wsp submit {} --anyway \"why\"`", t.id),
            ]),
        };
    }
    let Some(trunk) = crate::base::git::trunk(&tree) else { return Ok(Vec::new()) };
    let Some(branch) = crate::base::git::trunk_branch(&trunk) else { return Ok(Vec::new()) };
    let Some(base) = git(&tree, &["merge-base", &branch, &tip]).map(|s| s.trim().to_string()) else { return Ok(Vec::new()) };
    let steps = crate::check::rule_steps_at(&tree, &base, &tip);
    if steps.is_empty() {
        return Ok(Vec::new());
    }
    let mut said = Vec::new();
    let mut red: Vec<(crate::check::Step, String)> = Vec::new();
    for step in &steps {
        let log = store.state.join("review-rules").join(format!("{}-{}.log", t.id, step.name));
        match crate::check::run_at_review(&tree, step, &log) {
            crate::check::AtReview::Green => said.push(format!("{} green in the seat at {short} (`{}`)", step.name, step.command)),
            crate::check::AtReview::Skipped(why) => said.push(why),
            crate::check::AtReview::Red(tail) => red.push((step.clone(), tail)),
        }
    }
    if red.is_empty() {
        return Ok(said);
    }
    let names: Vec<&str> = red.iter().map(|(s, _)| s.name.as_str()).collect();
    if let Some(why) = anyway {
        note(format!("anyway: reviewed at {short} over a red rule step ({}): {}", names.join(", "), fold(why)));
        said.push(format!("{} red in the seat at {short}, put up over it: {}", names.join(", "), fold(why)));
        return Ok(said);
    }
    let tails: Vec<String> = red.iter().map(|(s, tail)| format!("`{}`: {}", s.command, fold(tail))).collect();
    note(format!("{RULE_RED} {tip} ({}) — {}", names.join(", "), util::truncate(&tails.join("; "), 600)));
    let mut out = vec![format!(
        "wsp: {} not moved to review — {} red in its seat at {short}, so no verifier is started for it",
        t.id,
        names.join(", ")
    )];
    for (step, tail) in &red {
        out.push(format!("  `{}`:", step.command));
        out.extend(tail.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("  | {l}")));
    }
    out.extend(said.iter().map(|l| format!("  {l}")));
    out.push("  fix what it found and commit (`wsp commit-help`), then `wsp submit` again".into());
    out.push(format!("  if the red is the tree's and not the row's: `wsp submit {} --anyway \"why\"`", t.id));
    Err(out)
}

/// Deliberately not yet — the other half of what `block` used to mean.
///
/// Demands a reason for the same procedural reason `block` does, and wants a
/// different kind of sentence: not the question somebody owes you, but the
/// condition that should bring the task back. Writing that down was already
/// the habit before there was a status for it — render-045 recorded its
/// three revisit conditions unprompted, and then nothing read them, because to
/// every list on the machine it was one more red row. It is a log line here
/// too, and the status is what makes the log line findable: `wsp ls -s parked`
/// is now a question you can ask.
pub fn park(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp park <id> \"what would bring it back\"   (or `-` to read it from stdin)";
    let (reason, from) = match prose_payload(args, USAGE) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let said = from.map(|src| format!("{} characters from {src}", reason.len()));
    let row = args.rest.first().and_then(|n| store.task_or_why(n).ok()).map(|t| t.id);
    let seats = row.as_deref().map(|r| crate::attention::seats_of_row(store, r)).unwrap_or_default();
    let code = mutate_saying(store, args, "parked", said.as_deref(), |t| {
        t.set_status(Status::Parked);
        t.log(&format!("parked: {reason}"));
    });
    // A parked row has nobody to be waiting on (`wsp-649`).
    if let (0, Some(row)) = (code, row.as_deref()) {
        crate::attention::row_let_go(store, row, &seats);
    }
    code
}

/// `wsp unpark <id>`: a parked row, and only a parked one, back to `todo`.
pub fn unpark(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first() else {
        eprintln!("usage: wsp unpark <id>");
        return 2;
    };
    match store.task_or_why(needle) {
        Ok(t) if t.status() == Status::Parked => set_status(store, args, Status::Todo),
        Ok(t) => {
            eprintln!("wsp: {} is at {}, not parked", t.id, t.status().as_str());
            1
        }
        Err(why) => {
            eprintln!("{why}");
            1
        }
    }
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
pub fn payload_source(payload: &[String]) -> Option<String> {
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

/// The prose for a log entry or a decision: the rest of the line, or the stream
/// it names. `Err` is the exit code, with the reason already on stderr.
fn prose_payload(args: &Args, usage: &str) -> Result<(String, Option<String>), i32> {
    let Some(src) = payload_source(args.rest.get(1..).unwrap_or_default()) else {
        let typed = args.text(1);
        // Before the fold, because folding collapses runs of whitespace and a
        // carriage return is whitespace: the evidence that this came off a
        // terminal is the first thing `fold` would destroy.
        if let Some(why) = util::terminal_output(&typed) {
            eprintln!("wsp: {why}");
            // Names the cause, because the shape of the mistake is not
            // visible in what arrived: the caller sees fluent prose.
            eprintln!("     a backtick inside double quotes runs a command. `-` reads the text from a stream, where a shell never sees it");
            return Err(2);
        }
        let text = fold(&typed);
        if text.is_empty() {
            eprintln!("usage: {usage}");
            return Err(2);
        }
        return Ok((text, None));
    };
    let (text, named) = prose_from_source(&src)?;
    Ok((text, Some(named)))
}

/// The stream half of [`prose_payload`], for the verbs whose payload does not
/// begin after a subject.
///
/// Split out for `say`, which has no id to skip and so cannot share the
/// positional arithmetic — but wants all three refusals here, and wants them
/// worded the same. The guards are the point of the split, not the reading: a
/// verb that grows a `-` form and not these grows the silent failure with it.
pub fn prose_from_source(src: &str) -> Result<(String, String), i32> {
    prose_read(src, fold)
}

/// [`prose_payload`] for a payload filed under a heading of its own rather than
/// appended to the log: a stream keeps its lines, because there a line break is
/// structure and not a new entry. Typed text is folded as it always is.
///
/// `wsp verified` is the reader. A verdict that blocks is a list of what has to
/// change, and folded into one line it is the paragraph wsp-174 left on its own
/// log — read by the member that has to act on it.
pub fn prose_kept(args: &Args, usage: &str) -> Result<String, i32> {
    match payload_source(args.rest.get(1..).unwrap_or_default()) {
        Some(src) => prose_read(&src, |raw| raw.trim().to_string()).map(|(t, _)| t),
        None => prose_payload(args, usage).map(|(t, _)| t),
    }
}

fn prose_read(src: &str, shape: fn(&str) -> String) -> Result<(String, String), i32> {
    // Named the way the rest of the CLI names a path — a receipt that says
    // `~/notes/survey.md` is one the reader recognises.
    let named = if src == "-" { "stdin".to_string() } else { util::contract(&util::expand(src)) };
    if src == "-" && util::stdin_is_tty() {
        // Reading a terminal is not an empty note, it is a command that stops
        // and says nothing while it swallows the keys. The one failure worse
        // than the silent one this task is about.
        eprintln!("wsp: nothing is piped in — `-` reads the text from a stream");
        return Err(2);
    }
    let text = match read_source(src) {
        Ok(raw) => shape(&raw),
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
    Ok((text, named))
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
    // A piece id writes to the row's log, prefixed `piece 2:` (`docs/pieces.md`,
    // *Seen and told*).
    let (row, prefix) = match args.rest.first().map(|n| crate::pieces::addressed(store, n)) {
        Some(Ok(Some(a))) => (Some(a.task.id.clone()), format!("piece {}: ", a.n)),
        Some(Err(why)) => {
            eprintln!("wsp: {why}");
            return 1;
        }
        _ => (None, String::new()),
    };
    mutate_saying_at(store, args, "note", said.as_deref(), row, |t| t.log(&crate::cmd_whoami::sign(format!("{prefix}{text}"))))
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

    if needle.starts_with("init-") {
        if let Some(code) = crate::cmd_initiative::decide(store, &needle, &text, &supersedes, args.json()) {
            return code;
        }
    }

    if let Some(t) = store.find_task(&needle) {
        if let Err(code) = check_supersedes(&t.body, &supersedes, &t.id) {
            return code;
        }
        let said = from.map(|src| format!("{} characters from {src}", text.len()));
        let blocked = t.status() == Status::Blocked;
        let code = mutate_saying(store, args, "decided", said.as_deref(), |t| {
            crate::decisions::decide_task(t, &text, &supersedes);
        });
        // A decision made on a blocked row is its answer (`wsp-635`): the run
        // reopens the member, so it is asked now and not at the next tick.
        if code == 0 && blocked {
            crate::cycle::poke_task(store, &t.id, "decided");
        }
        return code;
    }

    let index = Index::new(store.projects());
    let Some(mut p) = index.find(&needle).cloned() else {
        eprintln!("wsp: no task or project matching `{needle}`");
        return 1;
    };
    if let Err(code) = check_supersedes(&p.body, &supersedes, &p.id) {
        return code;
    }
    crate::decisions::decide_project(&mut p, &text, &supersedes);
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
                eprintln!("{}", util::no_such("project", &p));
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
    //
    // A shared row's new id is the shared side's to give, as it is when one is
    // filed straight into the project (`new_row_id`, the hub's counter, or a
    // provisional local id when the hub cannot be asked); a private row's is
    // the local counter's. Taken from the local counter, a shared row's id is
    // one another person filing into the same project may be given too.
    let mut renumber: std::collections::BTreeMap<String, String> = Default::default();
    if let Some(into) = &target {
        let mut filing: Vec<&Task> = Vec::new();
        if is_inbox_id(&subject.id) {
            filing.push(&subject);
        }
        for id in crate::resolve::descendants_of(&tasks, &subject.id) {
            if let Some(kid) = tasks.iter().find(|t| t.id == id).filter(|t| is_inbox_id(&t.id)) {
                filing.push(kid);
            }
        }
        for old in filing {
            // The parent it will have: a provisional parent makes it provisional.
            let parent = match (&new_parent, old.id == subject.id) {
                (Some(p), true) => p.as_ref().map(|p| p.id.clone()),
                _ => old.parent.clone(),
            };
            match new_row_id(store, Some(into), &old.workspace, parent.as_deref()) {
                Ok(new) => {
                    renumber.insert(old.id.clone(), new);
                }
                Err(e) => {
                    // The ids already reserved go back: nothing was written yet.
                    for new in renumber.values() {
                        let path = store.task_path(new);
                        if std::fs::metadata(&path).is_ok_and(|m| m.len() == 0) {
                            let _ = std::fs::remove_file(path);
                        }
                    }
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
            // What names the row outside the store: a list's retro record and
            // the row's own outputs, both under the artefacts root (`wsp-351`).
            let mut code = 0;
            match crate::retro_yours::follow_renames(store, &renumber) {
                Ok(lists) => {
                    for l in lists {
                        println!("  {}", p.dim(&format!("{l}'s retro record follows")));
                    }
                }
                Err(e) => {
                    eprintln!("wsp: a retro record still names the old id: {e}");
                    code = 1;
                }
            }
            if let Err(e) = crate::artefact::follow_renames(&crate::artefact::root(), &renumber) {
                eprintln!("wsp: {e}");
                code = 1;
            }
            code
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

/// `wsp ref <id> +PATH -PATH` — the files this row names outside its own tree.
///
/// **Add-only was the whole of it until `core-042`, and that is the same defect
/// [`prio`] was written for one field over.** `wsp add --ref PATH` set this at
/// the one moment you know least about the work, and nothing could move it
/// afterwards. That mattered little while `refs` was a note to a reader; it
/// stopped being little the moment [`crate::cmd_spawn::reach`] made it the
/// thing that decides where a spawned agent may reach. `worklist-surface` is
/// the case: five rows all needing one spec outside every worktree, the path
/// written in a parent's prose, every row already created — and no way to say
/// so on any of them. A mechanism that only works on rows nobody has filed yet
/// is not a mechanism.
///
/// `+`/`-` rather than a replacing list, because that is [`tag`]'s spelling for
/// the same shape and a caller should not have to learn a second one. The path
/// is stored the way `add` stores it — [`crate::util::contract`], so `~` comes
/// back — and removal matches on the contracted form, which is what `wsp show`
/// prints and therefore what a caller has in front of them.
///
/// A removal naming a path the row does not carry says so and exits 1, for
/// [`tag`]'s reason: a ref that stays on a task after a command reported
/// success is a reach nobody can see the reason for.
pub fn reference(store: &Store, args: &Args) -> i32 {
    let changes: Vec<String> = args.rest.iter().skip(1).cloned().collect();
    if changes.is_empty() {
        eprintln!("usage: wsp ref <id> +PATH -PATH");
        return 2;
    }
    let tidy = |p: &str| util::contract(&util::expand(p));
    let mut absent: Vec<String> = Vec::new();
    let code = mutate(store, args, "refs", |t| {
        for c in &changes {
            // A leading `-` is the removal syntax, and a path may not begin
            // with one — `-` is not a path component anywhere this store
            // points — so there is nothing to disambiguate against.
            if let Some(rm) = c.strip_prefix('-') {
                let at = tidy(rm);
                if !t.refs.iter().any(|x| *x == at) {
                    absent.push(rm.to_string());
                }
                t.refs.retain(|x| *x != at);
                continue;
            }
            let at = tidy(c.strip_prefix('+').unwrap_or(c));
            if !at.is_empty() && !t.refs.iter().any(|x| *x == at) {
                t.refs.push(at);
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
        absent = crate::tags::adjust(t, &changes);
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
/// hand — and while it read the store and asked backend inline, checking the
/// answer meant standing up two agents, binding one of them, and asking the
/// other. Three idle agents set going at once and all handed the same task is
/// the failure this filter exists to stop; there was nowhere to write it down.
pub struct Backlog {
    pub tasks: Vec<Task>,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    /// This pane. A `doing` task in the caller's own hand is precisely the
    /// caller's next piece of work, so it is not "taken".
    pub me: Option<String>,
}

impl Backlog {
    pub fn live(store: &Store) -> Backlog {
        Backlog {
            // Choosing reads front matter alone — status, project, who holds
            // it — so it is read from the index (`wsp-320`); the one task
            // chosen is read in full before it is printed.
            tasks: store.task_heads().iter().map(|h| h.bodiless()).collect(),
            bindings: store.bindings(),
            me: None,
        }
    }
}

/// What `next` found, and — when it found nothing — what the backlog was doing
/// instead. "Nothing actionable" on its own reads as an empty backlog, and the
/// commonest reason for landing there is the opposite.
pub struct Pick {
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
pub fn pick(b: &Backlog, scope_ids: Option<&[String]>) -> Pick {
    let in_scope = |t: &Task| match scope_ids {
        Some(ids) => t.project.as_ref().map(|p| ids.contains(p)).unwrap_or(false),
        None => true,
    };

    // Nothing another live agent is already holding. `claim` refuses those, so
    // naming one sends the caller into a guaranteed refusal — and three idle
    // agents set going at once would otherwise all be handed the same task and
    // all three bounce.
    let taken = |t: &Task| {
        !crate::claims::live_holders(&b.bindings, &[], &t.id, b.me.as_deref()).is_empty()
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
pub fn nothing_because(p: &Pick) -> String {
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
    let scope = match Seats::current_project(crate::cycle::seats(), store, args, &index) {
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
        crate::stage::say_looking(store, scope.as_deref(), false);

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
    crate::stage::say_looking(store, scope.as_deref(), true);

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
    // Same reading as `add`: one line, no stream form, and the one check that
    // costs nothing.
    if let Some(why) = util::terminal_output(&title) {
        eprintln!("wsp: {why}");
        return 2;
    }
    mutate(store, args, "rename", |t| {
        t.log(&format!("renamed from \"{}\"", t.title));
        t.title = title.trim().to_string();
    })
}

/// `wsp caption <id|slug> "…"`: the line a glance reads, on a task or a
/// worklist. `--clear` takes it off, and the derived one is read again.
///
/// Logged like a rename, with what it replaced, because a caption is what a
/// person reads instead of the title and changing it is changing what the row
/// is called. Both the task and the worklist answer to this one verb rather
/// than `worklist caption` beside it: the spec names one verb, and the two key
/// spaces only meet on an exact slug, which is asked first. A group's caption
/// is `wsp worklist group <slug> N --caption`, since a group has no name of
/// its own to give here.
pub fn caption(store: &Store, args: &Args) -> i32 {
    const USAGE: &str =
        "wsp caption <id|slug> \"one line, under 60\"   (or `-` to read it from stdin; --clear takes it off)";
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let text = if args.has("clear") {
        if args.rest.len() > 1 {
            eprintln!("wsp: --clear takes the caption off, so there is no caption to give with it");
            return 2;
        }
        None
    } else {
        let (raw, _) = match prose_payload(args, USAGE) {
            Ok(v) => v,
            Err(code) => return code,
        };
        match crate::caption::check(&raw) {
            Ok((text, note)) => {
                if let Some(note) = note {
                    eprintln!("  {}", Paint::new().dim(&note));
                }
                Some(text)
            }
            Err(why) => {
                eprintln!("wsp: {why}");
                return 2;
            }
        }
    };
    let entry = |old: Option<&str>| -> String {
        let was = old.map(|o| format!(" (was \"{o}\")")).unwrap_or_default();
        match &text {
            Some(t) => format!("captioned \"{t}\"{was}"),
            None => format!("caption cleared{was}"),
        }
    };

    if let Some(mut w) = store.worklist(&needle) {
        let line = entry(w.caption.as_deref());
        w.caption = text.clone();
        w.log(&line);
        if let Err(e) = store.save_worklist(&w) {
            eprintln!("wsp: write failed: {e}");
            return 1;
        }
        let c = crate::caption::of_worklist(&w);
        store.log_event("worklist-captioned", json!({ "id": w.id, "caption": c.text }));
        store.git_commit(&format!("wsp: caption {} — {}", w.id, c.text));
        if args.json() {
            println!("{}", json!({ "id": w.id, "title": w.title, "caption": c.json() }));
        } else {
            let p = Paint::new();
            println!("{} {}  {}", p.dim("·"), p.bold(&w.id), c.text);
            if !c.authored() {
                println!("  {}", p.dim("no caption: a glance reads its title's first clause"));
            }
        }
        return 0;
    }

    let said = match &text {
        Some(t) => format!("caption  {t}"),
        None => match store.task_or_why(&needle) {
            Ok(t) => format!("no caption: a glance reads \"{}\"", crate::caption::title(&t.title)),
            Err(why) => {
                eprintln!("{why}");
                return 1;
            }
        },
    };
    mutate_saying(store, args, "caption", Some(&said), |t| {
        t.log(&entry(t.caption.as_deref()));
        t.caption = text.clone();
    })
}

/// E-1 (Ed, 2026-10-09, compound-405): `wsp review` refuses a row with no
/// caption, printing the derived line so taking it is one paste.
///
/// Review is the moment a row starts wanting a person, which is when a glance
/// happens; and optional everywhere, the 13 of gpui-migration's 44 whose
/// derived line is still over 60 would stay that way. The derived line is
/// printed as the command that takes it, quoted for a shell, unless it is
/// over the limit — then pasting it would only be refused a second time, so
/// the refusal says to write one instead. Before the account is read, so a
/// stream given to `review` is not spent on a refusal.
fn uncaptioned(t: &Task) -> Option<Vec<String>> {
    if t.caption.as_deref().is_some_and(|c| !c.trim().is_empty()) {
        return None;
    }
    let derived = crate::caption::title(&t.title);
    let n = derived.chars().count();
    let mut out = vec![format!(
        "wsp: {} has no caption — the line a person reads once the row wants one, so review asks for it",
        t.id
    )];
    if n <= crate::caption::LIMIT {
        out.push(format!("     its title gives: {derived}"));
        out.push(format!("     wsp caption {} {}", t.id, util::shell_quote(&derived)));
        out.push("     then review again — or caption it in your own words, under 60".into());
    } else {
        out.push(format!("     its title gives {n} characters, past the {}: {derived}", crate::caption::LIMIT));
        out.push(format!("     wsp caption {} \"…\"   in your own words, then review again", t.id));
    }
    Some(out)
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
    // A row the shared side holds for others too: what removing it does there
    // is said first, and done only under `--force`.
    let removal = crate::addon::get().removing(store, &t.id);
    if let (Some(r), false) = (&removal, args.has("force")) {
        eprintln!("wsp: {} — pass --force to remove it", r.warning);
        return 1;
    }
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
        let mut doc = json!({ "removed": t.id, "archived": true, "filed_as": filed });
        if let Some(r) = &removal {
            doc["shared"] = json!(r.done);
        }
        println!("{doc}");
    } else {
        if filed == t.id {
            println!("removed {} (archived)", t.id);
        } else {
            // Say it rather than let it pass: the archive already held that id,
            // which means two tasks have worn it.
            println!("removed {} (archived as {filed} — that id was already in the archive)", t.id);
        }
        if let Some(r) = &removal {
            println!("{}", r.done);
        }
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
pub fn prose_source(args: &Args) -> Option<String> {
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
        "worklist" => match store.worklist(&item.id) {
            Some(w) => w.body,
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
        crate::log::append_to_body(&mut body, "decisions edited by hand");
    }

    let refused = overview_names_refused(item.what, &item.id, one, &body);

    let saved = match item.what {
        "project" => store.project(&item.id).map(|mut p| {
            p.body = body;
            store.save_project(&p)
        }),
        // A worklist's `## Groups` is never in `sections`, so nothing that
        // arrives here can rewrite the queue: this reaches the prose around it
        // and the verbs keep the structure. `## Overview` is the one that
        // matters — it is the start condition for group 1, there being no
        // barrier in front of it to carry one.
        "worklist" => store.worklist(&item.id).map(|mut w| {
            w.body = body;
            store.save_worklist(&w)
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

    // Said, not refused: the author may be writing it for a person to run.
    for why in &refused {
        eprintln!("wsp: {why}");
    }
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

/// What an edit that wrote `body` says about the verbs its overview tells a
/// seat to run that the seat is refused (`wsp-312`): an overview is the order a
/// seat reads, so it is said while its author is here. Only when the edit could
/// reach `## Overview`. A worklist's overview is read by no one row's seat, and
/// a project's is no order.
fn overview_names_refused(what: &str, id: &str, one: Option<&str>, body: &str) -> Vec<String> {
    if what == "project" || one.is_some_and(|sec| sec != "Overview") {
        return Vec::new();
    }
    let own = (what != "worklist").then_some(id);
    crate::split::warn_order(&crate::model::section_of(body, "Overview").unwrap_or_default(), own)
}

fn launch_editor(path: &std::path::Path) -> i32 {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    match wsp_base::calls::Command::new(&editor).arg(path).status() {
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

/// Sweep finished tasks out of `tasks/` and into `archive/tasks/<month>/`.
///
/// **`-n` is the whole point of this verb having a shape at all.** Every other
/// sweeping verb in wsp has one — `checkout --sweep -n`, `migrate -n`,
/// `install -n` — and `archive` was the sweep without it. Worse than absent:
/// `-n` expands to `--dry-run` for every verb in the parser, so
/// `wsp archive --all -n` — the careful invocation, the one an agent types
/// precisely because it does not know what is about to move — parsed, archived
/// 266 tasks, and *then* said the word had gone nowhere. `unknown_flags` is
/// honest and it is after the fact, which is all it can be; the repair is for
/// the verb to read the flag.
///
/// **The cutoff-remover is a number, not `--all`.** It was `--all`, which is
/// the most-typed word in the CLI and everywhere else in wsp widens what is
/// *looked at* — `ls --all`, `find --all`, `attempts --all`. Here it widened
/// what was *moved*, and it was the only thing standing between two words and
/// the whole store. `--days 0` already meant exactly this and still does; what
/// changed is that the guard now comes off by a number you chose rather than by
/// muscle memory.
///
/// The word is refused here rather than left to `unknown_flags`, for two
/// reasons that are the same reason. `unknown_flags` runs *after* the verb —
/// it is a read tally, and a tally is only complete once the command has
/// finished asking — so leaving `--all` to it means the thirty-day sweep runs
/// and commits before anyone is told the word went nowhere. And its suggestion
/// was `--full`, the nearest name on this verb's help within two edits, which
/// widens the *listing* and would send somebody reaching for "sweep
/// everything" to a flag that sweeps nothing extra. A retired flag is the one
/// case where the verb knows what was meant, so the verb says it.
///
/// Oldest first, because that is what a cutoff is about, and because the tail
/// the cap drops is then the newest — the least likely to be the record
/// somebody is citing. The count above the list is the whole answer either way,
/// which is what makes a cut list honest.
pub fn archive(store: &Store, args: &Args) -> i32 {
    if args.has("all") {
        eprintln!("wsp: `wsp archive --all` is now `--days 0`");
        eprintln!("     It removed the thirty-day cutoff, which is the only thing between");
        eprintln!("     this verb and the whole store — so it is a number now rather than");
        eprintln!("     the most-typed word in the CLI. `wsp archive --days 0 -n` first.");
        return 2;
    }
    let cutoff: i64 = args.get("days").and_then(|d| d.parse().ok()).unwrap_or(30);
    let dry = args.has("dry-run");
    // Read whether or not it is a dry run, so the flag is always in the tally
    // `unknown_flags` reads and `wsp archive --full` is never refused on the
    // strength of which branch this run happened to take.
    let full = args.has("full");

    let mut due: Vec<Task> = store
        .tasks()
        .into_iter()
        .filter(|t| t.status() == Status::Done && util::age_days(&t.updated) >= cutoff)
        .collect();
    due.sort_by(|a, b| a.updated.cmp(&b.updated).then_with(|| a.id.cmp(&b.id)));

    let moved: Vec<Task> = if dry {
        due
    } else {
        let filed: std::collections::HashSet<String> = store.archive_tasks(&due).into_iter().collect();
        due.into_iter().filter(|t| filed.contains(&t.id)).collect()
    };
    if !dry && !moved.is_empty() {
        store.git_commit(&format!("wsp: archive {} tasks", moved.len()));
    }

    if args.json() {
        // Uncapped: a caller parsing this is not reading it.
        println!(
            "{}",
            json!({
                "archived": moved.len(),
                "dry_run": dry,
                "tasks": moved.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            })
        );
        return 0;
    }

    let p = Paint::new();
    println!(
        "{} {} task(s)",
        if dry { "would archive" } else { "archived" },
        moved.len()
    );
    if !dry {
        return 0;
    }
    let shown = if full { moved.len() } else { moved.len().min(ARCHIVE_MAX) };
    let idw = moved[..shown].iter().map(|t| t.id.chars().count()).max().unwrap_or(12);
    for t in &moved[..shown] {
        println!(
            "  {} {} {}",
            p.dim(&util::pad(&t.id, idw)),
            p.dim(&archive_month(t)),
            util::truncate(&t.title, 62)
        );
    }
    if shown < moved.len() {
        println!(
            "{}",
            p.dim(&format!("  … {} more · --full lists them all", moved.len() - shown))
        );
    }
    0
}

/// The month directory `archive_task` would file this under — `updated`'s
/// year and month, which is also the only thing the reader needs to find the
/// file again. Kept beside the sweep rather than taken from `archive_task`,
/// which does not compute it until it is moving something.
fn archive_month(t: &Task) -> String {
    match t.updated.len() >= 7 {
        true => t.updated[0..7].to_string(),
        false => "unknown".into(),
    }
}

/// Rows a dry run prints before it says how many it is not printing. The same
/// twenty `find` stops at, for the same reason: past that the list is telling
/// you to look at the count instead.
const ARCHIVE_MAX: usize = 20;

/// `ls --json`'s rows: each task's record, its stage and a staged row's legs,
/// printed by `wsp-view` over the `State` the files make (wsp-308) — the
/// mirror's own projections, so web-chrome's fold of the daemon's `State`
/// prints these bytes too (`wsp_view::task::ls_all`). Each row's stage is the
/// mirror's half (`stage`, `stage_steps`, `stage_live`) with its seats' over it.
pub fn rows_json(store: &Store, tasks: &[Task]) -> String {
    // Rows, claims and seats: no list's facts, machine or ask (wsp-766).
    let state = crate::mirror::rows_state(store, crate::mirror::LOCAL);
    let picked: Vec<&wsp_store_contract::Row> =
        tasks.iter().filter_map(|t| state.rows.get(&wsp_store_contract::RowId(t.id.clone()))).collect();
    wsp_view::task::ls_rows(&state, &picked, util::epoch_secs())
}

/// How long `ls --json --all` waits for the API's hello before it reads the
/// files, as `wip` does (`docs/state-core.md` 4.6 (c)).
const LS_DIAL: std::time::Duration = std::time::Duration::from_millis(200);

/// How long each of its two reads may take once the API has answered: a full
/// store's heads are about 3 MB.
const LS_READ: std::time::Duration = std::time::Duration::from_secs(2);

/// `ls --json --all` off the API (wsp-374): the rows' heads and the daemon's
/// `tasks` reading, drawn by `wsp_view::task::ls_of_reading`, with no task file
/// opened and no child started. `None` reads the files: under `WSP_READ=files`;
/// for any other `ls` (a project, `-s` or `-t`, or open rows alone); where no
/// daemon has written an `api.json` (said nothing, as that is the files' own
/// case); and, said once on stderr, where the dial is silent or fails within
/// [`LS_DIAL`], a read fails, the daemon has no `tasks` reading yet, or the
/// reading does not tell of the rows the heads do.
fn ls_all_api(store: &Store, args: &Args) -> Option<String> {
    let filtered = ["project", "status", "tag"].iter().any(|k| args.has(k));
    if !args.has("all") || filtered || std::env::var("WSP_READ").is_ok_and(|v| v.trim() == "files") {
        return None;
    }
    if !store.state.join(wsp_api::LISTEN_FILE).exists() {
        return None;
    }
    let read = || -> Result<String, String> {
        let by = wsp_store_contract::Principal::Wsp("ls".into());
        let mut c = wsp_client::Client::dial(&store.state, by, LS_DIAL).map_err(|e| e.to_string())?;
        c.set_timeout(LS_READ);
        let heads = c.rows(wsp_store_contract::RowFilter { heads: true, ..Default::default() }).map_err(|e| format!("`rows`: {e}"))?;
        let readings = c.readings(&[wsp_view::reading::TASKS]).map_err(|e| format!("`readings`: {e}"))?;
        let reading = readings.get(wsp_view::reading::TASKS).ok_or("the daemon has no `tasks` reading yet")?;
        let tasks: wsp_view::reading::Tasks =
            wsp_view::reading::of(&reading.json).ok_or("the `tasks` reading does not read as its shape")?;
        wsp_view::task::ls_of_reading(&heads, &tasks).ok_or_else(|| "the `tasks` reading trails the rows".to_string())
    };
    read().map_err(|why| eprintln!("wsp: ls reads the files: {why}")).ok()
}

/// What `ls --json` prints of `tasks` beside each record, as `wsp-view` takes
/// it (wsp-743: the daemon's `tasks` reading is this over `ls --all`'s tasks).
fn ls_reading(store: &Store, tasks: &[Task]) -> wsp_view::reading::Tasks {
    // `wsp-242`: each row's stage, every source read once for all of them.
    let stages = crate::stage::with_history(store, &tasks.iter().collect::<Vec<_>>());
    wsp_view::reading::Tasks {
        order: tasks.iter().map(|t| t.id.clone()).collect(),
        // `wsp-274`: and since when, off the same timeline `show` gives.
        stages: stages.iter().map(|(id, (s, h))| (id.clone(), crate::stage::view(s, h))).collect(),
        // The objects `show --json` has, and absent on an unstaged row.
        legs: tasks
            .iter()
            .filter(|t| t.body.contains(&format!("## {}", crate::legs::SECTION)) && !crate::legs::legs(t).is_empty())
            .map(|t| (t.id.clone(), crate::legs::view(t)))
            .collect(),
    }
}

/// The `tasks` reading (wsp-743): `ls --json --all`'s order, stages and legs,
/// over the tasks it lists (every live task, by status, priority and id).
pub fn tasks_reading(store: &Store) -> wsp_view::reading::Tasks {
    ls_reading(store, &ls_all(store))
}

/// The tasks `ls --json --all` lists, in its order, with no project in scope.
pub fn ls_all(store: &Store) -> Vec<Task> {
    let index = Index::new(store.projects());
    let args = Args::synth("ls", &[], &[("all", ""), ("json", "")]);
    filtered(store, &args, &index, None)
}

/// The contract row a task is, as the mirror projects it, for `wsp-view`.
fn row_of(t: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> wsp_store_contract::Row {
    crate::mirror::rows(std::slice::from_ref(t), claims, &[], &Default::default(), &Default::default(), &Default::default())
        .into_iter()
        .find_map(|e| match e {
            wsp_store_contract::Entity::Row(r) => Some(r),
            _ => None,
        })
        .expect("a task projects to one row")
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["add", "new"],
        run: add,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 10,
                text: r##"  wsp add "title" [-p proj] [-t tag]… [--prio high] [--ref PATH]"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 20,
                text: r##"  wsp add "title" --parent <id>     a sub-task, filed where its parent is; refused from
                                    a seat working that member, which stages legs instead
                                    (`--anyway` files it, unrun)"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 25,
                text: r##"  wsp add "title" --from <id>       a follow-up: work found while doing <id>, filed where
                                    <id> is; it records where it came from and never holds
                                    <id> open (a sub-task is --parent, the row's own work)"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 30,
                text: r##"  wsp add "title" --caption "…"     …with the line a glance reads, under 60"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 40,
                text: r##"  wsp add "title" --after <id>[,<id>…]   …that rests on those, as `wsp after`"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 45,
                text: r##"  wsp add "title" --workspace shared:<name>   …filed in a shared workspace (needs a login;
                                    `$WSP_WORKSPACE` is the default); private unless said,
                                    and a sub-task is where its parent is"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["ls", "list"],
        run: list,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 50,
                text: r##"  wsp ls [-p proj] [-t tag] [-s status] [--all]"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["find", "search"],
        run: find,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 60,
                text: r##"  wsp find <text> [-p proj] [--all] [--full]
                                    every task the words are in — the title or
                                    the prose; the project you are in and open
                                    work only, unless --all, which also reaches
                                    the archive. It says when the answer is
                                    somewhere you did not look. Stops at 20
                                    hits and says how many more; --full for
                                    all of them"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["inbox"],
        run: inbox,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 70,
                text: r##"  wsp inbox                         tasks with no project"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["show", "cat"],
        run: show,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 80,
                text: r##"  wsp show <id> [--log]             full task, including notes. The log is
                                    the last few entries and says how many
                                    earlier ones it did not print; --log for
                                    all of them; --pieces prints each piece
                                    whole"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["decide"],
        run: decide,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 230,
                text: r##"  wsp decide <task|proj> "…"      record what was settled, and why"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 240,
                text: r##"  wsp decide <t|p> "…" --supersedes d1   …and which earlier one it replaces"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["note"],
        run: note,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 250,
                text: r##"  wsp note <id> "text"              append to the log"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["start", "doing"],
        run: |store, args| set_status(store, args, crate::model::Status::Doing),
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 90,
                text: r##"  wsp start|review <id>           move through the workflow"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["block"],
        run: block,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 210,
                text: r##"  wsp block <id> "reason"           stop it: somebody owes you an answer"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 260,
                text: r##"  wsp block|park|decide|note <id> - | --from FILE
                                    …and `wsp say` the same way, with no id
                                    …or from stdin, or a file. A paragraph
                                    typed between double quotes is rewritten by
                                    the shell — every backtick in it runs a
                                    command — and `-` is the path that never
                                    meets one. For the log, one entry is one
                                    line, so what arrives on several is folded"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["park", "pause"],
        run: park,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 220,
                text: r##"  wsp park <id> "reason"            not yet, deliberately — say what brings
                                    it back. Open work, sorted last and drawn
                                    quiet, and not counted as wanting you"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["unpark"],
        run: unpark,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 221,
                text: r##"  wsp unpark <id>                   a parked row, back to todo"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["review"],
        run: review,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 99,
                text: r##"  wsp review --auto-close off|<hours> -p <proj>
                                a row at review closes itself (`done`) once
                                landed, verified, owed nothing and quiet that
                                long (24 by default); `wsp review <id> "…"
                                --hold-review` keeps one for the person"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["reopen"],
        run: reopen,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 100,
                text: r##"  wsp reopen|todo <id> "what is owed" [--no-seat]
                                send work back: moves the row, tells
                                its pane, and stops the run reading it as
                                finished (`todo` sets the status and takes no prose).
                                A row whose agent is gone is seated again:
                                by its run, or with the kind and tier it ran
                                with; --no-seat leaves it to you"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["todo"],
        run: |store, args| set_status(store, args, crate::model::Status::Todo),
        help: &[],
    },
    crate::verbs::Verb {
        names: &["mv", "move"],
        run: mv,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 310,
                text: r##"  wsp mv <id> -p proj               reassign, sub-tree and all"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 320,
                text: r##"  wsp mv <id> --parent <id>|none    re-parent it, or detach it"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["tag"],
        run: tag,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 340,
                text: r##"  wsp tag <id> +dsp -ui             adjust tags"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["ref"],
        run: reference,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 350,
                text: r##"  wsp ref <id> +PATH -PATH          the files this row names outside its own
                                    tree. A spawned agent may reach them
                                    without asking, and nothing above its tree
                                    is reachable however it is written"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["prio", "priority"],
        run: prio,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 360,
                text: r##"  wsp prio <id> high|normal|low     what comes first inside its project"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["next"],
        run: next,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 370,
                text: r##"  wsp next [-p proj]                highest-priority actionable task"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["edit"],
        run: edit,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 270,
                text: r##"  wsp edit <id> [--overview|--details|--decisions]  prose, in $EDITOR"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 280,
                text: r##"  wsp edit <id> --overview --from F|-    …or from a file, or stdin"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["rename"],
        run: rename,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 290,
                text: r##"  wsp rename <id> "title"           retitle it; the old title goes in the log"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["caption"],
        run: caption,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 300,
                text: r##"  wsp caption <id|slug> "…"|-|--clear   the line a glance reads, refused past
                                    60; with none, the title's first clause.
                                    `wsp review` asks for one"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["rm", "remove"],
        run: rm,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 380,
                text: r##"  wsp rm <id> [--force]             retire it to the archive; a shared row is
                                    retired on the hub for everyone too, for good,
                                    which --force confirms"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["archive"],
        run: archive,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 390,
                text: r##"  wsp archive [--days N] [-n] [--full]
                                    sweep done tasks older than 30d into
                                    archive/tasks/<month>/; --days 0 for every
                                    finished task there is, -n for which ones
                                    first and --full for all of that list"##,
            },
        ],
    },
];

/// What the tree of `task` still holds, said with the engine's two answers: whether a
/// run's verifier lands the row, and the commit its newest ready line names.
fn outstanding_for(store: &Store, task: &str) -> Vec<String> {
    let by_a_run = crate::worklist::verified_by_a_run(store, task);
    let ready = store.find_task(task).and_then(|t| crate::repair::ready(&t));
    crate::cmd_checkout::outstanding(store, task, by_a_run, ready)
}

/// `claim expires <when>`, under the claim line (wsp-410): the record's own
/// `expires` and how far off it is. `None` for a claim with none, which never
/// expires and says nothing.
fn claim_expires_line(claim: &serde_json::Value, now: i64) -> Option<String> {
    let at = wsp_store::store::record_expires(claim)?;
    let when = util::iso_at(at);
    Some(match at - now {
        left if left > 0 => format!("claim expires {when} (in {})", util::duration_human(left)),
        left => format!("claim expires {when} (lapsed {} ago)", util::duration_human(-left)),
    })
}

#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(not(test), allow(unused_imports, dead_code))]
pub mod tests {
    use super::*;

    thread_local! {
        /// What [`super::respawn`] would have run, instead of a real spawn.
        pub static RESEATED: std::cell::RefCell<Vec<Vec<String>>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    pub fn reseated() -> Vec<Vec<String>> {
        RESEATED.with(|r| r.borrow_mut().drain(..).collect())
    }

    /// compound-581's shape: claimed at a tier, landed, reviewed, released.
    pub fn released(store: &Store, id: &str, claim: &str) {
        let mut t = Task::new(id, id);
        t.set_status(Status::Review);
        t.log(claim);
        crate::log::note(store, &mut t, "released after 28m · ran sonnet-5-5/high · 93 turns").unwrap();
    }

    /// wsp-410: `wsp show` says when a claim expires, and says nothing for one
    /// that never does.
    #[test]
    fn show_says_when_a_claim_expires() {
        let now = util::epoch_of("2026-11-23T10:00:00Z");
        let c = serde_json::json!({ "expires": "2026-11-30T10:00:00Z" });
        let said = claim_expires_line(&c, now).unwrap();
        assert!(said.starts_with("claim expires 2026-11-30T10:00:00Z (in "), "{said}");
        let late = claim_expires_line(&c, now + 8 * 86_400).unwrap();
        assert!(late.contains("lapsed"), "{late}");
        assert_eq!(claim_expires_line(&serde_json::json!({ "workspace": "w" }), now), None);
    }

    pub fn scratch(tag: &str) -> Store {
        crate::testkit::store(tag)
    }

    /// `wsp-274`, at the verb compound's flight view reads (§24, FL-1): a
    /// row on no list carries `stage.since`, when it entered its word — its
    /// claim — beside `at`, the record's time, which is later.
    #[test]
    fn ls_json_gives_each_row_since_when_it_has_been_at_its_stage() {
        let store = scratch("ls-since");
        let mut t = Task::new("A row on no list", "s-1");
        t.status_raw = "doing".into();
        t.created = "2026-10-08T07:00:00Z".into();
        t.updated = "2026-10-08T09:20:00Z".into();
        crate::model::set_section_in(&mut t.body, "Log", "- 2026-10-08T08:00:00Z claimed by pane w1:p1\n- 2026-10-08T08:00:00Z → doing\n");
        store.save_task(&t).unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_str(&rows_json(&store, &store.tasks())).unwrap();
        let stage = &rows[0]["stage"];
        assert_eq!((&stage["word"], &stage["since"]), (&json!("started"), &json!("2026-10-08T08:00:00Z")), "{rows:?}");
        assert_ne!(stage["at"], stage["since"], "at stays the record's time");
    }

    /// A shared-work add-on that knows who holds `s-1` (bo on box-b) and that
    /// `s-2` is allocated to ann, who is the user here.
    struct Holds;

    impl crate::addon::SharedWork for Holds {
        fn user(&self) -> Option<String> {
            Some("ann".into())
        }
        fn row_facts(&self, _: &Store) -> std::collections::BTreeMap<String, crate::addon::RowFacts> {
            let held = crate::addon::RowFacts { held_by: Some("bo".into()), held_on: Some("box-b".into()), held_until: Some(1_900_000_000), ..Default::default() };
            [("s-1".to_string(), held)].into()
        }
    }

    /// `ls --json`'s rows carry the shared-row facts the feed's rows do (both
    /// are `mirror::row_half`'s, `docs/multiplayer.md` "What a desktop app
    /// reads"): `workspace` on every shared row and `for` where allocated,
    /// `mine` and `head_only` (`"true"`/`"false"`) on every shared row,
    /// `held_by`, `held_on` and `held_until` where known, and a private row none.
    #[test]
    fn ls_json_rows_carry_the_shared_row_facts_the_feed_rows_do() {
        let _iso = crate::util::isolated("ls-facts");
        let store = scratch("ls-facts");
        for (id, ws, for_user) in [("s-1", "shared:team", None), ("s-2", "shared:team", Some("ann")), ("s-3", "", None)] {
            let mut t = Task::new(id, id);
            if !ws.is_empty() {
                t.workspace = ws.parse().unwrap();
            }
            t.for_user = for_user.map(str::to_string);
            store.save_task(&t).unwrap();
        }
        let _a = crate::addon::scoped(std::sync::Arc::new(Holds));
        let rows: Vec<serde_json::Value> = serde_json::from_str(&rows_json(&store, &store.tasks())).unwrap();
        let row = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap().clone();
        let facts = |id: &str| ["mine", "head_only", "held_by", "held_on", "held_until"].map(|k| row(id).get(k).cloned());
        let s = |v: &str| Some(json!(v));
        assert_eq!(facts("s-1"), [s("false"), s("false"), s("bo"), s("box-b"), s(&crate::util::iso_at(1_900_000_000))], "{}", row("s-1"));
        assert_eq!(facts("s-2"), [s("true"), s("false"), None, None, None], "allocated to ann: {}", row("s-2"));
        assert_eq!(facts("s-3"), [None, None, None, None, None], "a private row: {}", row("s-3"));
        // And the row's sharing itself, as the feed's row has it: its workspace and, where allocated, `for`.
        let sharing = |id: &str| ["workspace", "for"].map(|k| row(id).get(k).cloned());
        assert_eq!(sharing("s-1"), [s("shared:team"), None], "{}", row("s-1"));
        assert_eq!(sharing("s-2"), [s("shared:team"), s("ann")], "{}", row("s-2"));
        assert_eq!(sharing("s-3"), [None, None], "a private row: {}", row("s-3"));
        // `show --json` names the same workspace; a private row none.
        assert_eq!(show_json(&store, "s-2").unwrap()["workspace"], json!("shared:team"));
        assert_eq!(show_json(&store, "s-3").unwrap().get("workspace"), None);
    }

    /// `show --json` of an id a row was moved from (`ids.json`, as `wsp mv` and
    /// a shared row's publish or retire record it) is the row it was moved to,
    /// as text `show` is: both resolve through `Store::resolve_task`, and the
    /// document's `id` is the new id, which is how it says which row it is.
    #[test]
    fn show_json_of_an_old_id_is_the_row_it_was_moved_to_under_its_new_id() {
        let _iso = crate::util::isolated("show-moved");
        let store = scratch("show-moved");
        store.save_task(&Task::new("A moved row", "aa-001")).unwrap();
        store.rename_tasks(&[("aa-001".to_string(), "bb-007".to_string())].into()).unwrap();
        assert!(store.task("aa-001").is_none() && store.task("bb-007").is_some());
        let v = show_json(&store, "aa-001").expect("the old id resolves");
        assert_eq!((v["id"].clone(), v["title"].clone()), (json!("bb-007"), json!("A moved row")));
    }

    /// `ls --json` gives a staged row the `legs` objects `show --json` has, and an
    /// unstaged row none at all.
    #[test]
    fn ls_json_carries_a_staged_rows_legs_and_an_unstaged_row_none() {
        let store = scratch("ls-legs");
        let mut staged = Task::new("A staged row", "s-1");
        staged.status_raw = "doing".into();
        crate::legs::write(&mut staged, &[crate::legs::Leg::pending("scout", "look"), crate::legs::Leg::pending("finish", "land")]);
        store.save_task(&staged).unwrap();
        let mut plain = Task::new("An unstaged row", "s-2");
        plain.status_raw = "doing".into();
        store.save_task(&plain).unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_str(&rows_json(&store, &store.tasks())).unwrap();
        let row = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap().clone();
        assert_eq!(row("s-1")["legs"], crate::legs::json(&store.find_task("s-1").unwrap()));
        assert_eq!(row("s-1")["legs"][1]["after"], json!(["scout"]));
        assert!(row("s-2").get("legs").is_none(), "absent, not empty: {}", row("s-2"));
    }

    /// The occurrence, and it is `render-014`'s exactly: `wsp review render-014`
    /// with two modified files in the tree and nothing on the branch, exit 0, a
    /// row reading `review` on every surface the governor looks at, and a
    /// person's turn spent noticing.
    ///
    /// The status is asserted beside the exit code because a refusal that has
    /// already moved the row is the failure being fixed and reads identically
    /// from the code alone.
    #[test]
    fn review_refuses_a_row_with_no_account_and_leaves_it_where_it_was() {
        // **Held for the whole test, and that is the fix rather than tidiness.**
        // `review`'s no-account branch lists what this row has outstanding, and
        // that walk reaches `cmd_checkout::former_ids`, which opens the store
        // from the environment rather than the one it was handed — so the
        // `Store::open()` guard fires unless an isolation is alive. Which made
        // this test pass or fail depending on whether some *other* test happened
        // to be holding one on the same thread, and it failed that way on
        // `wsp-146` the first time the suite ran with any change in it.
        let _iso = crate::util::isolated("review-no-account");
        let store = scratch("review-no-account");
        let mut t = Task::new("wsp review looks like the last step", "wsp-101");
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();

        assert_eq!(review(&store, &parse(&["review", "wsp-101"])), 2);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing", "and it did not move");
    }

    pub fn git_at(dir: &std::path::Path, args: &[&str]) -> String {
        let out = wsp_base::calls::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A member `wsp-101` of a running group with an `agent:` line, in a real
    /// repository, with a tree of its own holding one commit ahead of master.
    #[cfg(test)] // `testkit::repo` is engine's dev-dependency
    pub fn a_member_with_a_commit(tag: &str) -> (crate::util::Isolated, Store, std::path::PathBuf) {
        let env = crate::util::isolated(tag);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        git_at(&repo, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
        let tree = repo.join(".worktrees").join("wsp-101");
        git_at(&repo, &["worktree", "add", "--quiet", "-b", "wsp-101", &tree.display().to_string(), "master"]);
        std::fs::write(tree.join("work.txt"), "mine\n").unwrap();
        git_at(&tree, &["add", "."]);
        git_at(&tree, &["commit", "--quiet", "-m", "the work"]);
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let mut t = Task::new("a member", "wsp-101");
        t.project = Some("p".into());
        t.status_raw = "doing".into();
        t.caption = Some("A member that did its work".into());
        store.save_task(&t).unwrap();
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["wsp-101".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();
        (env, store, tree)
    }

    /// `wsp-341`: review is the ready signal for a member of a run, and it does
    /// not land.
    #[test]
    fn a_member_reviewed_in_a_run_is_ready_and_not_landed() {
        let (_env, store, tree) = a_member_with_a_commit("review-ready");
        let tip = git_at(&tree, &["rev-parse", "HEAD"]);
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 0);
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "review");
        assert_eq!(crate::repair::ready(&t).as_deref(), Some(tip.as_str()));
        assert!(crate::repair::landed(&t).is_none(), "ready is not on the trunk");
        let log = t.section("Log").unwrap();
        assert!(log.contains(&format!("{} {tip} on wsp-101", crate::repair::READY)), "{log}");
        let repo = tree.parent().unwrap().parent().unwrap();
        assert_ne!(git_at(repo, &["rev-parse", "master"]), tip, "nothing reached the trunk");
    }

    /// A member `wsp-101` of a running group whose trunk keeps the layering
    /// test and the component map, and whose one commit adds `src/newmod.rs`
    /// — with its component row when `with_row`. `extra` files go in the same
    /// commit.
    #[cfg(test)]
    fn a_member_adding_a_module(tag: &str, with_row: bool, extra: &[(&str, &str)]) -> (crate::util::Isolated, Store, std::path::PathBuf) {
        let env = crate::util::isolated(tag);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let repo = env.path("repo");
        std::fs::create_dir_all(repo.join("tests")).unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        crate::testkit::repo(&repo);
        std::fs::write(repo.join(crate::check::LAYERING_FILE), "// the layering test\n").unwrap();
        std::fs::write(repo.join("docs/components.md"), "| old | src/old.rs |\n").unwrap();
        std::fs::write(repo.join("src/old.rs"), "\n").unwrap();
        git_at(&repo, &["add", "."]);
        git_at(&repo, &["commit", "--quiet", "-m", "first"]);
        let tree = repo.join(".worktrees").join("wsp-101");
        git_at(&repo, &["worktree", "add", "--quiet", "-b", "wsp-101", &tree.display().to_string(), "master"]);
        std::fs::write(tree.join("src/newmod.rs"), "pub fn f() {}\n").unwrap();
        if with_row {
            std::fs::write(tree.join("docs/components.md"), "| old | src/old.rs |\n| newmod | src/newmod.rs |\n").unwrap();
        }
        for (path, text) in extra {
            std::fs::write(tree.join(path), text).unwrap();
        }
        git_at(&tree, &["add", "."]);
        git_at(&tree, &["commit", "--quiet", "-m", "wsp-101: a module"]);
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let mut t = Task::new("a member", "wsp-101");
        t.project = Some("p".into());
        t.status_raw = "doing".into();
        t.caption = Some("A member that adds a module".into());
        store.save_task(&t).unwrap();
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["wsp-101".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();
        (env, store, tree)
    }

    /// The layering test as a unit test can run it: red, with the test's own
    /// message, while the component map lacks the new module.
    #[cfg(test)]
    const FAKE_LAYERING: &str = "grep -q newmod docs/components.md || { echo 'modules with no component in docs/components.md: src/newmod'; exit 1; }";

    #[cfg(test)]
    fn rule_run(command: &str, limit_secs: u64) {
        crate::check::TEST_RULE_RUN.with(|r| *r.borrow_mut() = Some((command.into(), std::time::Duration::from_secs(limit_secs))));
    }

    /// `wsp-858` (1): a row that adds a module absent from docs/components.md
    /// is refused at `wsp review` with the layering output; it stays at doing
    /// and is not ready, so no verifier opens on it.
    #[test]
    fn a_row_adding_a_module_without_its_component_is_refused_at_review_and_gets_no_verifier() {
        let (_env, store, tree) = a_member_adding_a_module("review-layering-red", false, &[]);
        rule_run(FAKE_LAYERING, 30);
        let tip = git_at(&tree, &["rev-parse", "HEAD"]);
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 1);
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "doing", "and it did not move");
        assert!(crate::repair::ready(&t).is_none(), "not ready, so the cycle owes it no verifier");
        let log = t.section("Log").unwrap();
        assert!(log.contains(&format!("{RULE_RED} {tip} (layering)")), "{log}");
        assert!(log.contains("modules with no component in docs/components.md: src/newmod"), "the layering output is kept: {log}");
    }

    /// `wsp-858` (2): the same row with its component row added reaches review,
    /// ready at its commit, as today.
    #[test]
    fn a_row_adding_a_module_with_its_component_reaches_review_and_its_verifier() {
        let (_env, store, tree) = a_member_adding_a_module("review-layering-green", true, &[]);
        rule_run(FAKE_LAYERING, 30);
        let tip = git_at(&tree, &["rev-parse", "HEAD"]);
        let t = store.task("wsp-101").unwrap();
        let said = rule_gate(&store, &t, None).unwrap();
        assert!(said.iter().any(|l| l.starts_with("layering green in the seat")), "{said:?}");
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 0);
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "review");
        assert_eq!(crate::repair::ready(&t).as_deref(), Some(tip.as_str()), "ready, so its verifier opens");
        assert!(!t.section("Log").unwrap().contains(RULE_RED));
    }

    /// `wsp-858` (3): a commit whose last review was refused on a red rule step
    /// is not put up again unchanged — refused from the record, even where the
    /// step would now pass — and a new commit goes through.
    #[test]
    fn a_commit_refused_on_a_red_rule_step_is_not_marked_ready_again_unchanged() {
        let (_env, store, tree) = a_member_adding_a_module("review-layering-again", false, &[]);
        rule_run(FAKE_LAYERING, 30);
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 1);
        rule_run("true", 30);
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 1, "the same commit, unchanged");
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "doing");
        let lines = rule_gate(&store, &t, None).unwrap_err();
        assert!(lines[0].contains("refused on a red rule step, and nothing has changed since"), "{lines:?}");
        std::fs::write(tree.join("docs/components.md"), "| old | src/old.rs |\n| newmod | src/newmod.rs |\n").unwrap();
        git_at(&tree, &["commit", "--quiet", "-am", "wsp-101: its component"]);
        rule_run(FAKE_LAYERING, 30);
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 0, "a new commit is read again");
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "review");
    }

    /// `wsp-858` (4): a rule step not run in the seat — a cold `target/`, or
    /// stopped at its limit — is said in the review's output, and the review
    /// goes on: the verifier's check runs it.
    #[test]
    fn a_rule_step_skipped_in_the_seat_is_said_at_review() {
        let (_env, store, _tree) = a_member_adding_a_module("review-layering-cold", false, &[("Cargo.lock", "# cold\n")]);
        rule_run(FAKE_LAYERING, 30);
        let t = store.task("wsp-101").unwrap();
        let said = rule_gate(&store, &t, None).unwrap();
        assert!(said.iter().any(|l| l.contains("layering not run in the seat: the tree's target/ is cold")), "{said:?}");
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 0);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "review");
    }

    /// `wsp-858` (4), the limit: a step stopped at its limit in the seat is
    /// said as not finished, never as red.
    #[test]
    fn a_rule_step_stopped_at_its_limit_in_the_seat_is_said_and_not_red() {
        let (_env, store, _tree) = a_member_adding_a_module("review-layering-limit", false, &[]);
        rule_run("sleep 5", 1);
        let t = store.task("wsp-101").unwrap();
        let said = rule_gate(&store, &t, None).unwrap();
        assert!(said.iter().any(|l| l.contains("layering not finished in the seat: stopped at its 1s limit")), "{said:?}");
    }

    /// compounding-617: a `## Checks` line that runs `wsp check` on its own row
    /// (compounding-520, wsp-638) is refused at review, named, and the row does
    /// not move; with the line fixed the review goes through.
    #[test]
    fn review_refuses_a_check_line_that_runs_the_rows_own_check() {
        let (_env, store, _tree) = a_member_with_a_commit("review-self-check");
        let write = |line: &str| {
            let mut t = store.task("wsp-101").unwrap();
            crate::model::set_section_in(&mut t.body, "Checks", line);
            store.save_task(&t).unwrap();
        };
        write("- timeline tests (2s): wsp check wsp-101");
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 2);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing", "and it did not move");
        write("- timeline tests (2s): cargo test timeline::");
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done, nothing left"])), 0);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "review");
    }

    /// `wsp-947`: a submit refused for its `checks:` leaves `## Checks` as it
    /// was, declared before or not. The test runs itself as a child to read
    /// what the refusal printed.
    #[test]
    fn a_refused_submit_leaves_the_rows_checks_as_they_were() {
        const CHILD: &str = "WSP_TEST_REFUSED_SUBMIT";
        if std::env::var_os(CHILD).is_none() {
            let out = wsp_base::calls::Command::new(std::env::current_exe().unwrap())
                .args(["cmd_task::tests::a_refused_submit_leaves_the_rows_checks_as_they_were", "--exact", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let said = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{}{said}", String::from_utf8_lossy(&out.stdout));
            assert_eq!(said.matches("its ## Checks has line(s) that call its own check:").count(), 2, "{said}");
            return;
        }
        let (env, store, _tree) = a_member_with_a_commit("submit-refused-checks");
        let file = env.path("exit.md");
        std::fs::write(
            &file,
            "summary: the member did its work\n\nIt wrote work.txt and left nothing.\n\ndone-when:\n- held: the work — work.txt\n\nchecks:\n- own (1m): wsp check wsp-101\n\nasks:\n- note: nothing\n\nowed: nothing\n",
        )
        .unwrap();
        let path = file.display().to_string();
        let submit = parse(&["submit", "wsp-101", "--from", &path]);
        let checks = || store.task("wsp-101").unwrap().section("Checks");
        for declared in [None, Some("- closing (2m): true\n- legs (1m): true")] {
            let mut t = store.task("wsp-101").unwrap();
            crate::model::set_section_in(&mut t.body, "Checks", declared.unwrap_or_default());
            store.save_task(&t).unwrap();
            assert_eq!(crate::cmd_submit::submit_as(&store, &submit, Some("member")), 2);
            assert_eq!(checks().as_deref(), declared);
            assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing");
        }
    }

    /// `wsp-529`: a commit a verifier blocked is not put up again unchanged —
    /// refused with the verdict's head, the row does not move; a new commit is
    /// allowed; `--anyway "why"` puts it up as it is and the log says so.
    #[test]
    fn review_is_refused_at_an_unchanged_blocked_commit_and_allowed_after_a_new_one_or_anyway() {
        let (_env, store, tree) = a_member_with_a_commit("review-blocked");
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done"])), 0);
        let tip = git_at(&tree, &["rev-parse", "HEAD"]);
        let mut t = store.task("wsp-101").unwrap();
        let mut ps = vec![crate::verification::Pass::opened(Some(tip.clone()), None)];
        ps[0].state = crate::verification::State::Blocks;
        ps[0].text = "signing breaks the already-live no-op\nsecond line".into();
        crate::verification::write(&mut t, &ps);
        crate::verification::send_back(&mut t, crate::util::now_iso(), "a verifier", Some(&tip), "signing breaks the no-op", None);
        store.save_task(&t).unwrap();

        assert_eq!(review(&store, &parse(&["review", "wsp-101", "fixed it"])), 1, "same commit, clean tree: refused");
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "doing", "and the row did not move");
        let lines = blocked_refusal("wsp-101", &crate::verification::passes(&t)[0]).join("\n");
        assert!(lines.contains("signing breaks") && lines.contains("## Verification") && lines.contains("--anyway"), "{lines}");

        let anyway = parse(&["review", "wsp-101", "--anyway", "the verdict reads the wrong file", "I disagree"]);
        assert_eq!(review(&store, &anyway), 0, "--anyway overrides");
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "review");
        let log = t.section("Log").unwrap();
        assert!(log.contains("anyway:") && log.contains("the verdict reads the wrong file") && log.contains(&tip[..12]), "{log}");

        // Back to doing, a new commit, and review goes through.
        let mut t = store.task("wsp-101").unwrap();
        t.set_status(Status::Doing);
        store.save_task(&t).unwrap();
        std::fs::write(tree.join("more.txt"), "fix\n").unwrap();
        git_at(&tree, &["add", "."]);
        git_at(&tree, &["commit", "--quiet", "-m", "the fix"]);
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "fixed it for real"])), 0, "a new commit is reviewed");
        assert!(!store.task("wsp-101").unwrap().section("Log").unwrap().lines().last().unwrap_or_default().contains("anyway:"));
    }

    /// A tree holding uncommitted work cannot be ready: refused, named, and the
    /// row does not move.
    #[test]
    fn a_dirty_tree_refuses_review() {
        let (_env, store, tree) = a_member_with_a_commit("review-dirty");
        std::fs::write(tree.join("loose.txt"), "not committed\n").unwrap();
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done"])), 1);
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "doing", "and the row did not move");
        assert!(crate::repair::ready(&t).is_none());
    }

    /// A row in no run is left as before: no ready line, no refusal.
    #[test]
    fn a_row_in_no_run_records_no_ready_line() {
        let (_env, store, tree) = a_member_with_a_commit("review-no-run");
        let mut w = store.worklist("run").unwrap();
        w.set_status(crate::model::WorklistStatus::Draft);
        store.save_worklist(&w).unwrap();
        std::fs::write(tree.join("loose.txt"), "not committed\n").unwrap();
        assert_eq!(review(&store, &parse(&["review", "wsp-101", "done"])), 0);
        assert!(crate::repair::ready(&store.task("wsp-101").unwrap()).is_none());
    }

    /// Where the account goes, which on 2026-08-24 a governor and an agent
    /// disagreed about in good faith — one had written it onto the row's
    /// overview, the other was reading the log. `review` takes it and puts it
    /// in the log, so there is nothing left to disagree about.
    #[test]
    fn the_account_it_takes_is_written_into_the_log_a_barrier_reads() {
        // An isolation for the whole test, for
        // [`review_refuses_a_row_with_no_account_and_leaves_it_where_it_was`]'s
        // reason: `review` reaches `cmd_checkout::former_ids`, which opens the
        // store from the environment. All three `review` tests need it and all
        // three are the same shape.
        let _iso = crate::util::isolated("review-account");
        let store = scratch("review-account");
        let mut t = Task::new("wsp review looks like the last step", "wsp-101");
        t.status_raw = "doing".into();
        t.caption = Some("Review takes the account".into());
        store.save_task(&t).unwrap();

        let said = "the gate is on the account, not on what landed — ui-007 had nothing to land";
        assert_eq!(review(&store, &parse(&["review", "wsp-101", said])), 0);

        let back = store.task("wsp-101").unwrap();
        assert_eq!(back.status_raw, "review");
        let log = back.section("Log").expect("the log is where it went");
        assert!(log.contains(&format!("review: {said}")), "{log}");
        assert!(
            !back.section("Overview").unwrap_or_default().contains("ui-007"),
            "and not onto the prose the row was written with"
        );
    }

    /// A row that is genuinely finished with nothing to land is still owed one.
    ///
    /// This is `ui-007`, and it is the case that decides the whole shape: it
    /// reached review having produced no diff, which was *correct*, so a gate
    /// keyed on "nothing is ahead of the trunk" would refuse the one row that
    /// had done the right thing. What it did not do was say so unprompted. So
    /// the account is what is refused over, and it is refused over identically
    /// whether or not there is a commit anywhere.
    #[test]
    fn a_row_that_correctly_produced_no_diff_owes_the_same_account() {
        // The isolation, again for the same reason as the two tests above it.
        let _iso = crate::util::isolated("review-no-diff");
        let store = scratch("review-no-diff");
        let mut t = Task::new("the worklists zones prove their fit", "ui-007");
        t.status_raw = "doing".into();
        t.caption = Some("The zones prove their fit".into());
        store.save_task(&t).unwrap();

        assert_eq!(review(&store, &parse(&["review", "ui-007"])), 2, "no diff is not no account");
        assert_eq!(
            review(&store, &parse(&["review", "ui-007", "the zones already fitted; nothing to change"])),
            0,
            "and having said so, it goes to review with nothing landed and no argument"
        );
        assert_eq!(store.task("ui-007").unwrap().status_raw, "review");
    }

    /// E-1, Ed's answer of 2026-10-09: a row with no caption is refused at
    /// review, and the refusal carries the derived line as the command that
    /// takes it — so the paste is the whole of the fix, and the review after
    /// it goes through with the caption the title gave.
    #[test]
    fn review_refuses_a_row_with_no_caption_and_hands_over_the_derived_one() {
        let _iso = crate::util::isolated("review-no-caption");
        let store = scratch("review-no-caption");
        let mut t = Task::new(
            "gpui: IME preedit (marked text) is not drawn in a pane — the wire has no preedit message",
            "wsp-101",
        );
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();

        let account = "preedit drawn; the wire carries it now";
        assert_eq!(review(&store, &parse(&["review", "wsp-101", account])), 2);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing", "refused before anything moved");

        let lines = uncaptioned(&store.task("wsp-101").unwrap()).expect("no caption is refused");
        let paste = lines.iter().find(|l| l.trim_start().starts_with("wsp caption")).expect("the paste is printed");
        assert_eq!(paste.trim(), "wsp caption wsp-101 'IME preedit is not drawn in a pane'");

        assert_eq!(caption(&store, &parse(&["caption", "wsp-101", "IME preedit is not drawn in a pane"])), 0);
        assert_eq!(review(&store, &parse(&["review", "wsp-101", account])), 0);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "review");
    }

    /// A derived line past the limit would be refused when pasted, so the
    /// refusal does not offer it as a paste: it asks for the author's words.
    #[test]
    fn a_derived_caption_past_sixty_is_not_offered_as_the_paste() {
        let t = Task::new(
            "Parity audit: every golden, gallery, ctl act and accelerator the imgui window has, checked on gpui",
            "c-1",
        );
        let lines = uncaptioned(&t).unwrap();
        assert!(lines.iter().all(|l| !l.contains("wsp caption c-1 '")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("in your own words")), "{lines:?}");
    }

    /// `wsp caption` sets, replaces and clears, and every one is in the log
    /// with what it replaced — the shape `rename` gives a title. Past sixty
    /// is refused and nothing is written.
    #[test]
    fn a_caption_is_set_replaced_and_cleared_and_each_is_logged_like_a_rename() {
        let _iso = crate::util::isolated("caption-verb");
        let store = scratch("caption-verb");
        store.save_task(&Task::new("The chrome leaves imgui for gpui (compound-332)", "c-332")).unwrap();

        assert_eq!(caption(&store, &parse(&["caption", "c-332", "Leave imgui"])), 0);
        assert_eq!(caption(&store, &parse(&["caption", "c-332", "The chrome on gpui"])), 0);
        assert_eq!(caption(&store, &parse(&["caption", "c-332", &"x".repeat(61)])), 2, "past sixty");
        let t = store.task("c-332").unwrap();
        assert_eq!(t.caption.as_deref(), Some("The chrome on gpui"), "and the refusal wrote nothing");
        assert_eq!(crate::caption::of_task(&t).json(), json!({ "text": "The chrome on gpui", "from": "authored" }));

        assert_eq!(caption(&store, &parse(&["caption", "c-332", "--clear"])), 0);
        let t = store.task("c-332").unwrap();
        assert_eq!(t.caption, None);
        assert_eq!(crate::caption::of_task(&t).json(), json!({ "text": "The chrome leaves imgui for gpui", "from": "title" }));
        let log = t.section("Log").unwrap();
        assert!(log.contains("captioned \"Leave imgui\""), "{log}");
        assert!(log.contains("captioned \"The chrome on gpui\" (was \"Leave imgui\")"), "{log}");
        assert!(log.contains("caption cleared (was \"The chrome on gpui\")"), "{log}");
    }

    /// The same verb names a worklist by its slug, and the slug is asked
    /// first: a task whose title mentions the slug is not what was meant.
    #[test]
    fn caption_takes_a_worklist_by_its_exact_slug() {
        let _iso = crate::util::isolated("caption-worklist");
        let store = scratch("caption-worklist");
        store.save_task(&Task::new("gpui-migration leftovers", "c-1")).unwrap();
        store.save_worklist(&crate::model::Worklist::new("gpui-migration", "gpui migration: the chrome leaves imgui")).unwrap();

        assert_eq!(caption(&store, &parse(&["caption", "gpui-migration", "Move the chrome to gpui"])), 0);
        assert_eq!(store.worklist("gpui-migration").unwrap().caption.as_deref(), Some("Move the chrome to gpui"));
        assert_eq!(store.task("c-1").unwrap().caption, None, "the task that mentions it is untouched");
    }

    /// `add --caption` stores it, and a title ending in one of add's own
    /// flags is refused before an id is spent — compound-404 to -407, whose
    /// titles end in a literal ` --parent compound-403`.
    #[test]
    fn add_takes_a_caption_and_refuses_a_flag_swallowed_into_the_title() {
        let _iso = crate::util::isolated("add-caption");
        let store = scratch("add-caption");
        assert_eq!(add(&store, &parse(&["add", "Close the gaps blocking cutover — six of them", "--inbox", "--caption", "Close the gaps"])), 0);
        let made = store.tasks();
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].caption.as_deref(), Some("Close the gaps"));

        assert_eq!(add(&store, &parse(&["add", "Agents view as a hierarchy --parent compound-403", "--inbox"])), 2);
        assert_eq!(store.tasks().len(), 1, "refused before anything was filed");
        assert_eq!(add(&store, &parse(&["add", "--inbox", "--caption", &"x".repeat(61), "Too long a caption"])), 2);
        assert_eq!(store.tasks().len(), 1);
    }

    /// A shared-work add-on that lets shared rows be filed and gives each one
    /// the id it is scripted with, or refuses.
    struct Ids(Result<&'static str, &'static str>);

    impl crate::addon::SharedWork for Ids {
        fn may_file_shared(&self, _: &Store) -> Result<(), String> {
            Ok(())
        }
        fn new_row_id(&self, _: &Store, _: Option<&str>, ws: &crate::model::WorkspaceId, _: Option<&str>) -> Result<Option<String>, String> {
            if ws.is_private() {
                return Ok(None);
            }
            self.0.map(|id| Some(id.to_string())).map_err(str::to_string)
        }
    }

    /// `wsp add` takes a new row's id from the shared-work add-on where it gives
    /// one (a shared row's from the hub), writes nothing when it refuses, and
    /// takes the local counter for a private row and with no add-on.
    #[test]
    fn add_takes_a_shared_rows_id_from_the_addon_and_writes_nothing_when_it_refuses() {
        let _iso = crate::util::isolated("add-ids");
        let store = scratch("add-ids");
        store.save_project(&crate::model::Project::new("pp")).unwrap();
        {
            let _a = crate::addon::scoped(std::sync::Arc::new(Ids(Ok("pp-042"))));
            assert_eq!(add(&store, &parse(&["add", "shared work", "-p", "pp", "--workspace", "shared:team"])), 0);
            let t = store.task("pp-042").expect("filed under the add-on's id");
            assert_eq!((t.title.as_str(), t.workspace.to_string().as_str()), ("shared work", "shared:team"));
            assert_eq!(add(&store, &parse(&["add", "my own", "-p", "pp"])), 0);
            assert!(store.tasks().iter().any(|t| t.title == "my own" && t.id != "pp-042" && t.workspace.is_private()), "a private row takes the local counter");
        }
        let before = store.tasks().len();
        {
            let _a = crate::addon::scoped(std::sync::Arc::new(Ids(Err("you are not a member of shared:team"))));
            assert_eq!(add(&store, &parse(&["add", "refused", "-p", "pp", "--workspace", "shared:team"])), 1);
            assert_eq!(store.tasks().len(), before, "a refusal writes nothing");
        }
        let _a = crate::addon::scoped(std::sync::Arc::new(crate::addon::Standalone));
        assert_eq!(add(&store, &parse(&["add", "alone", "-p", "pp"])), 0);
        assert!(store.tasks().iter().any(|t| t.title == "alone"));
    }

    #[test]
    fn only_adds_own_flags_on_the_end_of_a_title_are_read_as_swallowed() {
        assert_eq!(flag_in_title("Agents view --parent compound-403").as_deref(), Some("--parent compound-403"));
        assert_eq!(flag_in_title("A row -p wsp").as_deref(), Some("-p wsp"));
        assert_eq!(flag_in_title("A row --prio=high").as_deref(), Some("--prio=high"));
        assert_eq!(flag_in_title("A row --inbox").as_deref(), Some("--inbox"));
        assert_eq!(flag_in_title("--parent is add-only, and mv takes it too"), None, "a title about the flag");
        assert_eq!(flag_in_title("spawn --headless leaves no pane"), None, "not one of add's");
        assert_eq!(flag_in_title("Ends in a range 3 -5"), None);
    }

    /// The `under` line did not go wrong when its parent was swept — it went
    /// *away*, which is worse. A sub-task read on its own is missing the one
    /// thing that says why it exists, and a row that vanishes silently cannot
    /// be told from a task that never had a parent at all.
    ///
    /// Asserted through `store` rather than through printed output because
    /// `show` prints: what the fix turns on is that the parent is findable, and
    /// that the live store is asked first so a live parent never pays for the
    /// archive walk.
    #[test]
    fn a_parent_that_was_swept_is_still_found_rather_than_quietly_dropped() {
        let store = scratch("show-archived-parent");
        let mut parent = Task::new("the argument this hangs from", "wsp-020");
        parent.updated = "2026-08-14T00:00:00Z".into();
        store.save_task(&parent).unwrap();
        let mut child = Task::new("still open", "wsp-021");
        child.parent = Some("wsp-020".into());
        store.save_task(&child).unwrap();

        assert!(store.tasks().iter().any(|x| x.id == "wsp-020"), "live first, and it is");
        store.archive_task(&parent).unwrap();

        assert!(!store.tasks().iter().any(|x| x.id == "wsp-020"), "now it is not");
        let (found, _) = store.archived_task("wsp-020").expect("but show can still name it");
        assert_eq!(found.title, "the argument this hangs from");
        assert_eq!(show(&store, &parse(&["show", "wsp-021"])), 0);
    }

    /// The verb that reads a record has to be able to read an archived one.
    ///
    /// Every other verb stops at the sentence — `claim` and `edit` have nothing
    /// sane to do with a task that has left the live store, and handing them one
    /// would put a live file beside the archived one under a single name. `show`
    /// is the exception because *displaying the record is the verb*, and this
    /// project reads by citation: `render-026`'s prose says "same seam as
    /// render-018" and `render-018` was swept, so the reader following that
    /// reference wanted the argument in it and got `no such task`.
    ///
    /// The exit code is the assertion because it is the load-bearing change —
    /// `show` answered 1 and printed nothing, and it now answers 0 and prints
    /// the record. The prose being reachable at all is asserted beside it, since
    /// an exit code alone would pass on a `show` that found the file and had
    /// nothing to say about it.
    #[test]
    fn show_reads_an_archived_task_because_reading_the_record_is_the_verb() {
        let store = scratch("show-archived");
        let mut t = Task::new("Overscrolling has dead travel", "render-018");
        t.updated = "2026-08-15T22:08:05Z".into();
        t.body = "## Overview\nThe wheel moved the cursor and the view was derived from it.\n".into();
        store.save_task(&t).unwrap();
        store.archive_task(&t).unwrap();

        assert_eq!(show(&store, &parse(&["show", "render-018"])), 0, "it is there and it is readable");
        let (found, path) = store.archived_task("render-018").expect("and this is what it read");
        assert!(found.body.contains("the view was derived from it"), "the argument came with it");
        assert!(path.ends_with("2026-08/render-018.md"), "{}", path.display());

        assert_eq!(show(&store, &parse(&["show", "render-999"])), 1, "and nothing is still nothing");
    }

    /// Two done tasks, one on either side of the cutoff — enough to say both
    /// halves of what a dry run is for.
    pub fn swept_store(tag: &str) -> Store {
        let store = scratch(tag);
        let mut old = Task::new("Overscrolling has dead travel", "render-018");
        old.updated = "2025-11-02T09:00:00Z".into();
        old.status_raw = "done".into();
        store.save_task(&old).unwrap();
        let mut recent = Task::new("the barrier design rests on this", "wsp-084");
        recent.updated = util::now_iso();
        recent.status_raw = "done".into();
        store.save_task(&recent).unwrap();
        store
    }

    /// The fault this row was written about, and it was sharper than "the flag
    /// is missing". `-n` expands to `--dry-run` for every verb in the parser,
    /// so `wsp archive -n` parsed, swept for real, and only afterwards said the
    /// word had gone nowhere — the careful invocation was the destructive one.
    ///
    /// Asserted on the store rather than on the printed line, because printing
    /// "would archive" while the file has already moved is exactly the failure
    /// being fixed: a saying-so version that does the damage anyway.
    #[test]
    fn a_dry_run_says_what_it_would_sweep_and_leaves_every_file_where_it_is() {
        let store = swept_store("archive-dry");
        assert_eq!(archive(&store, &parse(&["archive", "-n"])), 0);
        assert!(
            store.tasks().iter().any(|t| t.id == "render-018"),
            "a dry run moved the file it was only asked about"
        );
        assert!(store.archived_task("render-018").is_none(), "and nothing is in the archive");

        assert_eq!(archive(&store, &parse(&["archive"])), 0, "now for real");
        assert!(!store.tasks().iter().any(|t| t.id == "render-018"), "the sweep did not run");
        assert!(store.archived_task("render-018").is_some(), "and this is where it went");
    }

    /// `--all` is the most-typed word in the CLI and everywhere else in wsp it
    /// widens what is *looked at*. On this verb it widened what was *moved*,
    /// and it was the only thing between two words and the whole store. The
    /// cutoff now comes off by a number, and the retired word is refused by the
    /// verb rather than left to `unknown_flags` — which runs after the sweep has
    /// already committed, and whose nearest suggestion was `--full`.
    ///
    /// The store is asserted beside the exit code, because a refusal arriving
    /// after the write is the failure being fixed and reads identically from
    /// the exit code alone.
    #[test]
    fn the_cutoff_comes_off_by_a_number_and_no_longer_by_the_word_all() {
        let store = swept_store("archive-all");
        assert_eq!(archive(&store, &parse(&["archive", "--all"])), 2, "and it says why");
        assert_eq!(store.tasks().len(), 2, "the refusal came before the sweep, not after it");

        assert_eq!(archive(&store, &parse(&["archive", "--days", "0"])), 0);
        assert!(!store.tasks().iter().any(|t| t.id == "wsp-084"), "--days 0 is the way to ask");
    }

    /// `worklist-018`. A hand carrying a paragraph and no sentence drew as a
    /// blank row at the seat, and `wsp show <id>` — the seat's next move — said
    /// nothing about it either, so the only place the words existed was
    /// `flags.json`. That is how this task's own fault was found, and it is why
    /// the headline is the first line of the text and never a second field.
    #[test]
    fn a_task_says_what_is_raised_about_it_and_the_headline_is_the_first_line() {
        let store = scratch("show-raised");
        let t = task_with(&store, "worklist-010", "high");
        let m = message::Message::question(
            message::Party::pane("w46:p1", "w46"),
            message::Kind::Note,
            "landed and NOT INSTALLED — the retry hazard is live\n\nuntil somebody runs `wsp install`",
            message::Waiting::new("w46:p1", &t.id),
        )
        .about(message::About::Task(t.id.clone()));
        message::raise(&store, &m).unwrap();

        let up = raised_about(&store, &t.id);
        assert_eq!(up.len(), 1, "one hand is up");
        assert_eq!(
            headline(&up[0]),
            "landed and NOT INSTALLED — the retry hazard is live",
            "the first line is the headline, and there is no other field to prefer over it",
        );
        assert_eq!(
            up[0].body(),
            "until somebody runs `wsp install`",
            "and the rest is still there to draw",
        );
        assert!(asking(&up[0]).contains("wsp answer "), "with the verb that ends it: {}", asking(&up[0]));
    }

    /// `wsp flag <id>` on its own is a complete thing to say — *look at this
    /// task, it exists*. It is the one input that leaves no words in the
    /// record, and it is said out loud rather than drawn as an empty line,
    /// because a blank row is indistinguishable from a hand about a task that
    /// has been retired.
    #[test]
    fn a_hand_with_nothing_written_on_it_is_said_rather_than_left_blank() {
        let store = scratch("show-raised-empty");
        let t = task_with(&store, "worklist-011", "normal");
        let m = message::Message::new(message::Party::Human(None), message::Kind::Note, "")
            .about(message::About::Task(t.id.clone()));
        message::raise(&store, &m).unwrap();

        let up = raised_about(&store, &t.id);
        assert_eq!(headline(&up[0]), "raised this, with nothing written on it");
        assert!(asking(&up[0]).contains("wsp ack "), "a notification is acknowledged, not answered");
    }

    pub fn task_with(store: &Store, id: &str, level: &str) -> Task {
        let mut t = Task::new("Retune the early reflections", id);
        t.project = Some("verb".into());
        t.priority_raw = level.into();
        store.save_task(&t).unwrap();
        t
    }

    /// The shape `fork-003` was actually in on 2026-08-19, under the filter it
    /// was found with. A count taken over the filtered list is not absent, it
    /// is wrong — and wrong in the direction that makes the parent look ready.
    #[test]
    fn a_parent_counts_the_work_under_it_and_not_the_work_the_filter_left() {
        let store = scratch("ls-under-filter");
        let mut parent = Task::new("What the fork could own of agent spawning", "fork-003");
        parent.project = Some("fork".into());
        parent.status_raw = "review".into();
        store.save_task(&parent).unwrap();
        for (id, status) in [("fork-014", "todo"), ("fork-015", "review"), ("fork-016", "todo")] {
            let mut kid = Task::new("a piece of it", id);
            kid.project = Some("fork".into());
            kid.status_raw = status.into();
            kid.parent = Some("fork-003".into());
            store.save_task(&kid).unwrap();
        }

        let all = store.tasks();
        let shown: Vec<Task> =
            all.iter().filter(|t| t.status() == Status::Review).cloned().collect();
        let lines = task_lines(&shown, &all, &Paint::plain(), false, &Default::default());

        let parent_line = lines.iter().find(|l| l.contains("fork-003")).expect("the parent");
        assert!(
            parent_line.contains("(3 open)"),
            "the parent should count every open sub-task, not the one the filter kept: {parent_line}"
        );
        // And the rows themselves are still the filter's: the count is the
        // whole of what the parent borrows from outside the list.
        assert_eq!(lines.len(), 2, "the filter still decides which rows there are: {lines:?}");
    }

    /// ui-001. `--all` widens the list past open work; the header kept the
    /// word `open` over what had become a total, so `wsp ls -p fork --all`
    /// read `(27 open)` over twenty-seven done rows and the number was read
    /// aloud as work still outstanding. The label follows the list.
    #[test]
    fn a_widened_listing_says_what_its_count_counts() {
        let store = scratch("ls-all-header");
        store.save_project(&crate::model::Project::new("fork")).unwrap();
        for (id, status) in [("fork-001", "doing"), ("fork-002", "done"), ("fork-003", "done")] {
            let mut t = Task::new("a piece of the fork", id);
            t.project = Some("fork".into());
            t.status_raw = status.into();
            store.save_task(&t).unwrap();
        }

        let index = Index::new(store.projects());
        let args = Args::synth("ls", &[], &[("all", ""), ("project", "fork")]);
        let said = scope_line("fork", &filtered(&store, &args, &index, Some("fork".into())));
        assert!(said.contains("1 open"), "{said}");
        assert!(said.contains("2 done"), "the total is not an open count: {said}");

        // And the default keeps its word: everything it shows is open, so the
        // short form is still the truth there.
        let args = Args::synth("ls", &[], &[("project", "fork")]);
        let said = scope_line("fork", &filtered(&store, &args, &index, Some("fork".into())));
        assert!(said.contains("(1 open)"), "{said}");
        assert!(!said.contains("done"), "nothing finished is on the list to count: {said}");
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
        // most often is: retired, and still the one you meant. The key goes on
        // the end now that it prints those rows rather than counting them.
        store.archive_task(&t).unwrap();
        let said = nothing(&store, &index, &args, None, "reverb");
        assert!(said.contains("1 in the archive"), "{said}");
        assert!(said.contains("--all"), "and it says what reaches them: {said}");

        // Not when `--all` is already on the line. That is how the archive
        // came to be searched at all, so the hits this can still see are ones
        // `-p` or `-t` hid, and pointing at a key already typed says nothing
        // about which of them to take off.
        let all = Args::synth("find", &["reverb"], &[("all", ""), ("project", "wsp")]);
        let said = nothing(&store, &index, &all, Some("wsp"), "reverb");
        assert!(said.contains("1 in the archive") && !said.contains("--all"), "{said}");
    }

    /// wsp-764: a sweep files every due row in one pass and reports them all.
    #[test]
    fn a_sweep_archives_every_done_row_in_one_run() {
        let store = scratch("sweep-batch");
        store.save_project(&crate::model::Project::new("render")).unwrap();
        for n in 0..30 {
            let mut t = Task::new("old work", &format!("render-{n:03}"));
            t.project = Some("render".into());
            t.updated = "2026-01-15T22:08:05Z".into();
            t.status_raw = "done".into();
            store.save_task(&t).unwrap();
        }
        let args = Args::synth("archive", &[], &[("days", "0"), ("json", "")]);
        assert_eq!(archive(&store, &args), 0);
        assert!(store.tasks().is_empty());
        assert_eq!(store.archived_ids().len(), 30);
    }

    /// The dead end this task was filed about: `find` could see the archive
    /// and could not reach it.
    ///
    /// "nothing matching X — 1 in the archive" is honest and useless. It
    /// withholds the id, and the id is the whole of what a finding aid owes,
    /// because `wsp show` reads an archived record in full — so the id is a
    /// working reference and not a consolation prize. `--all` already meant
    /// "widen past the defaults"; the live set was the one boundary it left in
    /// place.
    #[test]
    fn all_reaches_the_archive_and_a_search_without_it_still_does_not() {
        let store = scratch("find-archived");
        for id in ["render", "verb"] {
            store.save_project(&crate::model::Project::new(id)).unwrap();
        }
        let mut t = Task::new("Overscrolling has dead travel", "render-018");
        t.project = Some("render".into());
        t.updated = "2026-08-15T22:08:05Z".into();
        store.save_task(&t).unwrap();
        store.archive_task(&t).unwrap();

        let index = Index::new(store.projects());
        let plain = Args::synth("find", &["dead travel"], &[]);
        assert!(
            archived_hits(&store, &plain, &index, None, "dead travel").is_empty(),
            "the walk is a directory read per archive month and find runs constantly"
        );

        let all = Args::synth("find", &["dead travel"], &[("all", "")]);
        let hits = archived_hits(&store, &all, &index, None, "dead travel");
        assert_eq!(hits.len(), 1, "and this is the id the sentence used to withhold");
        assert_eq!(hits[0].id, "render-018");

        // `-p` is a scope somebody typed, and it means the same thing on both
        // sides of the boundary — an archived task in another project is as
        // much noise as a live one.
        let elsewhere = Args::synth("find", &["dead travel"], &[("all", ""), ("project", "verb")]);
        assert!(
            archived_hits(&store, &elsewhere, &index, Some("verb".into()), "dead travel").is_empty(),
            "a filter that is visible is not widened by the flag beside it"
        );
    }

    /// What an archived row says about itself, which is the behaviour and not
    /// a detail of it: a reader who acts on one of these rows as if it were
    /// live has been misled by the list.
    ///
    /// The status underneath is deliberately not shown. A swept task is `done`
    /// and one removed by hand kept whatever it had on the way out; neither is
    /// a fact about now, and the column is worth more saying the record is not
    /// live. Its live neighbour is asserted beside it, because a marker that
    /// appeared on every row would say nothing.
    #[test]
    fn an_archived_row_says_so_where_a_live_row_says_its_status() {
        let mut live = Task::new("Overscrolling still has dead travel", "render-041");
        live.project = Some("render".into());
        live.status_raw = "doing".into();
        let mut filed = Task::new("Overscrolling has dead travel", "render-018");
        filed.project = Some("render".into());
        filed.status_raw = "done".into();

        let rows = vec![(live, false), (filed, true)];
        let lines = hit_lines(&rows, "dead travel", &Paint::plain());

        assert!(lines[0].contains("render-041") && lines[0].contains("doing"), "{lines:?}");
        assert!(lines[1].contains("render-018") && lines[1].contains("archived"), "{lines:?}");
        assert!(!lines[1].contains("done"), "the stale status is not what the column is for: {lines:?}");
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

    /// A row can be told where its work is after it exists, which is the only
    /// time anybody knows.
    ///
    /// `--ref` on `add` was the whole of it, and `core-042` turned `refs` into
    /// the thing that decides where a spawned agent may reach — so an add-only
    /// field meant the mechanism worked on no row that had already been filed.
    /// `worklist-surface` was five such rows and one spec outside every
    /// worktree.
    #[test]
    fn a_row_can_be_told_which_file_outside_its_tree_it_needs() {
        let store = scratch("ref-edit");
        let t = task_with(&store, "verb-021", "normal");
        store.save_task(&t).unwrap();

        let at = format!("{}/claude/.scratch/spec.html", util::home().display());
        assert_eq!(reference(&store, &Args::synth("ref", &["021", &at], &[])), 0);
        // Stored the way `add` stores it, which is the way `wsp show` prints
        // it, which is what a caller has in front of them to remove by.
        assert_eq!(
            store.find_task("021").expect("the task").refs,
            vec!["~/claude/.scratch/spec.html".to_string()]
        );

        // And it comes off again, named either way round.
        assert_eq!(reference(&store, &Args::synth("ref", &["021", "-~/claude/.scratch/spec.html"], &[])), 0);
        assert!(store.find_task("021").expect("the task").refs.is_empty());
        // A removal that removed nothing is [`tag`]'s rule, for its reason.
        assert_eq!(reference(&store, &Args::synth("ref", &["021", "-~/nowhere.md"], &[])), 1);
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

    pub fn open_task(id: &str, title: &str, status: &str, prio: &str) -> Task {
        let mut t = Task::new(title, id);
        t.project = Some("wsp".into());
        t.status_raw = status.to_string();
        t.priority_raw = prio.to_string();
        t
    }

    /// Four open tasks, one of them in another agent's hand.
    pub fn backlog() -> Backlog {
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

    pub fn parse(line: &[&str]) -> Args {
        Args::parse(line.iter().map(|s| (*s).to_string()).collect())
    }

    /// `wsp-305`, at the verb: `done` takes the row's tree when its work is on
    /// the trunk, and `--keep-tree` leaves it.
    #[test]
    fn done_takes_a_landed_rows_tree_and_keep_tree_leaves_it() {
        let env = util::isolated("done-tree");
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = wsp_base::calls::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "--quiet", "-b", "master"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "--quiet", "-m", "first"]);
        let store = Store::open();
        store.ensure_dirs().unwrap();
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let mut trees = Vec::new();
        for id in ["p-001", "p-002"] {
            let mut t = Task::new("a row", id);
            t.project = Some("p".into());
            t.set_status(Status::Review);
            store.save_task(&t).unwrap();
            let dir = crate::cmd_checkout::checkout_dir(&repo, id);
            crate::cmd_checkout::ensure(&repo, &dir, id, "master").unwrap();
            trees.push(dir);
        }

        assert_eq!(done(&store, &parse(&["done", "p-001"]), &|_| false, &Reads(vec![])), 0);
        assert!(!trees[0].exists(), "a done row whose work is on the trunk kept its tree");
        assert_eq!(done(&store, &parse(&["done", "p-002", "--keep-tree"]), &|_| false, &Reads(vec![])), 0);
        assert!(trees[1].join(".git").exists(), "--keep-tree did not keep it");
        assert_eq!(store.task("p-002").unwrap().status(), Status::Done);
    }

    use crate::place::State;
    use std::collections::BTreeMap;

    thread_local! {
        /// The last refusal `wsp done` made, as `--json` prints it (`wsp-315`).
        pub(super) static REFUSED: std::cell::RefCell<Option<serde_json::Value>> = const { std::cell::RefCell::new(None) };
    }

    pub fn refused() -> serde_json::Value {
        REFUSED.with(|r| r.borrow_mut().take()).expect("done refused")
    }

    pub fn labels(v: &serde_json::Value, key: &str) -> Vec<String> {
        v[key].as_array().unwrap().iter().map(|b| b["label"].as_str().unwrap().to_string()).collect()
    }

    /// Mark `seat` as on the far machine `machine`, as `place_compound` records it.
    pub fn far(store: &Store, seat: &str, machine: &str) {
        let dir = store.state.join(crate::place_compound::SEATS).join(seat);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seat.json"), json!({ "machine": machine }).to_string()).unwrap();
    }

    /// `wsp-315`. `wsp done --json`'s refusal carries the reason and the
    /// buttons that get the row closed, and exits 1: a seat that finished and
    /// still claims it, Release and mark done and Release; one on a far
    /// machine nothing can reach, Despawn (lost), confirmed; open sub-tasks,
    /// the count and Done anyway, confirmed. A turn in progress offers none.
    /// And the finished-but-claimed row closes after its Release.
    #[test]
    fn a_refused_done_under_json_carries_its_next_steps_as_buttons() {
        let _env = util::isolated("done-json-buttons");
        let store = Store::open();
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a row", "p-001");
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        store.set_binding("cpd-9", json!({ "task_id": "p-001", "pane_id": "cpd-9", "workspace_id": "cpd-9" }));
        store.set_claim("p-001", json!({ "workspace_id": "cpd-9", "workspace_label": "p-001", "cwd": "/tmp" }));
        let json_done = |id: &str| parse(&["done", id, "--json"]);

        // Finished and still claiming it (not verified, so done does not end it).
        assert_eq!(done_with(&store, &json_done("p-001"), &|_| true, &Reads(vec![("cpd-9", State::Idle)])), 1);
        let r = refused();
        assert_eq!((r["id"].as_str(), r["done"].as_bool()), (Some("p-001"), Some(false)));
        assert!(r["reason"].as_str().unwrap().starts_with("p-001 still has a seat running"), "{r}");
        assert_eq!(labels(&r, "buttons"), ["Release and mark done", "Release"]);
        assert_eq!(r["buttons"][0], json!({ "label": "Release and mark done", "verb": "release", "args": ["p-001"],
            "sentence": "none", "then": [{ "verb": "done", "args": ["p-001"] }] }));

        // A turn in progress: refused, and nothing to press.
        assert_eq!(done_with(&store, &json_done("p-001"), &|_| true, &Reads(vec![("cpd-9", State::Working)])), 1);
        assert_eq!(refused()["buttons"], json!([]));

        // On a far machine that nobody can read: Despawn (lost).
        far(&store, "cpd-9", "mb2");
        assert_eq!(done_with(&store, &json_done("p-001"), &|_| true, &Reads(vec![])), 1);
        assert_eq!(refused()["buttons"], json!([{ "label": "Despawn (lost)", "verb": "despawn", "args": ["p-001", "--lost"],
            "sentence": "none", "confirm": true }]));

        // Its Release, and the close goes through.
        store.clear_binding("cpd-9");
        store.clear_claim("p-001");
        assert_eq!(holder_with(&store, "p-001", &|_| true, &Reads(vec![("cpd-9", State::Idle)])), crate::classify::Holder::Nobody);
        assert_eq!(done_with(&store, &json_done("p-001"), &|_| true, &Reads(vec![])), 0);
        assert_eq!(store.task("p-001").unwrap().status(), Status::Done);

        // Open sub-tasks: the count, and Done anyway.
        let mut parent = Task::new("a parent", "p-002");
        parent.set_status(Status::Review);
        store.save_task(&parent).unwrap();
        let mut child = Task::new("a piece", "p-003");
        child.parent = Some("p-002".into());
        store.save_task(&child).unwrap();
        assert_eq!(done_with(&store, &json_done("p-002"), &|_| false, &Reads(vec![])), 1);
        let r = refused();
        assert_eq!(r["open_subtasks"], 1);
        assert_eq!(r["buttons"], json!([{ "label": "Done anyway", "verb": "done", "args": ["p-002", "--force"],
            "sentence": "none", "confirm": true }]));
        assert_eq!(done_with(&store, &parse(&["done", "p-002", "--force"]), &|_| false, &Reads(vec![])), 0, "and Done anyway closes it");
    }

    /// `wsp-315`. The finished-rows group as the mirror carries it: a row
    /// whose seat finished and still claims it offers Release and mark done,
    /// one with an open sub-task its count and Done anyway, one whose far seat
    /// is lost Despawn (lost); one whose seat is in a turn is not in the group
    /// at all; Mark all done runs only where `wsp done` goes through, and every
    /// row offered also offers Send back.
    #[test]
    fn the_finished_group_in_the_mirror_carries_each_rows_next_steps_and_leaves_out_a_working_seat() {
        let _env = util::isolated("finished-group-buttons");
        let store = Store::open();
        store.ensure_dirs().unwrap();
        for (id, seat) in [("p-001", "cpd-1"), ("p-002", ""), ("p-003", "cpd-3"), ("p-004", "cpd-4"), ("p-005", "")] {
            let mut t = Task::new("a row", id);
            t.set_status(Status::Review);
            let mut p = crate::verification::Pass::opened(Some("abc1234".into()), None);
            p.at = "2026-11-20T09:00:00Z".into();
            p.state = crate::verification::State::Holds;
            p.decided = Some("2026-11-20T09:30:00Z".into());
            crate::verification::write(&mut t, &[p]);
            store.save_task(&t).unwrap();
            if !seat.is_empty() {
                store.set_binding(seat, json!({ "task_id": id, "pane_id": seat, "workspace_id": seat }));
                store.set_claim(id, json!({ "workspace_id": seat, "workspace_label": id, "cwd": "/tmp" }));
            }
        }
        let mut child = Task::new("a piece", "p-006");
        child.parent = Some("p-002".into());
        store.save_task(&child).unwrap();
        far(&store, "cpd-3", "mb2");
        let reads = Reads(vec![("cpd-1", State::Idle), ("cpd-4", State::Working)]);
        let tasks = store.tasks();
        let holders: BTreeMap<String, crate::classify::Holder> =
            ["p-001", "p-002", "p-003", "p-004", "p-005"].iter().map(|id| (id.to_string(), holder_with(&store, id, &|_| true, &reads))).collect();
        let fields: BTreeMap<String, BTreeMap<String, String>> =
            crate::mirror::rows(&tasks, &store.claims(), &[], &BTreeMap::new(), &BTreeMap::new(), &holders)
                .into_iter()
                .filter_map(|e| match e {
                    wsp_store_contract::Entity::Row(r) => Some((r.id.0.clone(), r.fields.clone())),
                    _ => None,
                })
                .collect();
        let action = |id: &str| -> serde_json::Value { serde_json::from_str(&fields[id]["action"]).unwrap() };
        assert_eq!(labels(&action("p-001"), "buttons"), ["Release and mark done", "Release", "Send back"]);
        let p2 = action("p-002");
        assert_eq!((labels(&p2, "buttons"), &p2["open_subtasks"]), (vec!["Done anyway".to_string(), "Send back".into()], &json!(1)));
        assert_eq!(p2["buttons"][0]["confirm"], true);
        assert_eq!(labels(&action("p-003"), "buttons"), ["Despawn (lost)", "Send back"]);
        assert_eq!(labels(&action("p-005"), "buttons"), ["Mark done", "Send back"]);
        assert!(!fields["p-004"].contains_key("attention") && !fields["p-004"].contains_key("action"), "a working seat's row is left out");

        let group = fields.values().find_map(|f| f.get("group_action")).expect("one row carries the group");
        let group: serde_json::Value = serde_json::from_str(group).unwrap();
        let members: Vec<&str> = group["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
        assert_eq!(members, ["p-001", "p-002", "p-003", "p-005"]);
        assert_eq!(group["buttons"], json!([{ "label": "Mark all done", "verb": "done", "args": [], "sentence": "none", "each": [["p-005"]] }]));
    }

    /// `wsp-315`. Send back is `wsp reopen <id> "<what is owed>"`: pressed
    /// with the person's words, the row goes back to doing and the next
    /// seat's packet opens with those words.
    #[test]
    fn send_back_reopens_the_row_with_the_persons_words_where_the_next_seat_reads_them() {
        let store = scratch("sendbackbutton");
        released(&store, "m-1", "claimed by pane cpd-1306 · spawned at sonnet/high");
        let mut t = store.find_task("m-1").unwrap();
        crate::model::set_section_in(&mut t.body, "Overview", "Wire the first half.");
        crate::model::set_section_in(&mut t.body, crate::packet::SECTION, "Wire the first half.");
        store.save_task(&t).unwrap();
        let row = crate::classify::Row {
            id: "m-1",
            review: true,
            verified: true,
            owes: false,
            holder: crate::classify::Holder::Nobody,
            open: 0,
            run_closes: false,
        };
        let a = crate::classify::of(crate::classify::Item::Row(row)).unwrap().action.unwrap();
        let back = a.buttons.iter().find(|b| b.label == "Send back").expect("Send back offered");
        assert_eq!((back.verb.as_str(), back.sentence), ("reopen", crate::classify::Sentence::Needed));

        let words = "the empty state draws nothing — owed: a line saying why";
        let mut argv: Vec<&str> = back.args.iter().map(String::as_str).collect();
        argv.push(words);
        assert_eq!(reopen(&store, &Args::synth(&back.verb, &argv, &[])), 0);
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Doing);
        assert!(t.section("Log").unwrap_or_default().contains(&format!("sent back: {words}")));
        let out = crate::packet::build(&t, &std::env::temp_dir()).unwrap();
        assert!(out.find(words).expect(&out) < out.find("Wire the first half.").unwrap(), "the next seat reads the words first: {out}");
    }

    /// What each seat reads, for `wsp done`'s judgement (`wsp-344`).
    pub struct Reads(pub Vec<(&'static str, State)>);

    impl crate::cycle::Seats for Reads {
        fn state(&self, seat: &str) -> Option<State> {
            self.0.iter().find(|(s, _)| *s == seat).map(|(_, st)| *st)
        }
    }

    /// A row at `review` whose verifier holds, claimed and bound on `cpd-9`.
    pub fn verified_row(store: &Store) {
        let mut t = Task::new("a row", "p-001");
        t.set_status(Status::Review);
        let mut p = crate::verification::Pass::opened(Some("abc1234".into()), None);
        p.at = "2026-11-20T09:00:00Z".into();
        p.pane = Some("cpd-2".into());
        p.state = crate::verification::State::Holds;
        p.decided = Some("2026-11-20T09:30:00Z".into());
        crate::verification::write(&mut t, &[p]);
        store.save_task(&t).unwrap();
        store.set_binding("cpd-9", json!({ "task_id": "p-001", "pane_id": "cpd-9", "workspace_id": "cpd-9" }));
        store.set_claim("p-001", json!({ "workspace_id": "cpd-9", "workspace_label": "p-001", "cwd": "/tmp" }));
        crate::cycle::tests::ENDED.with(|e| e.borrow_mut().clear());
    }

    /// `wsp-509`: a real despawn runs in its own process, and its release
    /// writes "released after …" on the row's log while `wsp done` waits on
    /// it. Done's own save reads the row after the despawn, so that line stays.
    #[test]
    fn done_keeps_the_line_the_despawn_writes_on_the_row() {
        let _env = util::isolated("done-verified-despawn-log");
        let store = Store::open();
        store.ensure_dirs().unwrap();
        verified_row(&store);
        crate::cycle::tests::ON_END.with(|h| {
            *h.borrow_mut() = Some(Box::new(|seat: &str| {
                // Another process: its own store handle, its own read and save.
                let other = Store::open();
                let mut t = other.task("p-001").unwrap();
                t.log(&format!("released after 12m — {seat} ended"));
                other.save_task(&t).unwrap();
            }));
        });

        let code = done_with(&store, &parse(&["done", "p-001"]), &|_| true, &Reads(vec![("cpd-9", State::Idle)]));
        crate::cycle::tests::ON_END.with(|h| *h.borrow_mut() = None);
        assert_eq!(code, 0);
        let t = store.task("p-001").unwrap();
        assert_eq!(t.status(), Status::Done);
        let log = t.section("Log").unwrap_or_default();
        assert!(log.contains("released after 12m — cpd-9 ended"), "done's save dropped the despawn's line: {log}");
        assert!(log.contains("`wsp done` ended it and released the claim") && log.contains("→ done"), "{log}");
    }

    /// A seat no backend has: `state` reads `None`, `absent` reads true.
    pub struct Absent;

    impl crate::cycle::Seats for Absent {
        fn state(&self, _: &str) -> Option<State> {
            None
        }
        fn absent(&self, _: &str) -> bool {
            true
        }
    }

    /// `wsp-344`: a turn in progress still refuses the close, verified or not,
    /// and nothing is ended, released or closed.
    #[test]
    fn done_still_refuses_a_verified_row_while_a_turn_runs() {
        let _env = util::isolated("done-verified-working");
        let store = Store::open();
        store.ensure_dirs().unwrap();
        verified_row(&store);

        for st in [State::Working, State::Blocked, State::Starting] {
            assert_eq!(done_with(&store, &parse(&["done", "p-001"]), &|_| true, &Reads(vec![("cpd-9", st)])), 1, "{st:?}");
            assert_eq!(store.task("p-001").unwrap().status(), Status::Review, "{st:?}: nothing closed");
            assert!(store.claims().contains_key("p-001"), "{st:?}: nothing released");
            assert!(store.bindings().contains_key("cpd-9"));
            assert!(crate::cycle::tests::ENDED.with(|e| e.borrow().is_empty()), "{st:?}: nothing ended");
        }
    }

    /// `wsp-345`: `wsp add --from` files a follow-up, recording `found_by` and
    /// no parent, and `wsp done` closes its origin while it is open; a sub-task
    /// (`--parent`) still holds its parent until it is finished. Both ends say
    /// so in `show --json`, and `--parent` with `--from` is refused.
    #[test]
    fn a_follow_up_records_where_it_came_from_and_does_not_hold_it_open_and_a_sub_task_does() {
        let _env = util::isolated("follow-up-from");
        let store = Store::open();
        store.ensure_dirs().unwrap();
        let mut origin = Task::new("the row being worked", "p-001");
        origin.set_status(Status::Doing);
        store.save_task(&origin).unwrap();
        let filed = |s: &Store, title: &str| s.tasks().into_iter().find(|t| t.title == title).expect(title);

        assert_eq!(add_from(&store, &parse(&["add", "found while doing it", "--from", "p-001"]), None), 0);
        let found = filed(&store, "found while doing it");
        assert_eq!((found.found_by.as_deref(), found.parent.as_deref()), (Some("p-001"), None), "found_by, never a parent");
        assert_eq!(found.project, origin.project, "filed where it was found");
        assert_eq!(add_from(&store, &parse(&["add", "both", "--from", "p-001", "--parent", "p-001"]), None), 2);
        assert_eq!(add_from(&store, &parse(&["add", "from nothing", "--from", "p-404"]), None), 1);
        assert!(store.tasks().iter().all(|t| t.title != "both" && t.title != "from nothing"), "a refusal files nothing");

        let shown = show_json(&store, "p-001").unwrap();
        assert_eq!(shown["follow_ups"][0]["id"], found.id.as_str(), "{shown}");
        assert_eq!(show_json(&store, &found.id).unwrap()["found_by"], "p-001");

        // A sub-task is part of the row's done-when: `done` refuses while it is open.
        assert_eq!(add_from(&store, &parse(&["add", "a piece of it", "--parent", "p-001"]), None), 0);
        let piece = filed(&store, "a piece of it");
        assert_eq!(done_with(&store, &parse(&["done", "p-001"]), &|_| false, &Reads(vec![])), 1);
        assert_eq!(store.task("p-001").unwrap().status(), Status::Doing);

        // The sub-task finished, the open follow-up does not stop the close.
        assert_eq!(done_with(&store, &parse(&["done", &piece.id]), &|_| false, &Reads(vec![])), 0);
        assert_eq!(done_with(&store, &parse(&["done", "p-001"]), &|_| false, &Reads(vec![])), 0);
        assert_eq!(store.task("p-001").unwrap().status(), Status::Done);
        assert!(store.task(&found.id).unwrap().status().is_open(), "the follow-up is still open, and held nothing");

        // And it is now a follow-up of a closed row on no list and under no
        // seat: what `wsp ls` and `wsp wip` name.
        let unheld = crate::follow_up::unheld(&store);
        assert_eq!(unheld.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(), vec![found.id.as_str()]);
    }

    pub fn scratch_file(name: &str, text: &str) -> String {
        let p = std::env::temp_dir().join(format!("wsp-{name}-{}.md", std::process::id()));
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().to_string()
    }

    pub fn log_of(store: &Store, id: &str) -> String {
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

    /// The verbs whose prose is a *question about the code*, and therefore the
    /// verbs whose prose is full of backticks.
    ///
    /// `block` and `park` were argv-only until this. Neither is long by nature,
    /// which is why they were left out the first time — but length was never
    /// what made the shell dangerous. A blocking question names the two shapes
    /// it is choosing between, and naming a shape in this store means naming a
    /// verb, and a verb between double quotes inside backticks is a command
    /// substitution. See [`crate::util::terminal_output`].
    #[test]
    fn a_block_and_a_park_take_their_reason_from_a_stream_too() {
        let store = scratch("block-stream");
        task_with(&store, "t-260815-047", "normal");
        task_with(&store, "t-260815-048", "normal");

        let q = scratch_file("question", "Does `wsp verify --alone` mean the tree, or HEAD?\n");
        assert_eq!(block(&store, &parse(&["block", "047", "--from", &q])), 0);
        let log = log_of(&store, "047");
        assert!(log.contains("blocked: Does `wsp verify --alone`"), "{log}");
        assert_eq!(store.find_task("047").unwrap().status(), Status::Blocked);

        let when = scratch_file("when", "When `place_super` stops being the only backend.\n");
        assert_eq!(park(&store, &parse(&["park", "048", "--from", &when])), 0);
        let log = log_of(&store, "048");
        assert!(log.contains("parked: When `place_super`"), "{log}");
        assert_eq!(store.find_task("048").unwrap().status(), Status::Parked);

        // And the dash that used to be recorded as the whole reason.
        assert_eq!(payload_source(&["-".to_string()]), Some("-".into()));
    }

    /// The second occurrence, and the one that made this a data-integrity fault
    /// rather than a nuisance: a decision replaced in the store by the progress
    /// bar of the test run its own prose caused to execute.
    ///
    /// Asserted through `decide` and through a file rather than through a shell,
    /// because what a shell would hand over is exactly this — a terminal's bytes
    /// where a paragraph was meant. Both doors are checked: the one the
    /// substitution comes through (argv) and the one an agent pipes a log into
    /// by mistake (a stream).
    #[test]
    fn a_captured_terminal_is_refused_at_both_doors() {
        let store = scratch("decide-capture");
        task_with(&store, "t-260815-047", "normal");

        let captured = "verifying HEAD itself \u{1b}[2mrunning 745 tests\u{1b}[0m\ralone 87/745  ";
        assert_eq!(decide(&store, &parse(&["decide", "047", captured])), 2, "argv");
        // 1 rather than 2 on this one, and deliberately: the stream door is
        // the single place every source comes through, so this arrives as a
        // source that could not be read, which is already an exit 1.
        let path = scratch_file("captured", captured);
        assert_eq!(decide(&store, &parse(&["decide", "047", "--from", &path])), 1, "a stream");

        let body = store.find_task("047").unwrap().body;
        assert!(!body.contains("745"), "nothing of it may reach the store: {body}");

        // And the prose it is protecting is not itself refused. Newlines and
        // tabs are what a paragraph is made of.
        assert_eq!(decide(&store, &parse(&["decide", "047", "One shape.\n\tThen the other."])), 0);
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

    /// The whole of what this cap is for, and the thing it must not do.
    ///
    /// `wsp show` is the command a seat runs all day and the log was the one
    /// section with no limit on it: 50.1% of every token of prose in this store,
    /// and the only part that grows without bound. It is now the recent entries
    /// — and a log cut short must say so, or six entries read exactly like a
    /// task that only ever had six, which is the history somebody is about to
    /// ask an agent to re-derive.
    #[test]
    fn a_log_longer_than_the_cap_is_cut_from_the_top_and_says_how_much_it_cut() {
        let p = Paint::plain();
        let log: String = (1..=9).map(|i| format!("- 2026-08-19 entry {i}\n")).collect();

        let out = log_lines(&p, &log, "core-008", false);
        assert_eq!(out.len(), MAX_LOG + 1, "the cap, and the line that owns up to it: {out:?}");
        assert_eq!(
            out[0], "3 of 9 earlier · wsp show core-008 --log",
            "how many are missing, and the one keystroke that prints them",
        );
        assert_eq!(out[1], "- 2026-08-19 entry 4", "kept from the recent end");
        assert_eq!(out[MAX_LOG], "- 2026-08-19 entry 9", "up to and including the last");

        assert_eq!(
            log_lines(&p, &log, "core-008", true).len(),
            9,
            "`--log` is the whole of it, and nothing here ever removed an entry from the record",
        );
    }

    /// A cap that fires on every task is a cap somebody turns off. Under it, the
    /// page is the same page it always was — no notice line, nothing dropped —
    /// which is most reads: 119 of the 156 open tasks in this store.
    #[test]
    fn a_log_inside_the_cap_prints_whole_and_says_nothing() {
        let p = Paint::plain();
        let log = "- 2026-08-19 filed\n- 2026-08-19 claimed by pane w4V:p1\n";
        assert_eq!(
            log_lines(&p, log, "core-008", false),
            vec!["- 2026-08-19 filed", "- 2026-08-19 claimed by pane w4V:p1"],
            "nothing to say, so nothing said",
        );
    }

    /// A cap that fires for no gain teaches a reader to type `--log` past it.
    /// The line announcing a single dropped entry is longer than the entry —
    /// `→ review`, `claimed by pane w4V:p1` — and costs a second command on top,
    /// so one over the cap is printed instead of being announced.
    #[test]
    fn a_log_one_entry_over_the_cap_prints_that_entry_rather_than_a_line_about_it() {
        let p = Paint::plain();
        let log: String = (1..=MAX_LOG + 1).map(|i| format!("- 2026-08-19 entry {i}\n")).collect();

        let out = log_lines(&p, &log, "core-008", false);
        assert_eq!(out.len(), MAX_LOG + 1, "seven entries, and no notice among them: {out:?}");
        assert_eq!(out[0], "- 2026-08-19 entry 1", "the oldest is still on the page");
    }

    /// An entry is a bullet and everything under it. 287 lines in this store are
    /// the continuation of the one above — an indented block of measured output,
    /// a paragraph somebody wrote by hand — and a tail counted in lines would
    /// start in the middle of one and print a fragment with no date on it,
    /// under a count that had already been told a lie about how many there were.
    #[test]
    fn an_entry_is_a_bullet_and_the_lines_beneath_it() {
        let p = Paint::plain();
        let log = "- 2026-08-19 one\n- 2026-08-19 two\n    wsp tag verb-001 +dsp\n    exit 0\n- 2026-08-19 three\n";

        let entries = log_entries(log);
        assert_eq!(entries.len(), 3, "three entries, not five lines: {entries:?}");
        assert!(entries[1].ends_with("    exit 0"), "the block belongs to the bullet above it");

        let out = log_lines(&p, log, "core-008", false);
        assert_eq!(out.len(), 3, "and under the cap it is untouched: {out:?}");
    }

    /// Prose that arrived in a shape nothing anticipated is counted rather than
    /// dropped. `wsp doctor` folds stray headings back into the log, and what it
    /// folds does not always begin with a bullet.
    #[test]
    fn text_before_the_first_bullet_is_an_entry_of_its_own() {
        let log = "a line somebody wrote without a bullet\n- 2026-08-19 filed\n";
        assert_eq!(log_entries(log).len(), 2, "counted, so `--log` is what prints it");
    }

    /// `wsp-311`, compound-758's shape: the seat working a member at doing files
    /// a sub-task under it. Refused, nothing filed, and the refusal names legs;
    /// `--anyway` files it. The same from a seat not bound to the member, and
    /// under the member once it is at review, files it with nothing said.
    /// `wsp-538`: from a seat, `add` does not file a row requiring the full suite.
    #[test]
    fn add_from_a_seat_refuses_a_row_that_requires_the_full_workspace_run() {
        let store = scratch("add-full-suite");
        let ask = Args::synth("add", &["Fix it; done when `cargo nextest run --workspace` runs to its end"], &[("inbox", "true")]);
        assert_eq!(add_from(&store, &ask, Some("cpd-1")), 1, "refused from a seat");
        assert!(store.tasks().is_empty(), "and nothing was filed");
        let scoped = Args::synth("add", &["Fix it; done when `cargo nextest run -E 'test(=a::b)'` passes"], &[("inbox", "true")]);
        assert_eq!(add_from(&store, &scoped, Some("cpd-1")), 0);
        assert_eq!(add_from(&store, &ask, None), 0, "a person may write one");
    }

    #[test]
    fn add_parent_from_the_seat_working_a_member_is_refused_without_anyway() {
        let store = scratch("add-parent-member");
        let mut m = Task::new("Build the four pieces", "m-1");
        m.set_status(Status::Doing);
        crate::model::set_section_in(&mut m.body, "Overview", "Four pieces.\n\nDone when: all four land.");
        store.save_task(&m).unwrap();
        let mut w = crate::model::Worklist::new("tweaks-next", "tweaks-next");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["m-1".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        let children = |s: &Store| s.tasks().into_iter().filter(|t| t.parent.as_deref() == Some("m-1")).count();

        let piece = |anyway: bool| {
            let mut flags = vec![("parent", "m-1")];
            if anyway {
                flags.push(("anyway", "true"));
            }
            Args::synth("add", &["The Review tab"], &flags)
        };
        assert_eq!(add_from(&store, &piece(false), Some("cpd-1")), 1, "refused from the member's own seat");
        assert_eq!(children(&store), 0, "and nothing was filed");
        assert_eq!(add_from(&store, &piece(true), Some("cpd-1")), 0, "--anyway files it");
        assert_eq!(children(&store), 1);
        assert_eq!(add_from(&store, &piece(false), Some("cpd-9")), 0, "a seat not working the member is not refused");
        assert_eq!(add_from(&store, &piece(false), None), 0, "nor a person");
        let mut m = store.find_task("m-1").unwrap();
        m.set_status(Status::Review);
        store.save_task(&m).unwrap();
        assert_eq!(add_from(&store, &piece(false), Some("cpd-1")), 0, "a follow-up under a member at review is not refused");
        assert_eq!(children(&store), 4);
    }

    /// `wsp-163`, and the whole of it in one verb. Sending work back and moving
    /// the row were two acts, and a `wsp tell` is the one nobody remembers: the
    /// member landed, its verifier was told to install and drive delivery live,
    /// the row stayed at `review`, and the run read the group as finished and
    /// opened a barrier over outstanding work.
    #[test]
    fn sending_work_back_moves_the_row_records_the_reason_and_tells_the_pane() {
        let store = scratch("reopenverb");
        let mut t = Task::new("m-1", "m-1");
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));

        let args = Args::synth("reopen", &["m-1", "install and drive delivery live"], &[]);
        assert_eq!(reopen(&store, &args), 0);

        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Doing, "the row moved, which is what the run reads");
        let log = t.section("Log").unwrap_or_default();
        assert!(log.contains("install and drive delivery live"), "{log}");
        let told = crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().drain(..).collect::<Vec<_>>());
        assert_eq!(told.len(), 1, "and the reason reached the pane: {told:?}");
        assert_eq!(told[0].0, "m-1");
        assert!(told[0].1.contains("install and drive delivery live"), "{}", told[0].1);
    }

    /// A reopen with no reason would put work back into a group with nothing to
    /// act on and nothing to read at the other end — which is the state this
    /// verb exists to end. It is refused rather than defaulted.
    #[test]
    fn sending_work_back_with_no_reason_is_refused_and_moves_nothing() {
        let store = scratch("reopennoreason");
        let mut t = Task::new("m-1", "m-1");
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();

        let args = Args::synth("reopen", &["m-1"], &[]);
        assert_eq!(reopen(&store, &args), 2, "usage, and the row is where it was");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Review);
    }

    /// A row nobody is holding has no pane, and saying so on every reopen of
    /// finished work would be noise.
    #[test]
    fn a_reopen_of_a_row_with_no_pane_moves_the_row_and_says_nothing_to_anyone() {
        let store = scratch("reopennopane");
        let mut t = Task::new("m-1", "m-1");
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();

        let args = Args::synth("reopen", &["m-1", "not finished after all"], &[]);
        assert_eq!(reopen(&store, &args), 0);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing);
        assert!(crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow().is_empty()));
        assert!(reseated().is_empty(), "no agent ever claimed it, so none is guessed at");
    }

    /// wsp-303's done-when rested on `wsp mv`. Its review says so in one line,
    /// however many items or verbs; the overview before a done-when, and a
    /// done-when that needs no refused verb, say nothing (`wsp-312`).
    #[test]
    fn a_done_when_that_needs_a_refused_verb_is_warned_about_once_at_review() {
        let mut t = Task::new("detach the spec rows", "wsp-303");
        t.body = "## Overview\nThe rows were filed with `wsp add --parent`.\n\n## Done when\n- `wsp mv <id> --parent none` ran for each of the 86\n- wsp mv left wsp-302 alone\n".into();
        let said = done_when_refused(&t).expect("warned");
        assert!(!said.contains('\n') && said.contains("needs `wsp mv`,") && !said.contains("`wsp add --parent"), "{said}");
        // The same done-when inside the overview, as most rows write it.
        t.body = "## Overview\nThe rows were filed with `wsp add --parent`.\n\n### Done when\n- the 86 are detached with `wsp mv`\n".into();
        let said = done_when_refused(&t).expect("warned");
        assert!(said.contains("needs `wsp mv`,") && !said.contains("`wsp add --parent"), "{said}");
        t.body = "## Overview\nUse `wsp mv` if you must.\n\n## Done when\n`cargo test packet::` passes.\n".into();
        assert_eq!(done_when_refused(&t), None);
    }

    /// wsp-154's overview told its own seat to `wsp add --parent`, and wsp-303's
    /// barrier order told its seat to `wsp mv`. An edit that writes an overview
    /// like that says which verb, and what the seat does instead; one that writes
    /// another section, a project, or an overview with nothing refused says
    /// nothing (`wsp-312`).
    #[test]
    fn an_overview_that_tells_its_seat_a_refused_verb_is_said_when_it_is_written() {
        let body = "## Overview\n2. Sub-tasks of this row (`wsp add --parent`).\n- `wsp mv <id> --parent none` for each of the 86.\n\n## Details\nx\n";
        let said = overview_names_refused("task", "wsp-154", Some("Overview"), body);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].contains("`wsp add --parent`") && said[0].contains("wsp leg add wsp-154"), "{}", said[0]);
        assert!(said[1].contains("`wsp mv`") && said[1].contains("`wsp ask wsp-154 --person`"), "{}", said[1]);
        assert_eq!(overview_names_refused("task", "wsp-154", None, body).len(), 2, "the whole body reaches Overview");
        assert!(overview_names_refused("task", "wsp-154", Some("Details"), body).is_empty());
        assert!(overview_names_refused("project", "wsp", None, body).is_empty());
        assert_eq!(overview_names_refused("worklist", "spec-breakdown", None, body).len(), 2);
        assert!(overview_names_refused("task", "wsp-154", Some("Overview"), "## Overview\nMake a.\n").is_empty());
    }

    /// `compounding-220`: the packet generated for the second seat of a
    /// reopened row starts with the reopen's reason, before the original
    /// overview. compounding-189's second seat re-reported a landed commit
    /// because its packet carried only the first brief.
    #[test]
    fn the_packet_of_a_reopened_rows_second_seat_opens_with_the_reopen_reason() {
        let store = scratch("reopenpacket");
        released(&store, "m-1", "claimed by pane cpd-1306 · spawned at sonnet/high");
        let mut t = store.find_task("m-1").unwrap();
        crate::model::set_section_in(&mut t.body, "Overview", "Wire the first half.");
        crate::model::set_section_in(&mut t.body, crate::packet::SECTION, "Wire the first half.");
        store.save_task(&t).unwrap();

        assert_eq!(reopen(&store, &Args::synth("reopen", &["m-1", "the second half is what is owed"], &[])), 0);
        assert_eq!(reseated().len(), 1, "a second seat is started");

        let t = store.find_task("m-1").unwrap();
        let out = crate::packet::build(&t, &std::env::temp_dir()).unwrap();
        let (owed, original) = (out.find("the second half is what is owed").expect(&out), out.find("Wire the first half.").unwrap());
        assert!(owed < original, "the reason comes first: {out}");
        assert!(crate::packet::at_spawn(&store, Some("m-1"), true, false, false, true).is_some_and(|n| n as usize == out.len()));
    }

    /// An opencode model has a `/` of its own, and an unstated half stays
    /// unstated; with no session learned the kind is the default.
    #[test]
    fn the_tier_splits_at_the_last_slash_and_default_is_left_unsaid() {
        let store = scratch("reopenreseattier");
        released(&store, "m-1", "claimed by pane w2:p1 · spawned at opencode/x-free/default");

        assert_eq!(reopen(&store, &Args::synth("reopen", &["m-1", "again"], &[])), 0);
        let kind = crate::agent_kind::kind_or_default("");
        assert_eq!(reseated(), [vec!["spawn", "m-1", "--agent", "--kind", kind.as_str(), "--model", "opencode/x-free"]]);
    }

    /// The agent still in its seat was just told what is owed: a second one
    /// would be two agents in one tree. And `--no-seat` asks for nobody.
    #[test]
    fn a_reopen_seats_nobody_beside_a_bound_agent_or_when_told_not_to() {
        let store = scratch("reopenreseatheld");
        released(&store, "m-1", "claimed by pane cpd-1 · spawned at opus/high");
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        assert_eq!(reopen(&store, &Args::synth("reopen", &["m-1", "one more thing"], &[])), 0);
        assert!(reseated().is_empty(), "its agent is there and was told");
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().clear());

        released(&store, "m-2", "claimed by pane cpd-2 · spawned at opus/high");
        assert_eq!(reopen(&store, &Args::synth("reopen", &["m-2", "later"], &[("no-seat", "true")])), 0);
        assert!(reseated().is_empty(), "--no-seat leaves it to whoever sent it back");
        assert_eq!(store.find_task("m-2").unwrap().status(), Status::Doing);
    }

    const REBASE_FAILED: &str = "wsp: the rebase onto master does not apply cleanly\ngit said: Rebasing (1/41)\r\
        Rebasing (2/41)\rRebasing (3/41)\rerror: could not apply 17079a4... the change\nhint: Resolve all conflicts manually";

    /// A member of a running list with `cpd-1` bound, sent back for a rebase
    /// whose stderr holds git's progress line, as the run does it.
    fn sent_back_for_a_rebase(tag: &str) -> Store {
        let store = scratch(tag);
        released(&store, "m-1", "claimed by pane cpd-1 · spawned at opus/high");
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        crate::repair::tests::list(&store, &["m-1"], "claude");
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().clear());
        crate::cycle::tests::TOLD.with(|t| t.borrow_mut().clear());
        let mut t = store.find_task("m-1").unwrap();
        crate::verification::send_back(&mut t, util::now_iso(), "wsp's rebase", None, REBASE_FAILED, None);
        store.save_task(&t).unwrap();
        store
    }

    /// `wsp-846`: the seat is told, without the carriage returns `wsp tell` refuses.
    #[test]
    fn a_send_back_for_a_failed_rebase_reaches_the_seat_with_no_carriage_return() {
        let store = sent_back_for_a_rebase("rebasetold");
        crate::verification::deliver_unless_reworked(&store, "m-1", REBASE_FAILED);
        let told = crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().drain(..).collect::<Vec<_>>());
        assert_eq!(told.len(), 1, "{told:?}");
        let line = &told[0].1;
        assert!(line.contains("error: could not apply 17079a4") && line.contains("sent m-1 back"), "{line}");
        assert!(!line.contains('\r') && !line.contains("Rebasing ("), "{line:?}");
        let t = store.find_task("m-1").unwrap();
        let log = t.section("Log").unwrap();
        assert!(log.contains("could not apply") && !log.contains("Rebasing ("), "{log}");
        assert!(crate::repair::undelivered_reason(&t).is_none(), "nothing is owed");
    }

    /// `wsp-846`: a sentence the prose check still refuses goes to the governor
    /// once, and is not left pending for the seat's next idle.
    #[test]
    fn a_tell_the_prose_check_refuses_is_reported_to_the_governor_once_and_not_left_pending() {
        let store = sent_back_for_a_rebase("rebaserefused");
        crate::cycle::tests::TELL_PROSE_REFUSED.with(|r| r.set(true));
        crate::verification::deliver_unless_reworked(&store, "m-1", REBASE_FAILED);
        crate::cycle::tests::TELL_PROSE_REFUSED.with(|r| r.set(false));
        let governed = crate::cycle::tests::TOLD.with(|t| t.borrow_mut().drain(..).collect::<Vec<_>>());
        assert_eq!(governed.iter().filter(|l| l.contains("m-1") && l.contains("NOT told")).count(), 1, "{governed:?}");
        assert!(crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow().is_empty()));
        let t = store.find_task("m-1").unwrap();
        assert!(crate::repair::undelivered_reason(&t).is_none(), "{:?}", t.section("Log"));
    }
}
