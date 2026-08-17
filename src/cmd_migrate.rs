//! `wsp migrate` — off dated ids and into each project's own numbering space.
//!
//! The change this carries out is t-260815-024, and the reason for it is two
//! incidents on one day. An agent read `wsp add`'s answer of `t-260817-014`,
//! typed `013`, and wrote one task's overview onto a task in a *different
//! project* — a single-digit slip that crossed a project boundary invisibly,
//! because under the dated scheme nothing in an id says where its task lives.
//! Hours later Ed lost his own time to `022`, which by then named two open
//! tasks. The first cost an agent a minute and was recoverable from git. The
//! second cost a person their attention, and that is the thing this scheme
//! exists to protect.
//!
//! Under the new scheme those two ids are `wsp-013` and `robustness-014`, and
//! the same slip is an id that does not exist rather than one that does.
//!
//! # Why this is a command and not a script
//!
//! It has to be re-runnable, it has to be inspectable before it is believed,
//! and it has to leave the old ids resolving afterwards. A migration that
//! renames 267 tasks and rewrites 800-odd references to them is not something
//! to find out about halfway through, so `-n` plans it in full and applies
//! nothing.

use std::collections::BTreeMap;

use crate::model::Project;
use crate::store::Store;
use crate::util::Paint;
use crate::Args;

/// Is this an id from the dated scheme — `t-YYMMDD-NNN`?
///
/// Asked of the id rather than of a migration flag, so a store part-way
/// through, or one that gained a task between the plan and the apply, is
/// simply migrated the rest of the way. Re-running on a finished store is a
/// no-op rather than a second renumbering.
pub fn is_dated(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("t-") else { return false };
    let Some((date, seq)) = rest.split_once('-') else { return false };
    date.len() == 6
        && seq.len() == 3
        && date.bytes().all(|b| b.is_ascii_digit())
        && seq.bytes().all(|b| b.is_ascii_digit())
}

/// Work out what every dated id should become.
///
/// Numbering runs in old-id order, which under the dated scheme is
/// chronological — so a project's new ids climb in the order its work was
/// filed, and `wsp-001` is the first task ever opened against wsp rather than
/// whichever one the directory happened to list first.
///
/// Archived tasks are numbered alongside live ones and not skipped. They share
/// the space, four of them are cited by live prose, and a number given to a
/// live task that an archived one already answers to is precisely the "one
/// name, two pieces of work" failure the archive already carries scars from.
fn plan(store: &Store) -> (BTreeMap<String, String>, BTreeMap<String, usize>) {
    let by_code: BTreeMap<String, String> =
        store.projects().into_iter().map(|p| (p.id.clone(), p.code().to_string())).collect();

    // Everything in the id space, live and archived, oldest first.
    let mut all: Vec<(String, Option<String>)> = store
        .tasks()
        .into_iter()
        .map(|t| (t.id, t.project))
        .chain(store.archived_tasks().into_iter().map(|t| (t.id, t.project)))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all.dedup_by(|a, b| a.0 == b.0);

    // Where each space has already got to, so a part-migrated store carries on
    // from where it stopped rather than colliding with what it did last time.
    let mut next: BTreeMap<String, usize> = BTreeMap::new();
    for (id, _) in &all {
        if let Some((code, n)) = id.rsplit_once('-') {
            if let Ok(n) = n.parse::<usize>() {
                let slot = next.entry(code.to_string()).or_insert(0);
                *slot = (*slot).max(n);
            }
        }
    }

    let mut map = BTreeMap::new();
    for (id, project) in &all {
        if !is_dated(id) {
            continue;
        }
        let code = match project {
            Some(p) => by_code.get(p).cloned().unwrap_or_else(|| p.clone()),
            None => crate::store::INBOX_CODE.to_string(),
        };
        let n = next.entry(code.clone()).or_insert(0);
        *n += 1;
        map.insert(id.clone(), format!("{code}-{:03}", *n));
    }
    (map, next)
}

pub fn run(store: &Store, args: &Args) -> i32 {
    if let Some(dir) = args.get("refs") {
        return refs(store, &dir, args);
    }

    let p = Paint::new();
    let (map, tops) = plan(store);
    if map.is_empty() {
        println!("nothing to migrate — every id is already in its project's space");
        return 0;
    }

    // What it will do, always, whether or not it is about to do it. A rename of
    // this size that only reports afterwards is one you cannot decline.
    let mut by_code: BTreeMap<String, Vec<(&String, &String)>> = BTreeMap::new();
    for (from, to) in &map {
        let code = to.rsplit_once('-').map(|(c, _)| c).unwrap_or(to);
        by_code.entry(code.to_string()).or_default().push((from, to));
    }
    for (code, rows) in &by_code {
        println!("{}  {} task(s)", p.bold(code), rows.len());
        for (from, to) in rows.iter().take(if args.has("all") { usize::MAX } else { 3 }) {
            println!("    {} → {}", p.dim(from), to);
        }
        if rows.len() > 3 && !args.has("all") {
            println!("    {}", p.dim(&format!("… and {} more", rows.len() - 3)));
        }
    }
    println!("\n{} task(s) across {} space(s)", map.len(), by_code.len());

    if args.has("n") || args.has("dry-run") {
        println!("{}", p.dim("nothing written — drop -n to apply"));
        return 0;
    }

    match store.rename_tasks(&map) {
        Ok((files, refs)) => {
            // The counters come last and are set from what was actually handed
            // out, not from what the plan intended — if the rename stopped
            // short, a counter that ran ahead of the files would hand the next
            // task an id nobody can see is taken.
            let mut saved = 0;
            for mut proj in store.projects() {
                if let Some(top) = tops.get(proj.code()) {
                    if proj.seq != *top {
                        proj.seq = *top;
                        if store.save_project(&proj).is_ok() {
                            saved += 1;
                        }
                    }
                }
            }
            store.git_commit(&format!(
                "wsp: migrate {} task(s) to per-project ids",
                map.len()
            ));
            println!("renamed {} task(s)", map.len());
            println!("rewrote {refs} reference(s) across {files} file(s)");
            println!("set the counter on {saved} project(s)");
            println!(
                "{}",
                p.dim(&format!(
                    "old ids go on resolving — {} records what became what",
                    store.ids_path().display()
                ))
            );
            println!(
                "{}",
                p.dim("source trees that cite task ids: wsp migrate --refs <path>")
            );
            0
        }
        Err(e) => {
            eprintln!("wsp: migration failed: {e}");
            eprintln!("wsp: the store is under git — `git -C {} status` shows how far it got", store.root.display());
            1
        }
    }
}

