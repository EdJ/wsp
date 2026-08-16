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

    let index = Index::new(store.projects());
    let mut p = Project::new(&slug);
    p.name = args.get("name").unwrap_or_else(|| slug_raw.clone());
    p.tags = args.all("tag");
    p.roots = args.all("root").iter().map(|r| util::contract(&util::expand(r))).collect();
    p.brief = args.get("brief").unwrap_or_default();
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

pub fn show(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp project show <id> [--decisions]");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(proj) = index.find(&needle).cloned() else {
        eprintln!("wsp: no such project `{needle}`");
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
    let decided = crate::model::decisions(&proj.body);
    let full = args.has("decisions");
    if !decided.is_empty() {
        println!("\n{}", p.dim("DECISIONS"));
        let mut cut = 0;
        for (when, what) in &decided {
            if full {
                println!("  {}  {}", p.dim(&util::pad(when, 10)), what);
                continue;
            }
            let lead = util::first_sentence(what);
            if lead.len() < what.trim_end().len() {
                cut += 1;
            }
            println!("  {}  {}", p.dim(&util::pad(when, 10)), util::truncate(lead, 96));
        }
        // Never trim quietly: an entry cut to its first sentence reads exactly
        // like an entry that was only ever one, and the difference is the
        // reasoning somebody is about to re-derive.
        if cut > 0 {
            println!(
                "  {}  {}",
                util::pad("", 10),
                p.dim(&format!("{cut} of {} abridged · wsp project show {} --decisions", decided.len(), proj.id))
            );
        }
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
    let mut rest = proj.body.clone();
    crate::model::set_section_in(&mut rest, "Decisions", "");
    if !rest.trim().is_empty() {
        println!("\n{}", rest.trim());
    }
    0
}

/// Remove a project. Refuses while anything still points at it, because the
/// alternative is silently orphaning work. `--force` does the orphaning
/// explicitly: tasks fall back to the inbox, children reparent to whatever the
/// removed project hung from.
pub fn rm(store: &Store, args: &Args) -> i32 {
    // rest[0] is the `rm` subcommand itself; the id follows it.
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp project rm <id> [--force]");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(p) = index.find(&needle).cloned() else {
        eprintln!("wsp: no project matching `{needle}`");
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

    let path = store.projects_dir().join(format!("{}.md", p.id));
    if let Err(e) = std::fs::remove_file(&path) {
        eprintln!("wsp: {}: {e}", path.display());
        return 1;
    }
    // Removed here rather than through the store, so the commit has to be told
    // about it — the tasks and children rewritten above said so themselves.
    store.wrote(path);
    store.log_event("project-removed", json!({ "id": p.id, "parent": p.parent }));
    store.git_commit(&format!("wsp: project rm {}", p.id));

    if args.json() {
        println!("{}", json!({ "removed": p.id }));
    } else {
        println!("removed project {}", p.id);
    }
    0
}

/// Edit a project's prose, on the same terms as a task's: the body only, never
/// the frontmatter. `roots`, `tags`, `parent` and `status` all have
/// `wsp project set`, so there is nothing here an editor needs to reach.
pub fn edit(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp project edit <id> [--overview | --details | --decisions | --raw]");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(p) = index.find(&needle).cloned() else {
        eprintln!("wsp: no project matching `{needle}`");
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
        eprintln!("wsp: no such project `{needle}`");
        return 1;
    };

    let mut changed = Vec::new();
    for pair in args.rest.iter().skip(2) {
        let Some((k, v)) = pair.split_once('=') else {
            eprintln!("wsp: `{pair}` is not key=value");
            return 2;
        };
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
}
