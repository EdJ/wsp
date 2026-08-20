//! Project commands: the hierarchy and its tags.

use serde_json::json;

use crate::model::Project;
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// What a store *is* when it is new, without saying so.
///
/// `wsp init` is the command; `wsp sandbox` needs the same store made somewhere
/// else and has its own thing to print. A sandbox whose store differed from a
/// real one in any particular would be answering a question about itself rather
/// than about wsp, so there is one implementation of "new store" and both call
/// it.
pub fn init_store(store: &Store) -> std::io::Result<()> {
    store.ensure_dirs()?;
    store.git_init();

    let readme = store.root.join("README.md");
    if !readme.exists() {
        let _ = crate::store::write_atomic(
            &readme,
            "# wsp store\n\nProjects in `projects/`, tasks in `tasks/`, one file each.\n\
             Mutate through the `wsp` CLI — it owns id allocation, atomic writes and commits.\n",
        );
    }
    let _ = std::fs::create_dir_all(store.root.join("hooks"));

    // The whole store, and the one command entitled to it: what `init` is
    // about is the store itself, not a file in it.
    store.git_commit_all("wsp: init store");
    Ok(())
}

pub fn init(store: &Store, args: &Args) -> i32 {
    if let Err(e) = init_store(store) {
        eprintln!("wsp: cannot create {}: {e}", util::contract(&store.root));
        return 1;
    }

    if args.json() {
        println!("{}", json!({ "root": util::contract(&store.root), "state": util::contract(&store.state) }));
    } else {
        println!("store   {}", util::contract(&store.root));
        println!("state   {}", util::contract(&store.state));
        println!("\nNext: wsp project add <slug> --name \"Name\" --root ~/path");
    }
    0
}

pub fn dispatch(store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(|s| s.as_str()).unwrap_or("ls") {
        "add" | "new" => add(store, args),
        "ls" | "list" => list(store, args),
        "tree" => tree(store, args),
        "show" | "get" => show(store, args),
        "set" => set(store, args),
        "rm" | "remove" | "delete" => rm(store, args),
        "edit" => edit(store, args),
        other => {
            eprintln!("wsp project: unknown subcommand `{other}`");
            2
        }
    }
}

pub fn add(store: &Store, args: &Args) -> i32 {
    let Some(slug_raw) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp project add <slug> [--name N] [--parent P] [--tag T]… [--root PATH]…");
        return 2;
    };
    let slug = util::slugify(&slug_raw);
    if slug.is_empty() {
        eprintln!("wsp: `{slug_raw}` does not reduce to a usable slug");
        return 2;
    }
    if store.project(&slug).is_some() {
        eprintln!("wsp: project `{slug}` already exists");
        return 1;
    }
    // The other half of the scope key space, and it has to be checked here as
    // well as in `Store::scope_taken` or it only holds in whichever direction
    // was written second. A worklist slug and a project id are one name
    // because `governors.json` is keyed on it and a running worklist takes a
    // seat of its own; two things answering to one key would route a raised
    // hand to whichever the map happened to hold.
    if store.worklist(&slug).is_some() {
        eprintln!("wsp: `{slug}` is not free: worklist `{slug}` uses it");
        return 1;
    }

    let index = Index::new(store.projects());
    let mut p = Project::new(&slug);
    p.name = args.get("name").unwrap_or_else(|| slug_raw.clone());
    p.tags = args.all("tag");
    p.roots = args.all("root").iter().map(|r| util::contract(&util::expand(r))).collect();
    p.brief = args.get("brief").unwrap_or_default();
    // One frontmatter line, so no `-` form — `wsp project edit <slug>` is where
    // a project's prose is written. The check is here because this one is
    // frontmatter: a control byte on a `brief:` line is not a bad-looking
    // brief, it is a project file the parser reads differently.
    if let Some(why) = util::terminal_output(&p.brief) {
        eprintln!("wsp: {why}");
        return 2;
    }
    if let Some(status) = args.get("status") {
        p.status = status;
    }
    if let Some(parent) = args.get("parent") {
        match index.find(&parent) {
            Some(found) => p.parent = Some(found.id.clone()),
            None => {
                eprintln!("wsp: no such parent project `{parent}`");
                return 1;
            }
        }
    }

    if let Err(e) = store.save_project(&p) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("project-added", json!({ "id": p.id }));
    store.git_commit(&format!("wsp: add project {}", p.id));

    if args.json() {
        println!("{}", project_json(&p, &Index::new(store.projects())));
    } else {
        let parent = p.parent.clone().unwrap_or_else(|| "—".into());
        println!("added project {} ({}), parent {}", p.id, p.name, parent);
    }
    0
}

