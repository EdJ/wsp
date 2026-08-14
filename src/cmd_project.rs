//! Project commands: the hierarchy and its tags.

use serde_json::json;

use crate::model::Project;
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

pub fn init(store: &Store, args: &Args) -> i32 {
    if let Err(e) = store.ensure_dirs() {
        eprintln!("wsp: cannot create {}: {e}", util::contract(&store.root));
        return 1;
    }
    store.git_init();

    let readme = store.root.join("README.md");
    if !readme.exists() {
        let _ = crate::store::write_atomic(
            &readme,
            "# wsp store\n\nProjects in `projects/`, tasks in `tasks/`, one file each.\n\
             Mutate through the `wsp` CLI — it owns id allocation, atomic writes and commits.\n",
        );
    }
    let hooks = store.root.join("hooks");
    let _ = std::fs::create_dir_all(&hooks);

    store.git_commit("wsp: init store");

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
        eprintln!("usage: wsp project show <id>");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(proj) = index.find(&needle).cloned() else {
        eprintln!("wsp: no such project `{needle}`");
        return 1;
    };

    let tasks = store.tasks();
    let scope = index.subtree(&proj.id);
    let mine: Vec<_> = tasks
        .iter()
        .filter(|t| t.project.as_ref().map(|p| scope.contains(p)).unwrap_or(false))
        .filter(|t| t.status().is_open())
        .collect();

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

    if !mine.is_empty() {
        println!("\n{}", p.dim("OPEN TASKS"));
        for t in &mine {
            println!(
                "  {}  {}  {}",
                p.dim(&t.id),
                util::pad(t.status().as_str(), 8),
                util::truncate(&t.title, 60)
            );
        }
    }
    if !proj.body.trim().is_empty() {
        println!("\n{}", proj.body.trim());
    }
    0
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
