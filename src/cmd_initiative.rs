//! `wsp initiative` (`wsp-722`): a record below a product — a repository of
//! artefacts and decisions that leads to planning and is linked to many
//! worklists and tasks.
//!
//! One file each at `<store>/initiatives/init-N.md`, ids from one global
//! sequence (compound's artefact owner is `init-N`, folder
//! `~/.compound/artefacts/init-N/`). Planning, increments and aligns are not
//! here: `worklists` and `tasks` are links only.

use serde_json::json;
use std::path::PathBuf;

use crate::base::args::Args;
use wsp_base::fm::{self, Doc};
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};

/// The states, in the order an initiative passes through them.
pub const STATES: [&str; 5] = ["draft", "requirements", "planned", "building", "closed"];
/// The prose sections, in file order. `Decisions` is written by `wsp decide`.
pub const SECTIONS: [&str; 6] = ["Brief", "Requirements", "Decisions", "Questions", "Assumptions", "Sources"];

#[derive(Debug, Clone, Default)]
pub struct Initiative {
    pub id: String,
    pub title: String,
    pub product: String,
    pub state: String,
    pub projects: Vec<String>,
    pub worklists: Vec<String>,
    pub tasks: Vec<String>,
    pub created: String,
    pub body: String,
}

impl Initiative {
    pub fn from_doc(doc: &Doc, fallback: &str) -> Initiative {
        Initiative {
            id: doc.opt("id").unwrap_or_else(|| fallback.to_string()),
            title: doc.str("title"),
            product: doc.str("product"),
            state: doc.opt("state").unwrap_or_else(|| "draft".into()),
            projects: doc.list("projects"),
            worklists: doc.list("worklists"),
            tasks: doc.list("tasks"),
            created: doc.str("created"),
            body: doc.body.clone(),
        }
    }

    pub fn render(&self) -> String {
        let mut d = Doc::default();
        d.set_str("id", &self.id);
        d.set_str("title", &self.title);
        d.set_str("product", &self.product);
        d.set_str("state", &self.state);
        d.set_list("projects", &self.projects);
        d.set_list("worklists", &self.worklists);
        d.set_list("tasks", &self.tasks);
        d.set_str("created", &self.created);
        d.body = self.body.clone();
        fm::emit(&d)
    }

    pub fn section(&self, name: &str) -> Option<String> {
        crate::model::section_of(&self.body, name)
    }

    /// `~/.compound/artefacts/init-N/`
    pub fn artefacts(&self) -> PathBuf {
        crate::artefact::root().join(&self.id)
    }

    pub fn json(&self) -> serde_json::Value {
        let decisions: Vec<_> = crate::model::decisions_of(&self.body)
            .into_iter()
            .map(|d| json!({ "id": d.id, "at": d.at, "when": d.when, "supersedes": d.supersedes, "text": d.text }))
            .collect();
        json!({
            "id": self.id,
            "title": self.title,
            "product": self.product,
            "state": self.state,
            "projects": self.projects,
            "worklists": self.worklists,
            "tasks": self.tasks,
            "created": self.created,
            "artefacts": self.artefacts().display().to_string(),
            "brief": self.section("Brief"),
            "requirements": self.section("Requirements"),
            "decisions": decisions,
            "criteria": self.criteria().iter().map(|r| r.json()).collect::<Vec<_>>(),
            "assumption_records": self.assumptions().iter().map(|r| r.json()).collect::<Vec<_>>(),
            "questions": self.section("Questions"),
            "assumptions": self.section("Assumptions"),
            "sources": self.section("Sources"),
        })
    }
}

/// A criterion or an assumption: one JSON line in the `Criteria` /
/// `Assumption records` section (the prose `Assumptions` section stays prose).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rec {
    pub id: String,
    pub text: String,
    pub source: String,
    pub status: String,
    /// Assumptions only: how much it matters if it is wrong, 1 (most) upward.
    pub effect: Option<u32>,
    /// `agent` or `person`.
    pub by: String,
}