pub fn list(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let counts = resolve::counts_by_project(&index, &store.tasks());
    let want_tag = args.get("tag");

    let mut rows: Vec<&Project> = index
        .projects
        .iter()
        .filter(|p| match &want_tag {
            Some(t) => index.effective_tags(&p.id).iter().any(|x| x == t),
            None => true,
        })
        .filter(|p| args.has("all") || p.status != "done")
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));

    if args.json() {
        let out: Vec<_> = rows.iter().map(|p| project_json(p, &index)).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }

    if rows.is_empty() {
        println!("no projects yet — wsp project add <slug>");
        return 0;
    }

    let p = Paint::new();
    let w = rows.iter().map(|r| r.id.chars().count()).max().unwrap_or(8).max(7);
    println!("{}  {}  {}", p.dim(&util::pad("PROJECT", w)), p.dim(&util::pad("OPEN", 5)), p.dim("TAGS"));
    for r in rows {
        let c = counts.get(&r.id).copied().unwrap_or_default();
        let tags = index.effective_tags(&r.id).join(" ");
        let open = if c.open == 0 { p.dim("·") } else { c.open.to_string() };
        println!(
            "{}  {}  {}",
            util::pad(&r.id, w),
            util::pad(&open, 5),
            p.dim(&util::truncate(&tags, 44))
        );
    }
    0
}

pub fn tree(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let counts = resolve::counts_by_project(&index, &store.tasks());

    if args.json() {
        let out: Vec<_> = index.projects.iter().map(|p| project_json(p, &index)).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }

    if index.projects.is_empty() {
        println!("no projects yet — wsp project add <slug>");
        return 0;
    }

    let p = Paint::new();
    fn walk(
        index: &Index,
        counts: &std::collections::BTreeMap<String, resolve::Counts>,
        parent: Option<&str>,
        prefix: &str,
        paint: &Paint,
    ) {
        let kids = index.children(parent);
        for (i, kid) in kids.iter().enumerate() {
            let last = i + 1 == kids.len();
            let branch = if last { "└── " } else { "├── " };
            let c = counts.get(&kid.id).copied().unwrap_or_default();

            let mut badge = Vec::new();
            if c.open > 0 {
                badge.push(format!("{} open", c.open));
            }
            if c.doing > 0 {
                badge.push(paint.cyan(&format!("{} doing", c.doing)));
            }
            if c.blocked > 0 {
                badge.push(paint.red(&format!("{} blocked", c.blocked)));
            }
            let badge = if badge.is_empty() {
                paint.dim("·")
            } else {
                badge.join(paint.dim(" · ").as_str())
            };

            let own_tags = if kid.tags.is_empty() {
                String::new()
            } else {
                paint.dim(&format!("  [{}]", kid.tags.join(" ")))
            };
            println!("{prefix}{branch}{}{}   {}", paint.bold(&kid.id), own_tags, badge);

            let next_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
            walk(index, counts, Some(&kid.id), &next_prefix, paint);
        }
    }

    // Roots: no parent, or a dangling parent.
    let roots = index.roots();
    for (i, root) in roots.iter().enumerate() {
        let c = counts.get(&root.id).copied().unwrap_or_default();
        let mut badge = Vec::new();
        if c.open > 0 {
            badge.push(format!("{} open", c.open));
        }
        if c.blocked > 0 {
            badge.push(p.red(&format!("{} blocked", c.blocked)));
        }
        let tags = if root.tags.is_empty() {
            String::new()
        } else {
            p.dim(&format!("  [{}]", root.tags.join(" ")))
        };
        println!("{}{}   {}", p.bold(&root.id), tags, badge.join(p.dim(" · ").as_str()));
        walk(&index, &counts, Some(&root.id), "", &p);
        if i + 1 < roots.len() {
            println!();
        }
    }

    let inbox = counts.get("").copied().unwrap_or_default();
    if inbox.open > 0 {
        println!("\n{}   {} open", p.bold("(inbox)"), inbox.open);
    }
    0
}

/// The DECISIONS block of [`show`], as lines. Split out to be asserted on: the
/// mark is the whole point of the block and printing straight to stdout would
/// have left it untestable.
///
/// An index of first sentences has one failure mode, and it is what the id
/// column is here for: a decision that a later one supersedes states the
/// *wrong* rule, with the correction some rows below and nothing joining them.
/// Read whole, the reader hits "until wsp-029 narrows it…" and knows to look;
/// abridged, they do not. So a superseded entry is struck *and* named —
/// `superseded by d4` — because the strike is invisible under `NO_COLOR` and to
/// whatever is reading this through a pipe, and the id column is what makes
/// `d4` a row you can find.
///
/// Struck rather than dropped. This is the command you come to for the record,
/// and a record with the withdrawn entries quietly gone is exactly the tidied
/// conclusion append-only exists to prevent — what a reader needs is that it
/// was decided, and that it no longer holds. The brief is the other way round
/// (`model::live_decisions`): it lists what binds now and has four lines to do
/// it in.
fn decision_lines(p: &Paint, decided: &[crate::model::Decision], full: bool, id: &str) -> Vec<String> {
    let over = crate::model::supersessions(decided);
    let mut out = Vec::with_capacity(decided.len() + 1);
    let mut cut = 0;
    for (d, superseded) in decided.iter().zip(&over) {
        let head = format!("  {}  {}", p.dim(&util::pad(&d.when, 10)), p.dim(&util::pad(&d.id, 3)));
        let text = if full {
            d.text.clone()
        } else {
            let lead = util::first_sentence(&d.text);
            if lead.len() < d.text.trim_end().len() {
                cut += 1;
            }
            util::truncate(lead, 96).to_string()
        };
        // The back-reference goes on the superseded line rather than on the one
        // doing it. A reader scanning down meets the withdrawn rule first,
        // which is the moment the warning is worth its width; on the later
        // entry it would be bookkeeping printed for every reader for ever.
        out.push(match superseded {
            Some(by) => {
                format!("{head}  {}{}", p.strike(&text), p.dim(&format!("  · superseded by {by}")))
            }
            None => format!("{head}  {text}"),
        });
    }
    // Never trim quietly: an entry cut to its first sentence reads exactly like
    // an entry that was only ever one, and the difference is the reasoning
    // somebody is about to re-derive.
    if cut > 0 {
        out.push(format!(
            "  {}  {}",
            util::pad("", 10),
            p.dim(&format!("{cut} of {} abridged · wsp project show {id} --decisions", decided.len()))
        ));
    }
    out
}