/// Apply the recorded renaming to a tree of source files.
///
/// Separate from the migration proper, and after it, because the map does not
/// exist until the store has moved. It is also the general tool: any repo whose
/// comments cite task ids — this one has 229 of them — can be brought forward
/// without the migration having to know where those repos are.
///
/// Git history is deliberately not in scope. Ed settled that: commits before
/// the migration go on naming ids the tree no longer uses, and `ids.json` is
/// what makes that disagreement navigable rather than a dead end.
fn refs(store: &Store, dir: &str, args: &Args) -> i32 {
    let map = store.renamed_ids();
    if map.is_empty() {
        eprintln!("wsp: no renaming on record — run `wsp migrate` first");
        return 1;
    }
    let root = crate::util::expand(dir);
    let mut files = 0usize;
    let mut total = 0usize;
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            // `.git` above all: rewriting objects is not rewriting references,
            // and `target` is build output that is regenerated anyway.
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let (out, n) = crate::store::substitute_tokens(&text, &map);
            if n == 0 {
                continue;
            }
            files += 1;
            total += n;
            if args.has("n") || args.has("dry-run") {
                println!("{n:4}  {}", path.strip_prefix(&root).unwrap_or(&path).display());
            } else if let Err(e) = crate::store::write_atomic(&path, &out) {
                eprintln!("wsp: {}: {e}", path.display());
                return 1;
            }
        }
    }
    let p = Paint::new();
    println!("{total} reference(s) across {files} file(s) under {}", root.display());
    if args.has("n") || args.has("dry-run") {
        println!("{}", p.dim("nothing written — drop -n to apply"));
    }
    0
}

/// Set or show a project's id prefix.
///
/// A code is what lets a descriptive slug stay descriptive while the id stays
/// short: `strata-prototype` sets `code=sp` and its tasks are `sp-062`. Ed's
/// own examples were `wsp-rob` and `mtw-rob`, which is the point — a code that
/// wants to carry the path may, but the id is never *derived* from a path,
/// because ancestry is mutable and an id must not be.
pub fn code(store: &Store, args: &Args) -> i32 {
    let index = crate::resolve::Index::new(store.projects());
    let Some(needle) = args.rest.first() else {
        let p = Paint::new();
        for proj in store.projects() {
            let marker = if proj.code_raw.is_empty() { p.dim("(slug)") } else { String::new() };
            println!("{:<20} {:<12} {}", proj.id, proj.code(), marker);
        }
        return 0;
    };
    let Some(found) = index.find(needle) else {
        eprintln!("wsp: no such project `{needle}`");
        return 1;
    };
    let mut proj: Project = match store.project(&found.id) {
        Some(p) => p,
        None => return 1,
    };

    let Some(new) = args.rest.get(1) else {
        println!("{} {}", proj.id, proj.code());
        return 0;
    };
    let new = crate::util::slugify(new);
    if new.is_empty() {
        eprintln!("wsp: `{}` does not reduce to a usable code", args.rest[1]);
        return 2;
    }
    if new.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        // `1-014` would make the whole id read as a number, and a bare suffix
        // could no longer be told from a whole id.
        eprintln!("wsp: a code cannot begin with a digit");
        return 2;
    }
    if crate::cmd_migrate::is_dated(&format!("{new}-001")) {
        eprintln!("wsp: `{new}` would produce ids indistinguishable from the dated scheme");
        return 2;
    }
    if new == proj.code() {
        println!("{} already numbers under {}", proj.id, new);
        return 0;
    }
    if let Some(why) = store.code_taken(&new, Some(&proj.id)) {
        eprintln!("wsp: `{new}` is not free: {why}");
        return 1;
    }

    let was = proj.code().to_string();
    proj.code_raw = new.clone();
    // The counter belongs to the *space*, and the space just changed. Starting
    // the new one from scratch is right; what would be wrong is carrying the
    // old count over, which would leave `sp-001`…`sp-062` unused for ever.
    proj.seq = 0;
    if let Err(e) = store.save_project(&proj) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.git_commit(&format!("wsp: {} numbers under {new}", proj.id));
    println!("{} now numbers under {new} (was {was})", proj.id);
    println!(
        "{}",
        Paint::new().dim("tasks already handed out keep their old prefix — an id names the task, not where it sits")
    );
    0
}