pub const CRITERIA_STATUS: [&str; 2] = ["draft", "confirmed"];
pub const ASSUME_STATUS: [&str; 3] = ["assumed", "accepted", "changed"];
const CRIT_SEC: &str = "Criteria";
const ASSUME_SEC: &str = "Assumption records";
const ALL_SECTIONS: [&str; 8] = ["Brief", "Requirements", CRIT_SEC, "Decisions", "Questions", "Assumptions", ASSUME_SEC, "Sources"];

impl Rec {
    fn json(&self) -> serde_json::Value {
        json!({ "id": self.id, "text": self.text, "source": self.source, "status": self.status, "effect": self.effect, "by": self.by })
    }
}

impl Initiative {
    fn recs(&self, sec: &str) -> Vec<Rec> {
        let Some(t) = self.section(sec) else { return vec![] };
        t.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
            .map(|v| Rec {
                id: v["id"].as_str().unwrap_or("").into(),
                text: v["text"].as_str().unwrap_or("").into(),
                source: v["source"].as_str().unwrap_or("").into(),
                status: v["status"].as_str().unwrap_or("").into(),
                effect: v["effect"].as_u64().map(|n| n as u32),
                by: v["by"].as_str().unwrap_or("").into(),
            })
            .collect()
    }
    pub fn criteria(&self) -> Vec<Rec> {
        self.recs(CRIT_SEC)
    }
    pub fn assumptions(&self) -> Vec<Rec> {
        self.recs(ASSUME_SEC)
    }
    fn put_recs(&mut self, sec: &str, v: &[Rec]) {
        let text = v.iter().map(|r| r.json().to_string()).collect::<Vec<_>>().join("\n");
        crate::model::set_section_in_order(&mut self.body, sec, &text, &ALL_SECTIONS);
    }
}

fn rec_line(prefix: &str, r: &Rec) -> String {
    let eff = r.effect.map(|e| format!(" effect {e}")).unwrap_or_default();
    let src = if r.source.is_empty() { String::new() } else { format!(" ← {}", r.source) };
    format!("{prefix}{} [{}{eff}, {}] {}{src}", r.id, r.status, r.by, r.text)
}

pub fn dir(store: &Store) -> PathBuf {
    store.root.join("initiatives")
}

fn path(store: &Store, id: &str) -> PathBuf {
    dir(store).join(format!("{id}.md"))
}

/// `init-7` or `7`, as the id it names.
fn id_of(needle: &str) -> Option<String> {
    let n: u32 = needle.trim().strip_prefix("init-").unwrap_or(needle.trim()).parse().ok()?;
    Some(format!("init-{n}"))
}

pub fn find(store: &Store, needle: &str) -> Option<Initiative> {
    let id = id_of(needle)?;
    let text = std::fs::read_to_string(path(store, &id)).ok()?;
    Some(Initiative::from_doc(&fm::parse(&text), &id))
}

/// Every initiative, by number.
pub fn all(store: &Store) -> Vec<Initiative> {
    let mut out: Vec<(u32, Initiative)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir(store)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".md") else { continue };
            let Some(n) = id_of(stem).and_then(|i| i[5..].parse::<u32>().ok()) else { continue };
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                out.push((n, Initiative::from_doc(&fm::parse(&text), stem)));
            }
        }
    }
    out.sort_by_key(|(n, _)| *n);
    out.into_iter().map(|(_, i)| i).collect()
}

fn next_id(store: &Store) -> String {
    let n = all(store).iter().filter_map(|i| i.id[5..].parse::<u32>().ok()).max().unwrap_or(0);
    format!("init-{}", n + 1)
}

pub fn save(store: &Store, i: &Initiative) -> std::io::Result<()> {
    std::fs::create_dir_all(dir(store))?;
    let p = path(store, &i.id);
    crate::store::write_atomic(&p, &i.render())?;
    store.wrote(p);
    Ok(())
}

/// The projects under a product, not the product itself.
fn product_projects(index: &Index, product: &str) -> Vec<String> {
    index.subtree(product).into_iter().filter(|id| id != product).collect()
}

fn product_of(store: &Store, needle: &str) -> Result<String, String> {
    let index = Index::new(store.projects());
    match index.find(needle) {
        Some(p) if p.is_product() => Ok(p.id.clone()),
        Some(p) => Err(format!("wsp: `{}` is a project, not a product", p.id)),
        None => Err(format!("wsp: no product `{needle}`")),
    }
}