pub fn show(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp project show <id> [--decisions] [--handbook]");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(proj) = index.find(&needle).cloned() else {
        eprintln!("{}", no_project(store, &needle));
        return 1;
    };

    let tasks = store.tasks();
    let scope = index.subtree(&proj.id);
    let mut mine: Vec<_> = tasks
        .iter()
        .filter(|t| t.project.as_ref().map(|p| scope.contains(p)).unwrap_or(false))
        .filter(|t| t.status().is_open())
        .collect();
    // Raised work leads, deferred work sinks, and everything else stays in the
    // order it was filed. A stable sort by priority alone, deliberately: this
    // list is a project's backlog read top to bottom, and id order is how it
    // was written down — reordering it by status as well would be a different
    // list, and `wsp ls` is already that one.
    mine.sort_by_key(|t| t.priority().rank());

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": project_json(&proj, &index),
                "tasks": mine.iter().map(|t| t.json()).collect::<Vec<_>>(),
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    println!("{}  {}", p.bold(&proj.id), p.dim(&proj.name));
    if !proj.brief.is_empty() {
        println!("{}", proj.brief);
    }
    println!();
    println!("{}  {}", p.dim(util::pad("parent", 8).as_str()), proj.parent.clone().unwrap_or_else(|| "—".into()));
    println!("{}  {}", p.dim(util::pad("tags", 8).as_str()), index.effective_tags(&proj.id).join(" "));
    println!("{}  {}", p.dim(util::pad("roots", 8).as_str()), proj.roots.join("  "));
    println!("{}  {}", p.dim(util::pad("status", 8).as_str()), proj.status);

    let kids = index.children(Some(&proj.id));
    if !kids.is_empty() {
        let names: Vec<String> = kids.iter().map(|k| k.id.clone()).collect();
        println!("{}  {}", p.dim(util::pad("children", 8).as_str()), names.join(" "));
    }

    // Above the task list, not below the prose: a decision is a constraint on
    // what may be done next, so it has to be read before the list of things
    // somebody might pick up.
    //
    // One line each, because this is the only unbounded output wsp has. The
    // block is append-only and nothing trims it — eighteen entries landed on
    // `wsp` in a single day — and printed whole it was 3,437 of this command's
    // 4,104 tokens, three times the next most expensive thing a session can
    // run. The brief already caps its own view at four and then points here for
    // the rest, which made the escape hatch the expensive command.
    //
    // The first sentence is the rule and the rest is the argument for it, so an
    // index made of first sentences is a reader's whole question answered:
    // which of these did I mean. `--decisions` prints the block as it was
    // written, for when the answer is "that one, and I need to know why".
    //
    // What that index cannot say on its own is that a rule was withdrawn, which
    // is what the marks in `decision_lines` are for.
    let decided = crate::model::decisions_of(&proj.body);
    if !decided.is_empty() {
        println!("\n{}", p.dim("DECISIONS"));
        for line in decision_lines(&p, &decided, args.has("decisions"), &proj.id) {
            println!("{line}");
        }
    }

    // The handbook, named rather than printed — and this is the one place the
    // default is *not* "show the prose". It is written to be injected once at
    // the top of a session, where it is paid for once and re-read by the model
    // for free; `project show` is the command an agent runs several times a
    // session, and printing the same block into each of those is the exact
    // multiplication this whole line of work exists to remove. So: one line
    // saying it is there and how big, and `--handbook` for the caller who
    // actually wants to read it.
    if let Some(text) = crate::model::section_of(&proj.body, "Handbook") {
        println!("\n{}", p.dim("HANDBOOK"));
        if args.has("handbook") {
            println!("{}", text.trim_end());
        } else {
            println!(
                "  {}",
                p.dim(&format!(
                    "{} lines · wsp project show {} --handbook",
                    text.lines().count(),
                    proj.id
                ))
            );
        }
    } else if args.has("handbook") {
        // Asked for by name and not there. Silence would read as an empty
        // handbook rather than a project that has never had one written, and
        // the two want different things done about them.
        println!("\n{}", p.dim(&format!("no handbook — wsp project edit {} --handbook -", proj.id)));
    }

    if !mine.is_empty() {
        println!("\n{}", p.dim("OPEN TASKS"));
        for t in &mine {
            println!(
                "  {}  {}  {} {}",
                p.dim(&t.id),
                util::pad(t.status().as_str(), 8),
                crate::cmd_task::paint_prio(&p, t.priority()),
                util::truncate(&t.title, 58)
            );
        }
    }
    // The rest of the prose, with the decisions taken out — they are above,
    // and printing them twice would teach the reader to skip one of them.
    //
    // Not a `--terse` block, though it was written as one first. Measured
    // across five projects it saved 266 bytes on `wsp` and nothing at all on
    // `strata`, `strata-prototype`, `tooling` or `meta`, none of which carry an
    // Overview or Details — so the flag would have done nothing on four out of
    // five things you point it at, which is how a flag stops being believed.
    // What was expensive here was the decisions block, and abridging it took
    // 4,104 tokens to 1,058 for every caller rather than the ones who ask.
    let mut rest = crate::model::localise_dates(&proj.body);
    crate::model::set_section_in(&mut rest, "Decisions", "");
    crate::model::set_section_in(&mut rest, "Handbook", "");
    if !rest.trim().is_empty() {
        println!("\n{}", rest.trim());
    }
    0
}

/// What to say when a project is not there.
///
/// A removal now files the project in the archive instead of deleting it, and
/// the person typing `wsp project show batch` a month later is usually typing
/// it *because* it is gone from every list. "no such project" is then a true
/// sentence that sends them away from a file that is sitting right there with
/// the handbook in it, so the archive is checked before the answer is given.
fn no_project(store: &Store, needle: &str) -> String {
    // Through `Index` rather than by comparing slugs here, so a name, an id or
    // a unique prefix finds the archived project on exactly the terms it would
    // have found the live one.
    let archived = store.archived_projects();
    let index = Index::new(archived.iter().map(|(p, _)| p.clone()).collect());
    let Some(found) = index.find(needle) else {
        return format!("wsp: no project matching `{needle}`");
    };
    let path = archived
        .iter()
        .find(|(p, _)| p.id == found.id)
        .map(|(_, path)| util::contract(path))
        .unwrap_or_default();
    format!("wsp: `{}` was removed — the archive still holds it at {path}", found.id)
}

/// Remove a project. Refuses while anything still points at it, because the
/// alternative is silently orphaning work. `--force` does the orphaning
/// explicitly: tasks fall back to the inbox, children reparent to whatever the
/// removed project hung from.
///
/// The file is retired to the archive rather than deleted, and the prose it
/// carried is named on the way out. Removing a project used to be the one
/// operation in the store that destroyed writing nothing else held: tasks are
/// moved out first and take their overviews with them, but a handbook, a brief
/// and a decision log have no `wsp mv` — they live in this file and nowhere
/// else. `wsp project rm batch` took an eleven-lane table with it, said
/// `removed project batch`, and was recoverable only because the store happens
/// to be a git repository and somebody thought to look. That is luck, not a
/// recovery path.
///
/// So this does what `wsp rm` has always done for a task: retire, do not
/// delete. Refusing outright would be wrong — emptying a project and removing
/// it is the correct flow, and it was followed. The bug was that one field went
/// quietly, which is why the line it prints now says what was kept and where.
///
/// **`-n` shows all of that before any of it happens**, and it is nearly free:
/// this verb already counts the tasks and the children before it acts, because
/// that count is what it refuses on without `--force`. A dry run is the same
/// count printed on the other branch. Until `worklist-050` the word parsed, was
/// never read, and `wsp project rm batch --force -n` did the whole thing — the
/// invocation somebody types precisely because they do not know what is under
/// the project.
///
/// It is also the one of these whose damage is *dispersed* rather than
/// contained: a removed tree is one directory, and `--force` here writes a new
/// `project:` on every task the project held. Which tasks those are is the
/// question, and the answer is the list below.
pub fn rm(store: &Store, args: &Args) -> i32 {
    // rest[0] is the `rm` subcommand itself; the id follows it.
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp project rm <id> [--force]");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(p) = index.find(&needle).cloned() else {
        eprintln!("{}", no_project(store, &needle));
        return 1;
    };

    let tasks: Vec<_> = store.tasks().into_iter().filter(|t| t.project.as_deref() == Some(p.id.as_str())).collect();
    let children: Vec<_> = index.children(Some(&p.id)).into_iter().cloned().collect();

    if (!tasks.is_empty() || !children.is_empty()) && !args.has("force") {
        eprintln!(
            "wsp: `{}` still holds {} task(s) and {} child project(s) — pass --force to \
             orphan the tasks to the inbox and reparent the children",
            p.id,
            tasks.len(),
            children.len()
        );
        return 1;
    }
    // After the refusal above and not before it: on a project that still holds
    // work, what `rm` *would* do is refuse, and it says so in the sentence
    // above with the counts already in it. A dry run that printed a removal
    // there would be answering a question nobody asked.
    if args.has("dry-run") {
        return would_remove(&p, &tasks, &children, args);
    }

    for mut t in tasks {
        t.project = None;
        t.log(&format!("project `{}` removed — moved to inbox", p.id));
        t.touch();
        if let Err(e) = store.save_task(&t) {
            eprintln!("wsp: {}: {e}", t.id);
            return 1;
        }
    }
    for mut c in children {
        c.parent = p.parent.clone();
        if let Err(e) = store.save_project(&c) {
            eprintln!("wsp: {}: {e}", c.id);
            return 1;
        }
    }

    let kept = prose_in(&p);
    let filed = match store.archive_project(&p) {
        Ok(name) => name,
        Err(e) => {
            eprintln!("wsp: archive failed: {e}");
            return 1;
        }
    };
    store.log_event("project-removed", json!({ "id": p.id, "parent": p.parent }));
    store.git_commit(&format!("wsp: project rm {}", p.id));

    if args.json() {
        println!(
            "{}",
            json!({ "removed": p.id, "archived": true, "filed_as": filed, "kept": kept })
        );
    } else if kept.is_empty() {
        println!("removed project {} (archived)", p.id);
    } else {
        // The prose is the whole reason this is an archive and not a delete,
        // so it is named rather than counted: a reader six weeks later is
        // looking for the handbook, and this is the line that tells them it
        // still exists and which file it is in.
        println!(
            "removed project {} — {} kept in archive/projects/{filed}.md",
            p.id,
            kept.join(", ")
        );
    }
    0
}