/// Projects checked as a subset of the product's, as ids.
fn check_projects(store: &Store, product: &str, want: &[String]) -> Result<Vec<String>, String> {
    let index = Index::new(store.projects());
    let have = product_projects(&index, product);
    let mut out = Vec::new();
    for w in want {
        let id = index.find(w).map(|p| p.id.clone()).unwrap_or_else(|| w.clone());
        if !have.contains(&id) {
            return Err(format!("wsp: `{w}` is not a project of product `{product}`"));
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

pub fn initiative(store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(|s| s.as_str()).unwrap_or("ls") {
        "add" | "new" => add(store, args),
        "ls" | "list" => ls(store, args),
        "show" | "get" => show(store, args),
        "edit" => edit(store, args),
        "set" => set(store, args),
        "link" => link(store, args),
        "brief" => brief(store, args),
        "criterion" | "criteria" => records(store, args, true),
        "assume" | "assumption" => records(store, args, false),
        other => {
            eprintln!("wsp initiative: unknown subcommand `{other}` (add|ls|show|edit|set|link|brief|criterion|assume)");
            2
        }
    }
}

fn need(store: &Store, args: &Args, usage: &str) -> Result<Initiative, i32> {
    let Some(needle) = args.rest.get(1) else {
        eprintln!("usage: {usage}");
        return Err(2);
    };
    match find(store, needle) {
        Some(i) => Ok(i),
        None => {
            eprintln!("wsp: no initiative `{needle}`");
            Err(1)
        }
    }
}

fn finish(store: &Store, i: &Initiative, what: &str, args: &Args) -> i32 {
    if let Err(e) = save(store, i) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("initiative-changed", json!({ "id": i.id, "what": what }));
    store.git_commit(&format!("wsp: initiative {} {what}", i.id));
    if args.json() {
        println!("{}", serde_json::to_string_pretty(&i.json()).unwrap_or_default());
    } else {
        println!("{} {what}", i.id);
    }
    0
}

fn add(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative add \"title\" --product P [-p proj]…";
    let title = util::fold(&args.text(1));
    if title.is_empty() {
        eprintln!("usage: {USAGE}");
        return 2;
    }
    if let Some(why) = util::terminal_output(&title) {
        eprintln!("wsp: {why}");
        return 2;
    }
    let Some(prod) = args.get("product") else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let product = match product_of(store, &prod) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let projects = match check_projects(store, &product, &args.all("project")) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let i = Initiative {
        id: next_id(store),
        title,
        product,
        state: "draft".into(),
        projects,
        created: util::now_iso(),
        ..Default::default()
    };
    finish(store, &i, "added", args)
}

fn ls(store: &Store, args: &Args) -> i32 {
    let want = args.get("product");
    let rows: Vec<Initiative> = all(store).into_iter().filter(|i| want.as_deref().map_or(true, |w| i.product == w)).collect();
    if args.json() {
        let out: Vec<_> = rows.iter().map(|i| i.json()).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }
    if rows.is_empty() {
        println!("no initiatives yet — wsp initiative add \"title\" --product P");
        return 0;
    }
    let p = Paint::new();
    let w = rows.iter().map(|r| r.product.chars().count()).max().unwrap_or(7).max(7);
    println!("{}  {}  {}  {}", p.dim(&util::pad("ID", 8)), p.dim(&util::pad("PRODUCT", w)), p.dim(&util::pad("STATE", 12)), p.dim("TITLE"));
    for r in rows {
        println!("{}  {}  {}  {}", util::pad(&r.id, 8), util::pad(&r.product, w), util::pad(&r.state, 12), r.title);
    }
    0
}

fn show(store: &Store, args: &Args) -> i32 {
    let i = match need(store, args, "wsp initiative show <id> [--json]") {
        Ok(i) => i,
        Err(c) => return c,
    };
    if args.json() {
        println!("{}", serde_json::to_string_pretty(&i.json()).unwrap_or_default());
        return 0;
    }
    let p = Paint::new();
    println!("{}  {}", p.bold(&i.id), i.title);
    println!("product {} · {} · projects {} · created {}", i.product, i.state, list_or_dash(&i.projects), i.created);
    println!("worklists {} · tasks {}", list_or_dash(&i.worklists), list_or_dash(&i.tasks));
    println!("artefacts {}", i.artefacts().display());
    let (cs, asm) = (i.criteria(), i.assumptions());
    if !cs.is_empty() {
        println!("\n## Criteria");
        for r in &cs {
            println!("{}", rec_line("- ", r));
        }
    }
    if !asm.is_empty() {
        println!("\n## Assumption records");
        for r in &asm {
            println!("{}", rec_line("- ", r));
        }
    }
    for s in SECTIONS {
        if s == "Decisions" {
            let ds = crate::model::decisions_of(&i.body);
            if !ds.is_empty() {
                println!("\n## Decisions");
                for d in ds {
                    println!("- {} {} {}", d.when, d.id, d.text);
                }
            }
        } else if let Some(t) = i.section(s) {
            println!("\n## {s}\n{t}");
        }
    }
    0
}

fn list_or_dash(v: &[String]) -> String {
    if v.is_empty() { "—".into() } else { v.join(", ") }
}

fn edit(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative edit <id> --brief|--requirements|--questions|--assumptions|--sources [--from F|-]";
    let mut i = match need(store, args, USAGE) {
        Ok(i) => i,
        Err(c) => return c,
    };
    let named: Vec<&str> = SECTIONS.iter().copied().filter(|s| args.has(&s.to_lowercase())).collect();
    if args.has("decisions") {
        eprintln!("wsp: decisions are append-only — wsp decide {} \"…\"", i.id);
        return 2;
    }
    let [section] = named[..] else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let src = match args.get("from") {
        Some(f) if f != "true" => f,
        _ => "-".to_string(),
    };
    let text = match util::read_source(&src) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("wsp: cannot read `{src}`: {e}");
            return 1;
        }
    };
    crate::model::set_section_in_order(&mut i.body, section, &text, &SECTIONS);
    finish(store, &i, &format!("{} edited", section.to_lowercase()), args)
}

fn set(store: &Store, args: &Args) -> i32 {
    let mut i = match need(store, args, "wsp initiative set <id> k=v…") {
        Ok(i) => i,
        Err(c) => return c,
    };
    let pairs = &args.rest[2..];
    if pairs.is_empty() {
        eprintln!("usage: wsp initiative set <id> title=|state=|product=|projects=a,b");
        return 2;
    }
    let mut changed = Vec::new();
    let mut projects: Option<Vec<String>> = None;
    for kv in pairs {
        let Some((k, v)) = kv.split_once('=') else {
            eprintln!("wsp: `{kv}` is not k=v");
            return 2;
        };
        let v = v.trim();
        match k {
            "title" if !v.is_empty() => i.title = util::fold(v),
            "state" => {
                if !STATES.contains(&v) {
                    eprintln!("wsp: `{v}` is not a state — {}", STATES.join(" → "));
                    return 2;
                }
                i.state = v.to_string();
            }
            "product" => match product_of(store, v) {
                Ok(p) => i.product = p,
                Err(e) => {
                    eprintln!("{e}");
                    return 1;
                }
            },
            "projects" => projects = Some(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()),
            _ => {
                eprintln!("wsp: cannot set `{k}` — title, state, product, projects");
                return 2;
            }
        }
        changed.push(k);
    }
    // Checked against the product as it ends up, so `product=` and `projects=`
    // together are read as one change.
    let want = projects.unwrap_or_else(|| i.projects.clone());
    match check_projects(store, &i.product, &want) {
        Ok(p) => i.projects = p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    }
    finish(store, &i, &format!("set {}", changed.join(",")), args)
}

fn link(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative link <id> --list L | --task T  (--clear: remove the one named, or all)";
    let mut i = match need(store, args, USAGE) {
        Ok(i) => i,
        Err(c) => return c,
    };
    let val = |k: &str| args.get(k).filter(|v| v != "true");
    let (list, task) = (val("list"), val("task"));
    let clear = args.has("clear");
    if !clear && list.is_none() && task.is_none() {
        eprintln!("usage: {USAGE}");
        return 2;
    }
    if clear {
        if list.is_none() && task.is_none() {
            i.worklists.clear();
            i.tasks.clear();
        }
        i.worklists.retain(|w| Some(w) != list.as_ref());
        i.tasks.retain(|t| Some(t) != task.as_ref());
        return finish(store, &i, "cleared links", args);
    }
    if let Some(l) = list {
        let Some(w) = store.worklist(&l) else {
            eprintln!("wsp: no worklist `{l}`");
            return 1;
        };
        if !i.worklists.contains(&w.id) {
            i.worklists.push(w.id.clone());
        }
    }
    if let Some(t) = task {
        let Some(t) = store.find_task(&t) else {
            eprintln!("wsp: no task `{t}`");
            return 1;
        };
        if !i.tasks.contains(&t.id) {
            i.tasks.push(t.id.clone());
        }
    }
    finish(store, &i, "linked", args)
}

/// `wsp initiative criterion|assume add|edit|accept|rm <id> …`
fn records(store: &Store, args: &Args, criteria: bool) -> i32 {
    let (noun, letter, sec) = if criteria { ("criterion", 'C', CRIT_SEC) } else { ("assume", 'A', ASSUME_SEC) };
    let usage = if criteria {
        "wsp initiative criterion add <init> \"text\" [--source S] [--status draft|confirmed] | edit <init> C1 [\"text\"] [--source S] [--status …] | rm <init> C1"
    } else {
        "wsp initiative assume add <init> \"text\" [--source S] [--effect N] | edit <init> A1 [\"text\"] [--source S] [--status assumed|accepted|changed] [--effect N] | accept <init> A1 | rm <init> A1"
    };
    let Some(verb) = args.rest.get(1).map(|s| s.as_str()) else {
        eprintln!("usage: {usage}");
        return 2;
    };
    let Some(needle) = args.rest.get(2) else {
        eprintln!("usage: {usage}");
        return 2;
    };
    let Some(mut i) = find(store, needle) else {
        eprintln!("wsp: no initiative `{needle}`");
        return 1;
    };
    let statuses: &[&str] = if criteria { &CRITERIA_STATUS } else { &ASSUME_STATUS };
    let val = |k: &str| args.get(k).filter(|v| v != "true");
    let mut recs = i.recs(sec);
    let status = match val("status") {
        Some(s) if !statuses.contains(&s.as_str()) => {
            eprintln!("wsp: `{s}` is not a status — {}", statuses.join("|"));
            return 2;
        }
        s => s,
    };
    let effect = match val("effect") {
        None => None,
        Some(_) if criteria => {
            eprintln!("wsp: only assumptions have an effect");
            return 2;
        }
        Some(e) => match e.parse::<u32>() {
            Ok(n) if n >= 1 => Some(n),
            _ => {
                eprintln!("wsp: effect is a rank, 1 or more");
                return 2;
            }
        },
    };
    let by = match val("by") {
        Some(b) if b == "agent" || b == "person" => b,
        Some(b) => {
            eprintln!("wsp: `{b}` is not agent|person");
            return 2;
        }
        None => if std::env::var_os("WSP_SEAT_ID").is_some() { "agent" } else { "person" }.to_string(),
    };
    let what;
    match verb {
        "add" => {
            let text = util::fold(&args.text(3));
            if text.is_empty() {
                eprintln!("usage: {usage}");
                return 2;
            }
            if let Some(why) = util::terminal_output(&text) {
                eprintln!("wsp: {why}");
                return 2;
            }
            let n = recs.iter().filter_map(|r| r.id[1..].parse::<u32>().ok()).max().unwrap_or(0);
            let id = format!("{letter}{}", n + 1);
            recs.push(Rec {
                id: id.clone(),
                text,
                source: val("source").unwrap_or_default(),
                status: status.unwrap_or_else(|| statuses[0].to_string()),
                effect,
                by,
            });
            what = format!("{noun} {id} added");
        }
        "edit" | "accept" | "rm" => {
            let Some(rid) = args.rest.get(3).map(|s| s.to_uppercase()) else {
                eprintln!("usage: {usage}");
                return 2;
            };
            let Some(pos) = recs.iter().position(|r| r.id == rid) else {
                eprintln!("wsp: no {noun} `{rid}` on {} — it has {}", i.id, if recs.is_empty() { "none".into() } else { recs.iter().map(|r| r.id.clone()).collect::<Vec<_>>().join(" ") });
                return 1;
            };
            match verb {
                "rm" => {
                    recs.remove(pos);
                    what = format!("{noun} {rid} removed");
                }
                "accept" if criteria => {
                    eprintln!("usage: {usage}");
                    return 2;
                }
                "accept" => {
                    recs[pos].status = "accepted".into();
                    what = format!("{noun} {rid} accepted");
                }
                _ => {
                    let text = util::fold(&args.text(4));
                    let r = &mut recs[pos];
                    if !text.is_empty() {
                        r.text = text;
                    }
                    if let Some(s) = val("source") {
                        r.source = s;
                    }
                    if let Some(s) = status {
                        r.status = s;
                    }
                    if effect.is_some() {
                        r.effect = effect;
                    }
                    if val("by").is_some() {
                        r.by = by;
                    }
                    what = format!("{noun} {rid} edited");
                }
            }
        }
        other => {
            eprintln!("wsp initiative {noun}: unknown `{other}` — {usage}");
            return 2;
        }
    }
    i.put_recs(sec, &recs);
    finish(store, &i, &what, args)
}

/// What an agent popped out on the initiative is told.
pub fn brief_text(store: &Store, i: &Initiative) -> String {
    let mut o = String::new();
    o.push_str(&format!("# {} — {}\nstate: {}\nproduct: {}\n", i.id, i.title, i.state, i.product));
    if !i.projects.is_empty() {
        o.push_str("projects (handbook: wsp project show <p> --handbook):\n");
        for p in &i.projects {
            o.push_str(&format!("  - {p}   wsp project show {p} --handbook\n"));
        }
    }
    if !i.worklists.is_empty() {
        o.push_str(&format!("worklists: {}\n", i.worklists.join(", ")));
    }
    if !i.tasks.is_empty() {
        o.push_str(&format!("tasks: {}\n", i.tasks.join(", ")));
    }
    for (title, recs) in [("Criteria", i.criteria()), ("Assumption records", i.assumptions())] {
        if !recs.is_empty() {
            o.push_str(&format!("\n## {title}\n"));
            for r in &recs {
                o.push_str(&format!("{}\n", rec_line("- ", r)));
            }
        }
    }
    for s in SECTIONS {
        if s == "Decisions" {
            let ds = crate::model::decisions_of(&i.body);
            if !ds.is_empty() {
                o.push_str("\n## Decisions\n");
                for d in ds {
                    o.push_str(&format!("- {} {}\n", d.id, d.text));
                }
            }
        } else if let Some(t) = i.section(s) {
            o.push_str(&format!("\n## {s}\n{t}\n"));
        }
    }
    let _ = store;
    o.push_str(&format!("\nartefacts folder: {}\nrecord a decision: wsp decide {} \"…\"\n", i.artefacts().display(), i.id));
    o
}

fn brief(store: &Store, args: &Args) -> i32 {
    let i = match need(store, args, "wsp initiative brief <id>") {
        Ok(i) => i,
        Err(c) => return c,
    };
    print!("{}", brief_text(store, &i));
    0
}

/// Initiative lines under each product, for `wsp tree`.
pub fn tree_lines(store: &Store) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut m: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for i in all(store) {
        m.entry(i.product.clone()).or_default().push(format!("◇ {}  {} [{}]", i.id, i.title, i.state));
    }
    m
}