/// `wsp project rm <id> -n` — the same removal, said and not done.
///
/// Three lists, because `--force` does three different things and only the
/// first is visible from where the caller stands. The tasks are **named** and
/// not counted: `--force` writes a new `project:` onto every one of them, the
/// refusal above already gives the count, and a count is not something anybody
/// can check against what they meant. Capped, because a project holding two
/// hundred tasks would otherwise answer a safety question with two hundred
/// lines nobody reads to the end of — and the count is on the line above the
/// list either way, which is what makes cutting it honest.
///
/// The archive filename is deliberately not predicted. [`Store::archive_project`]
/// picks it, and it is not always `<id>.md` — the archive may already hold that
/// name from an earlier project that wore the id. A dry run that named a file
/// and was wrong about it would be worse than one that names the directory.
fn would_remove(proj: &Project, tasks: &[crate::model::Task], children: &[Project], args: &Args) -> i32 {
    let p = Paint::new();
    let kept = prose_in(proj);
    if args.json() {
        println!(
            "{}",
            json!({
                "removed": proj.id,
                "archived": true,
                "kept": kept,
                "orphaned": tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
                "reparented": children.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
                "to": proj.parent,
                "dry_run": true,
            })
        );
        return 0;
    }

    println!("would remove project {} {}", proj.id, p.dim("(archived, not deleted)"));
    if !tasks.is_empty() {
        println!("  {} task(s) would go to the inbox:", tasks.len());
        for t in tasks.iter().take(RM_MAX) {
            println!("    {} {}", p.dim(&t.id), util::truncate(&t.title, 60));
        }
        if tasks.len() > RM_MAX {
            println!("    {}", p.dim(&format!("… and {} more", tasks.len() - RM_MAX)));
        }
    }
    if !children.is_empty() {
        let to = match &proj.parent {
            Some(parent) => format!("under {parent}"),
            None => "to the top level".to_string(),
        };
        println!("  {} child project(s) would move {to}:", children.len());
        for c in children {
            println!("    {}", p.dim(&c.id));
        }
    }
    if !kept.is_empty() {
        println!("  {} kept in archive/projects/", kept.join(", "));
    }
    println!("{}", p.dim("nothing moved — drop -n to do it"));
    0
}

/// How many orphaned tasks a dry run names before it starts counting. Twenty is
/// a screen: enough to recognise a project you did not mean, short enough that
/// the last line — the one saying nothing has happened — is still on it.
const RM_MAX: usize = 20;

/// The prose a project would take with it, section by section, for the line
/// `rm` prints.
///
/// Sections come from [`crate::model::PROJECT_PROSE`] rather than a list here,
/// so a section added to the vocabulary cannot go missing from the sentence
/// that exists to account for it. `brief` is frontmatter and has no heading,
/// which is exactly why it is easy to forget it is prose too.
fn prose_in(p: &Project) -> Vec<String> {
    let mut out = Vec::new();
    if !p.brief.trim().is_empty() {
        out.push("brief".to_string());
    }
    for name in crate::model::PROJECT_PROSE {
        let text = crate::model::section_of(&p.body, name).unwrap_or_default();
        if text.trim().is_empty() {
            continue;
        }
        let n = text.trim().lines().count();
        out.push(format!(
            "{} ({n} line{})",
            name.to_ascii_lowercase(),
            if n == 1 { "" } else { "s" }
        ));
    }
    out
}

/// Edit a project's prose, on the same terms as a task's: the body only, never
/// the frontmatter. `roots`, `tags`, `parent` and `status` all have
/// `wsp project set`, so there is nothing here an editor needs to reach.
pub fn edit(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!(
            "usage: wsp project edit <id> [--overview | --details | --handbook | --decisions | --raw]"
        );
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(p) = index.find(&needle).cloned() else {
        eprintln!("{}", no_project(store, &needle));
        return 1;
    };
    crate::cmd_task::edit_prose(
        store,
        args,
        crate::cmd_task::Prose {
            what: "project",
            id: p.id.clone(),
            body: p.body.clone(),
            path: store.projects_dir().join(format!("{}.md", p.id)),
            sections: &crate::model::PROJECT_PROSE,
        },
    )
}

pub fn set(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp project set <id> key=value…");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(mut proj) = index.find(&needle).cloned() else {
        eprintln!("{}", no_project(store, &needle));
        return 1;
    };

    let mut changed = Vec::new();
    for pair in args.rest.iter().skip(2) {
        let Some((k, v)) = pair.split_once('=') else {
            eprintln!("wsp: `{pair}` is not key=value");
            return 2;
        };
        // Every value here is one frontmatter line. See the same check in
        // `add`: this is the file the whole tree is read out of.
        if let Some(why) = util::terminal_output(v) {
            eprintln!("wsp: {k}: {why}");
            return 2;
        }
        match k {
            "name" => proj.name = v.to_string(),
            "brief" => proj.brief = v.to_string(),
            "status" => proj.status = v.to_string(),
            "parent" => {
                if v.is_empty() || v == "none" {
                    proj.parent = None;
                } else {
                    match index.find(v) {
                        // Into itself, or into something already beneath it.
                        // Every walk over the tree guards against a cycle and
                        // stops, so this does not hang — it does something
                        // quieter and worse: the loop has no root, so nothing
                        // in it is ever reached from `children(None)` and the
                        // whole branch disappears from `wsp tree` and from the
                        // panel. Two projects and everything under them, gone
                        // from every list, with the files still on disk and
                        // the command reporting success.
                        //
                        // Refused here rather than in the panel, which is
                        // about to put this on a key: the rule belongs with
                        // the index that can see the subtree, and a caller
                        // that has to know it is a caller keeping a second
                        // copy of it.
                        Some(f) if index.subtree(&proj.id).contains(&f.id) => {
                            eprintln!(
                                "wsp: {} is {} — a project cannot go inside itself",
                                f.id,
                                if f.id == proj.id { "itself".into() } else { format!("under {}", proj.id) },
                            );
                            return 1;
                        }
                        Some(f) => proj.parent = Some(f.id.clone()),
                        None => {
                            eprintln!("wsp: no such parent `{v}`");
                            return 1;
                        }
                    }
                }
            }
            "tags" => proj.tags = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "roots" => {
                proj.roots = v
                    .split(',')
                    .map(|s| util::contract(&util::expand(s)))
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            other => {
                eprintln!("wsp: cannot set `{other}` (name|brief|status|parent|tags|roots)");
                return 2;
            }
        }
        changed.push(k.to_string());
    }

    if changed.is_empty() {
        eprintln!("wsp: nothing to set");
        return 2;
    }
    if let Err(e) = store.save_project(&proj) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.git_commit(&format!("wsp: update project {} ({})", proj.id, changed.join(",")));
    if args.json() {
        println!("{}", project_json(&proj, &Index::new(store.projects())));
    } else {
        println!("{} updated: {}", proj.id, changed.join(", "));
    }
    0
}