/// `decide` on an initiative, if `needle` names one: `None` where it does not.
pub fn decide(store: &Store, needle: &str, text: &str, supersedes: &[String], json: bool) -> Option<i32> {
    let mut i = find(store, needle)?;
    let have: Vec<String> = crate::model::decisions_of(&i.body).into_iter().map(|d| d.id).collect();
    for want in supersedes {
        if !have.contains(want) {
            eprintln!("wsp: no decision `{want}` on {} — it has {}", i.id, if have.is_empty() { "none".into() } else { have.join(" ") });
            return Some(1);
        }
    }
    let id = crate::model::append_decision(&mut i.body, text, supersedes);
    if let Err(e) = save(store, &i) {
        eprintln!("wsp: write failed: {e}");
        return Some(1);
    }
    store.log_event("initiative-decided", json!({ "id": i.id, "text": text }));
    store.git_commit(&format!("wsp: decide {} — {}", i.id, util::truncate(text, 60)));
    if json {
        println!("{}", json!({ "initiative": i.id, "decision": id, "decisions": crate::model::decisions(&i.body).len() }));
    } else {
        println!("◆ {}  {}", i.id, util::truncate(text, 60));
    }
    Some(0)
}

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["initiative"],
    run: initiative,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Projects,
        order: 26,
        text: r##"  wsp initiative add "title" --product P [-p proj]… | ls [--product P] | show <id>
                                    | edit <id> --brief|--requirements|… [--from F|-]
                                    | set <id> k=v… | link <id> --list L|--task T [--clear]
                                    | criterion add|edit|rm <id> …  | assume add|edit|accept|rm <id> …
                                    | brief <id>  a record below a product: artefacts and
                                    decisions (wsp decide init-N "…"), ids init-N"##,
    }],
}];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    fn run(store: &Store, rest: &[&str], flags: &[(&str, &str)]) -> i32 {
        initiative(store, &Args::synth("initiative", rest, flags))
    }

    /// Table-driven over add/show/set/link/decide, and the JSON shape.
    #[test]
    fn an_initiative_is_added_shown_set_linked_and_decided_below_its_product() {
        let store = crate::testkit::store("initiative");
        let mut prod = Project::new("compound");
        prod.kind = "product".into();
        store.save_project(&prod).unwrap();
        for (id, parent) in [("ui", Some("compound")), ("loose", None)] {
            let mut p = Project::new(id);
            p.parent = parent.map(|s| s.to_string());
            store.save_project(&p).unwrap();
        }

        let cases: &[(&[&str], &[(&str, &str)], i32)] = &[
            (&["add", "Initiatives"], &[("product", "compound"), ("project", "ui")], 0),
            (&["add", "Second"], &[("product", "compound")], 0),
            (&["add", "No product"], &[], 2),
            (&["add", "Bad"], &[("product", "ui")], 1),
            (&["add", "Bad"], &[("product", "compound"), ("project", "loose")], 1),
            (&["show", "init-1"], &[], 0),
            (&["show", "9"], &[], 1),
            (&["ls"], &[("json", "true")], 0),
            (&["set", "init-1", "state=requirements", "title=Renamed"], &[], 0),
            (&["set", "init-1", "state=bogus"], &[], 2),
            (&["set", "init-1", "projects=loose"], &[], 1),
            (&["link", "init-1", "--task", "nope"], &[("task", "nope")], 1),
            (&["link", "init-1"], &[], 2),
            (&["edit", "init-1"], &[("requirements", "true"), ("from", "/nonexistent")], 1),
            (&["edit", "init-1"], &[("decisions", "true")], 2),
            (&["brief", "init-1"], &[], 0),
            (&["bogus"], &[], 2),
        ];
        for (rest, flags, want) in cases {
            assert_eq!(run(&store, rest, flags), *want, "{rest:?} {flags:?}");
        }

        let i = find(&store, "init-1").unwrap();
        assert_eq!((i.state.as_str(), i.title.as_str()), ("requirements", "Renamed"));
        assert_eq!(i.projects, vec!["ui"]);
        assert_eq!(find(&store, "2").unwrap().id, "init-2", "ids run on one sequence");

        let src = store.root.join("req.md");
        crate::store::write_atomic(&src, "it must have a record\n").unwrap();
        let from = src.display().to_string();
        assert_eq!(run(&store, &["edit", "init-1"], &[("requirements", "true"), ("from", &from)]), 0);

        assert_eq!(decide(&store, "init-1", "ids are init-N", &[], false), Some(0));
        assert_eq!(decide(&store, "init-1", "again", &["d9".into()], false), Some(1));
        assert_eq!(decide(&store, "t-nope", "x", &[], false), None);

        let v = find(&store, "init-1").unwrap().json();
        for k in [
            "id", "title", "product", "state", "projects", "worklists", "tasks", "created", "artefacts", "brief",
            "requirements", "decisions", "questions", "assumptions", "sources",
        ] {
            assert!(v.get(k).is_some(), "json lacks {k}");
        }
        assert_eq!(v["requirements"], json!("it must have a record"));
        assert_eq!(v["decisions"][0]["id"], json!("d1"));
        assert_eq!(v["decisions"][0]["text"], json!("ids are init-N"));
        assert!(v["artefacts"].as_str().unwrap().ends_with("init-1"));
        assert!(brief_text(&store, &find(&store, "init-1").unwrap()).contains("wsp project show ui --handbook"));
        assert_eq!(tree_lines(&store)["compound"].len(), 2);
    }

    /// Table-driven over criterion and assume: add/edit/accept/rm, bad input, and where they show.
    #[test]
    fn criteria_and_assumptions_are_records_shown_in_json_and_the_brief() {
        let store = crate::testkit::store("initiative-recs");
        let mut prod = Project::new("compound");
        prod.kind = "product".into();
        store.save_project(&prod).unwrap();
        assert_eq!(run(&store, &["add", "X"], &[("product", "compound")]), 0);

        let cases: &[(&[&str], &[(&str, &str)], i32)] = &[
            (&["criterion", "add", "init-1", "has records"], &[("source", "doc.md")], 0),
            (&["criterion", "add", "init-1", "second"], &[("by", "agent")], 0),
            (&["criterion", "add", "init-1", "bad"], &[("status", "accepted")], 2),
            (&["criterion", "add", "init-1", "bad"], &[("effect", "2")], 2),
            (&["criterion", "add", "init-1", "bad"], &[("by", "robot")], 2),
            (&["criterion", "add", "init-1"], &[], 2),
            (&["criterion", "add", "init-9", "x"], &[], 1),
            (&["criterion", "edit", "init-1", "C1", "renamed"], &[("status", "confirmed")], 0),
            (&["criterion", "edit", "init-1", "C7"], &[], 1),
            (&["criterion", "accept", "init-1", "C1"], &[], 2),
            (&["criterion", "rm", "init-1", "C2"], &[], 0),
            (&["assume", "add", "init-1", "users have git, with commas"], &[("effect", "1")], 0),
            (&["assume", "add", "init-1", "second"], &[("effect", "zero")], 2),
            (&["assume", "edit", "init-1", "A1"], &[("status", "changed")], 0),
            (&["assume", "accept", "init-1", "a1"], &[], 0),
            (&["assume", "rm", "init-1", "A5"], &[], 1),
            (&["assume", "bogus", "init-1"], &[], 2),
            (&["assume"], &[], 2),
            (&["show", "init-1"], &[], 0),
            (&["brief", "init-1"], &[], 0),
        ];
        for (rest, flags, want) in cases {
            assert_eq!(run(&store, rest, flags), *want, "{rest:?} {flags:?}");
        }
        let i = find(&store, "init-1").unwrap();
        let c = i.criteria();
        assert_eq!((c.len(), c[0].id.as_str(), c[0].text.as_str(), c[0].status.as_str(), c[0].source.as_str()), (1, "C1", "renamed", "confirmed", "doc.md"));
        let a = i.assumptions();
        assert_eq!((a[0].status.as_str(), a[0].effect, a[0].text.as_str()), ("accepted", Some(1), "users have git, with commas"));
        let v = i.json();
        assert_eq!(v["criteria"][0]["id"], json!("C1"));
        assert_eq!(v["assumption_records"][0]["effect"], json!(1));
        assert!(["person", "agent"].contains(&v["criteria"][0]["by"].as_str().unwrap()));
        let b = brief_text(&store, &i);
        assert!(b.contains("C1 [confirmed") && b.contains("A1 [accepted effect 1"), "{b}");
        run(&store, &["assume", "add", "init-1", "again"], &[]);
        assert_eq!(find(&store, "init-1").unwrap().assumptions()[1].id, "A2");
    }
}