pub fn project_json(p: &Project, index: &Index) -> serde_json::Value {
    json!({
        "id": p.id,
        "name": p.name,
        "parent": p.parent,
        "tags": p.tags,
        "effective_tags": index.effective_tags(&p.id),
        "roots": p.roots,
        "status": p.status,
        "brief": p.brief,
        "handbook": crate::model::section_of(&p.body, "Handbook"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn scratch(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    fn proj(store: &Store, id: &str, parent: Option<&str>) {
        let mut p = Project::new(id);
        p.parent = parent.map(|s| s.to_string());
        store.save_project(&p).unwrap();
    }

    /// The bug this file exists to have fixed: `wsp project rm batch` deleted
    /// the project file, and with it an eleven-lane table that existed nowhere
    /// else, while printing `removed project batch`. The tasks had been moved
    /// out first and were fine — a task carries its own prose — so every
    /// visible thing was right, and the one field with no `wsp mv` was gone.
    ///
    /// Three things are asserted because they fail separately: the prose is
    /// still readable afterwards, the live file is gone (retiring that left a
    /// project in two places would be its own bug), and the removal *names*
    /// what it kept — a silent archive is only half an answer to a silent
    /// deletion, since nobody goes looking in a directory they were never told
    /// about.
    #[test]
    fn removing_a_project_retires_its_prose_instead_of_destroying_it() {
        let store = scratch("project-rm-archive");
        let mut p = Project::new("batch");
        p.brief = "parallel work, grouped by file".into();
        p.body = "## Handbook\none lane per file\ntwo agents never share one\n".into();
        store.save_project(&p).unwrap();

        assert_eq!(rm(&store, &Args::synth("project", &["rm", "batch"], &[])), 0);

        assert!(store.project("batch").is_none(), "and it is gone from the live store");
        let archived = store.archived_projects();
        assert_eq!(archived.len(), 1, "retired, not deleted");
        assert!(
            archived[0].0.body.contains("one lane per file"),
            "the handbook is the part nothing else holds:\n{}",
            archived[0].0.body
        );
        assert_eq!(archived[0].0.brief, "parallel work, grouped by file");

        assert_eq!(
            prose_in(&p),
            vec!["brief".to_string(), "handbook (2 lines)".to_string()],
            "and the line it prints says what it kept"
        );
    }

    /// `worklist-050`. `--force` here rewrites the `project:` of every task the
    /// project held, and until this the invocation that says *show me which
    /// ones* did the rewrite and then said nothing. So the assertion is on the
    /// store rather than on the output: after `-n`, the project is still there,
    /// the tasks are still in it, and the child is still under it.
    ///
    /// The exit code matters as much as the state. `-n` is a question that was
    /// answered, so it succeeds — a dry run that exited non-zero would put
    /// every careful caller's script in the branch it takes for failure.
    #[test]
    fn a_dry_run_of_project_rm_moves_no_task_and_keeps_the_project() {
        let store = scratch("project-rm-dry");
        proj(&store, "batch", None);
        proj(&store, "batch-sub", Some("batch"));
        let mut t = crate::model::Task::new("a lane", "t-260817-050");
        t.project = Some("batch".into());
        store.save_task(&t).unwrap();

        let dry = &[("force", "true"), ("dry-run", "true")];
        assert_eq!(rm(&store, &Args::synth("project", &["rm", "batch"], dry)), 0, "a dry run is an answer, not a failure");

        assert!(store.project("batch").is_some(), "the dry run retired the project");
        assert_eq!(store.task("t-260817-050").unwrap().project.as_deref(), Some("batch"), "a task was orphaned by a dry run");
        assert_eq!(store.project("batch-sub").unwrap().parent.as_deref(), Some("batch"), "a child was reparented by a dry run");
        assert!(store.archived_projects().is_empty(), "the dry run filed it in the archive");

        // And the same words without `-n` do all three, so the preview was of
        // something real.
        assert_eq!(rm(&store, &Args::synth("project", &["rm", "batch"], &[("force", "true")])), 0);
        assert!(store.project("batch").is_none());
        assert_eq!(store.task("t-260817-050").unwrap().project, None);
    }

    /// A project with nothing written on it says so plainly rather than
    /// claiming to have saved prose that was never there. The summary is what
    /// decides which sentence `rm` prints, so it is the thing to assert.
    #[test]
    fn a_project_with_no_prose_has_nothing_to_account_for() {
        assert!(prose_in(&Project::new("empty")).is_empty());
    }

    /// Retirement must not become deletion by another route. A slug can be
    /// used again — `batch` removed, `batch` created, `batch` removed — and an
    /// archive keyed by id would file the second one on top of the first,
    /// which is the exact failure the task archive already carries scars from.
    #[test]
    fn the_archive_never_files_a_project_on_top_of_an_earlier_one() {
        let store = scratch("project-rm-twice");
        for handbook in ["the first argument", "the second argument"] {
            let mut p = Project::new("batch");
            p.body = format!("## Handbook\n{handbook}\n");
            store.save_project(&p).unwrap();
            assert_eq!(rm(&store, &Args::synth("project", &["rm", "batch"], &[])), 0);
        }
        let bodies: Vec<String> = store.archived_projects().into_iter().map(|(p, _)| p.body).collect();
        assert_eq!(bodies.len(), 2, "both are kept");
        assert!(bodies.iter().any(|b| b.contains("the first argument")), "{bodies:#?}");
        assert!(bodies.iter().any(|b| b.contains("the second argument")), "{bodies:#?}");
    }

    /// "no such project" is a true sentence that sends someone away from a
    /// file sitting right there — and they are usually asking *because* it has
    /// left every list. The answer has to name where it went.
    #[test]
    fn a_project_that_was_removed_says_where_it_went() {
        let store = scratch("project-rm-miss");
        let mut p = Project::new("batch");
        p.body = "## Handbook\none lane per file\n".into();
        store.save_project(&p).unwrap();
        rm(&store, &Args::synth("project", &["rm", "batch"], &[]));

        let said = no_project(&store, "batch");
        assert!(said.contains("was removed"), "{said}");
        assert!(said.contains("archive/projects/batch.md"), "{said}");
        assert!(no_project(&store, "nothing-like-it").contains("no project matching"));
    }

    /// The failure this guard is for is a silent one. Every walk over the
    /// project tree already stops on a cycle, so nothing hangs — but a loop
    /// has no root, so `children(None)` never reaches it and the whole branch
    /// vanishes from `wsp tree` and from the panel, files still on disk,
    /// command reporting success. Measured before the guard: `project set
    /// alpha parent=alpha` printed `alpha updated: parent` and `wsp tree`
    /// printed nothing at all.
    ///
    /// It matters more now than it did, because `m` on a project row is this
    /// command with the tree as its picker, and a picker makes every wrong
    /// answer one keystroke away.
    #[test]
    fn a_project_cannot_be_moved_inside_itself() {
        let store = scratch("reparent-cycle");
        proj(&store, "alpha", None);
        proj(&store, "beta", Some("alpha"));
        proj(&store, "gamma", Some("beta"));
        proj(&store, "delta", None);

        let set = |rest: &[&str]| set(&store, &Args::synth("project", rest, &[]));
        let parent_of = |id: &str| {
            Index::new(store.projects()).get(id).and_then(|p| p.parent.clone())
        };

        assert_eq!(set(&["set", "alpha", "parent=alpha"]), 1, "itself");
        assert_eq!(set(&["set", "alpha", "parent=beta"]), 1, "its own child");
        // Two levels down is the same cycle and was the same silence.
        assert_eq!(set(&["set", "alpha", "parent=gamma"]), 1, "further beneath it");
        assert_eq!(parent_of("alpha"), None, "and nothing was written");

        // Everything else still moves. The guard is about the subtree, not
        // about reparenting.
        assert_eq!(set(&["set", "alpha", "parent=delta"]), 0);
        assert_eq!(parent_of("alpha"), Some("delta".into()));
        assert_eq!(set(&["set", "alpha", "parent=none"]), 0, "and back out again");
        assert_eq!(parent_of("alpha"), None);
    }

    /// The live example this was written from. `wsp project show` abridges each
    /// decision to its first sentence — the rule — so the entry 11eb4ac
    /// withdrew went on stating "the store autocommits with `git add -A`" as
    /// current, with the correction two rows below and nothing joining them.
    /// Unabridged the reader reaches "until wsp-029 narrows it…" and knows to
    /// check; abridged, they do not.
    ///
    /// Both marks are asserted, because they fail separately: colour is off
    /// here — under `NO_COLOR`, in a pipe, in this test — and the strike is
    /// then invisible, so the words have to carry it.
    #[test]
    fn a_superseded_decision_is_marked_in_the_abridged_block() {
        let mut body = String::new();
        let old = crate::model::append_decision(
            &mut body,
            "an uncommitted file in ~/wsp is not yours. The store autocommits with git add -A.",
            &[],
        );
        crate::model::append_decision(
            &mut body,
            "the commit is scoped to the paths a command wrote. Anything else you commit yourself.",
            &[old],
        );

        let p = Paint::new();
        let decided = crate::model::decisions_of(&body);
        let lines = decision_lines(&p, &decided, false, "wsp");

        assert!(lines[0].contains("d1"), "the id is the handle the mark needs:\n{lines:#?}");
        assert!(
            lines[0].contains("superseded by d2"),
            "the withdrawn rule is named as withdrawn:\n{lines:#?}"
        );
        assert!(
            lines[0].contains("an uncommitted file in ~/wsp is not yours."),
            "and it is still shown — this is the record, not a tidied conclusion:\n{lines:#?}"
        );
        assert!(!lines[1].contains("superseded"), "the live rule reads as live:\n{lines:#?}");
        assert!(
            lines[2].contains("2 of 2 abridged"),
            "and the count is unchanged by any of it:\n{lines:#?}"
        );
    }

    /// `Handbook` is the one section a project has and a task does not, and the
    /// vocabulary is per-kind so that neither answer has to be remembered.
    ///
    /// A task offered it would put an empty heading in the combined edit buffer
    /// of every task in the store, for a section nobody could sensibly fill —
    /// so `wsp edit <task> --handbook` is a typo, and a typo that this command
    /// guesses at costs whatever prose was already there.
    #[test]
    fn a_handbook_is_a_project_section_and_a_task_flag_is_a_typo() {
        let store = scratch("handbook");
        proj(&store, "alpha", None);
        let mut t = crate::model::Task::new("something", "t-260817-001");
        t.body = "## Overview\nthe prose that must survive a typo\n".into();
        store.save_task(&t).unwrap();

        let src = store.root.join("hb.md");
        crate::store::write_atomic(&src, "the map of the code is architecture.md at the root\n").unwrap();
        let from = src.display().to_string();

        assert_eq!(
            edit(&store, &Args::synth("project", &["edit", "alpha"], &[("handbook", "true"), ("from", &from)])),
            0
        );
        let body = store.project("alpha").unwrap().body;
        assert_eq!(
            crate::model::section_of(&body, "Handbook").as_deref(),
            Some("the map of the code is architecture.md at the root"),
        );
        // Written as a section the schema knows, so it survives a round trip
        // rather than being folded into `Overview` as a stray.
        assert!(crate::model::stray_sections(&body).is_empty(), "{body}");

        // The same flag on a task is refused, and refused before anything is
        // written — this is the failure that made `edit` refuse unknown flags
        // in the first place.
        assert_eq!(
            crate::cmd_task::edit(
                &store,
                &Args::synth("edit", &["t-260817-001"], &[("handbook", "true"), ("from", &from)])
            ),
            2
        );
        let after = store.task("t-260817-001").unwrap().body;
        assert!(after.contains("the prose that must survive a typo"), "{after}");
        assert!(!after.contains("architecture.md"), "{after}");

        let _ = std::fs::remove_dir_all(&store.root);
    }
}
