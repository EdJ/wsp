//! `wsp initiative` (`wsp-722`): a record below a product — a repository of
//! artefacts and decisions that leads to planning and is linked to many
//! worklists and tasks.
//!
//! One file each at `<store>/initiatives/init-N.md`, ids from one global
//! sequence (compound's artefact owner is `init-N`, folder
//! `~/.compound/artefacts/init-N/`). `worklists` and `tasks` are links; the
//! plan is `## Increments` (`wsp-731`), its order kept by the worklists'
//! `after` relation, and read back as one graph by `plan show`.

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
            "increments": self.increments().iter().map(|r| r.json()).collect::<Vec<_>>(),
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
    /// Criteria only: the checks recorded against it (`--check`), what the
    /// align reads as `covered`.
    pub checks: Vec<String>,
}

pub const CRITERIA_STATUS: [&str; 2] = ["draft", "confirmed"];
pub const ASSUME_STATUS: [&str; 3] = ["assumed", "accepted", "changed"];
const CRIT_SEC: &str = "Criteria";
const ASSUME_SEC: &str = "Assumption records";
const INC_SEC: &str = "Increments";
const ALL_SECTIONS: [&str; 9] = ["Brief", "Requirements", CRIT_SEC, INC_SEC, "Decisions", "Questions", "Assumptions", ASSUME_SEC, "Sources"];

/// One step of an initiative's plan (`wsp-731`): the criteria it delivers and
/// the worklists it owns, one JSON line each in `## Increments`, in plan
/// order. The order is not kept here a second time: the plan's lists are one
/// chain of the worklist `after` relation ([`sync_after`]), which is what the
/// queue reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Increment {
    pub id: String,
    pub caption: String,
    pub criteria: Vec<String>,
    pub lists: Vec<String>,
    /// What the person said at the align (`wsp-793`): passed, or sent back.
    pub aligned: Option<Align>,
}

impl Increment {
    fn json(&self) -> serde_json::Value {
        let mut v = json!({ "id": self.id, "caption": self.caption, "criteria": self.criteria, "lists": self.lists });
        if let Some(a) = &self.aligned {
            v["aligned"] = a.json();
        }
        v
    }
}

/// The person's word at an increment's align, kept on its `## Increments`
/// line: who, when, the criteria as they stood, the commits the increment's
/// lists put on the trunk, and the verdict (or, sent back, the note).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Align {
    /// `passed` or `back`.
    pub state: String,
    pub by: String,
    pub at: String,
    /// A short hash of the criteria text and their checks when it was said.
    pub criteria: String,
    pub commits: Vec<String>,
    pub text: String,
}

impl Align {
    fn json(&self) -> serde_json::Value {
        json!({ "state": self.state, "by": self.by, "at": self.at, "criteria_version": self.criteria, "commits": self.commits, "text": self.text })
    }
    fn from_json(v: &serde_json::Value) -> Option<Align> {
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        v.is_object().then(|| Align {
            state: s("state"),
            by: s("by"),
            at: s("at"),
            criteria: s("criteria_version"),
            commits: v["commits"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            text: s("text"),
        })
    }
}

/// The gate after a group in the plan graph. Every gate is a barrier today;
/// `align` is reserved for the last group of an increment, where the
/// increment's lists meet before the next increment starts.
pub const GATE_BARRIER: &str = "barrier";
pub const GATE_ALIGN: &str = "align";

impl Rec {
    fn json(&self) -> serde_json::Value {
        let mut v = json!({ "id": self.id, "text": self.text, "source": self.source, "status": self.status, "effect": self.effect, "by": self.by });
        if !self.checks.is_empty() {
            v["checks"] = json!(self.checks);
        }
        v
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
                checks: v["checks"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            })
            .collect()
    }
    pub fn criteria(&self) -> Vec<Rec> {
        self.recs(CRIT_SEC)
    }
    pub fn assumptions(&self) -> Vec<Rec> {
        self.recs(ASSUME_SEC)
    }
    pub fn increments(&self) -> Vec<Increment> {
        let Some(t) = self.section(INC_SEC) else { return vec![] };
        let strs = |v: &serde_json::Value| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        t.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
            .map(|v| Increment {
                id: v["id"].as_str().unwrap_or("").into(),
                caption: v["caption"].as_str().unwrap_or("").into(),
                criteria: strs(&v["criteria"]),
                lists: strs(&v["lists"]),
                aligned: Align::from_json(&v["aligned"]),
            })
            .collect()
    }
    fn put_increments(&mut self, v: &[Increment]) {
        let text = v.iter().map(|r| r.json().to_string()).collect::<Vec<_>>().join("\n");
        crate::model::set_section_in_order(&mut self.body, INC_SEC, &text, &ALL_SECTIONS);
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
        "increment" | "inc" => increment(store, args),
        "plan" => plan(store, args),
        "approve" => approve(store, args),
        "changes" => changes(store, args),
        "align" => align(store, args),
        "run" => crate::initiative_run::run(store, args),
        other => {
            eprintln!("wsp initiative: unknown subcommand `{other}` (add|ls|show|edit|set|link|brief|criterion|assume|increment|plan|approve|changes|align|run)");
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
        println!("{}", serde_json::to_string_pretty(&json_with_aligns(store, i)).unwrap_or_default());
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
        println!("{}", serde_json::to_string_pretty(&json_with_aligns(store, &i)).unwrap_or_default());
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
        "wsp initiative criterion add <init> \"text\" [--source S] [--status draft|confirmed] [--check \"…\"]… | edit <init> C1 [\"text\"] [--source S] [--status …] [--check \"…\"]… | rm <init> C1"
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
                checks: args.all("check").into_iter().filter(|c| c != "true" && !c.trim().is_empty()).collect(),
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
                    for c in args.all("check").into_iter().filter(|c| c != "true" && !c.trim().is_empty()) {
                        if !r.checks.contains(&c) {
                            r.checks.push(c);
                        }
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

/// `wsp initiative increment add|edit|rm <id> …`
fn increment(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative increment add <init> [\"caption\"] [--criteria C1,C2] [--list L]… | edit <init> I1 [\"caption\"] [--criteria …] [--list …] | rm <init> I1";
    let (Some(verb), Some(needle)) = (args.rest.get(1).map(|s| s.as_str()), args.rest.get(2)) else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let Some(mut i) = find(store, needle) else {
        eprintln!("wsp: no initiative `{needle}`");
        return 1;
    };
    let before = i.increments();
    let mut incs = before.clone();
    let criteria = args.has("criteria").then(|| args.all("criteria").into_iter().map(|c| c.to_uppercase()).collect::<Vec<_>>());
    if let Some(cs) = &criteria {
        let have: Vec<String> = i.criteria().into_iter().map(|r| r.id).collect();
        if let Some(bad) = cs.iter().find(|c| !have.contains(c)) {
            eprintln!("wsp: no criterion `{bad}` on {} — it has {}", i.id, if have.is_empty() { "none".into() } else { have.join(" ") });
            return 1;
        }
    }
    let lists = if args.has("list") {
        let mut out = Vec::new();
        for l in args.all("list") {
            let Some(w) = store.worklist(&l) else {
                eprintln!("wsp: no worklist `{l}`");
                return 1;
            };
            if !out.contains(&w.id) {
                out.push(w.id);
            }
        }
        Some(out)
    } else {
        None
    };
    let what;
    let mut touched = None;
    match verb {
        "add" => {
            let caption = util::fold(&args.text(3));
            if let Some(why) = util::terminal_output(&caption) {
                eprintln!("wsp: {why}");
                return 2;
            }
            let n = incs.iter().filter_map(|r| r.id.get(1..)?.parse::<u32>().ok()).max().unwrap_or(0);
            let id = format!("I{}", n + 1);
            incs.push(Increment { id: id.clone(), caption, criteria: criteria.unwrap_or_default(), lists: lists.clone().unwrap_or_default(), ..Increment::default() });
            touched = Some(incs.len() - 1);
            what = format!("increment {id} added");
        }
        "edit" | "rm" => {
            let Some(rid) = args.rest.get(3).map(|s| s.to_uppercase()) else {
                eprintln!("usage: {USAGE}");
                return 2;
            };
            let Some(pos) = incs.iter().position(|r| r.id == rid) else {
                eprintln!("wsp: no increment `{rid}` on {} — it has {}", i.id, if incs.is_empty() { "none".into() } else { incs.iter().map(|r| r.id.clone()).collect::<Vec<_>>().join(" ") });
                return 1;
            };
            if verb == "rm" {
                incs.remove(pos);
                what = format!("increment {rid} removed");
            } else {
                let caption = util::fold(&args.text(4));
                if let Some(why) = util::terminal_output(&caption) {
                    eprintln!("wsp: {why}");
                    return 2;
                }
                let r = &mut incs[pos];
                if !caption.is_empty() {
                    r.caption = caption;
                }
                if let Some(c) = criteria {
                    r.criteria = c;
                }
                if let Some(l) = &lists {
                    r.lists = l.clone();
                }
                touched = Some(pos);
                what = format!("increment {rid} edited");
            }
        }
        other => {
            eprintln!("wsp initiative increment: unknown `{other}` — {USAGE}");
            return 2;
        }
    }
    // A list is owned by one increment: two would put it after itself.
    if let (Some(at), Some(l)) = (touched, &lists) {
        for w in l {
            if let Some(other) = incs.iter().enumerate().find(|(k, r)| *k != at && r.lists.contains(w)) {
                eprintln!("wsp: `{w}` is already in increment {} — a list belongs to one increment", other.1.id);
                return 1;
            }
        }
    }
    // What a list of the plan is, the initiative links: `increment --list`
    // is a `link --list` too.
    for inc in &incs {
        for w in &inc.lists {
            if !i.worklists.contains(w) {
                i.worklists.push(w.clone());
            }
        }
    }
    if let Err(why) = sync_after(store, &before, &incs) {
        eprintln!("{why}");
        return 1;
    }
    i.put_increments(&incs);
    finish(store, &i, &what, args)
}

/// The plan's lists ordered by increment through `after`: every list of an
/// increment waits on *all* the lists of the nearest earlier increment that
/// has any (`wsp-734`), so two lists of one increment run side by side and the
/// next increment starts only when both are done. Lists inside one increment
/// wait on nothing the plan says. `after` is wsp's to keep for a planned list:
/// an edit of one increment never undoes it. A list the plan let go of is let
/// go by its `after` too, where that named a list of the plan. A chain that
/// would come back round is refused before anything is written.
fn sync_after(store: &Store, before: &[Increment], after: &[Increment]) -> Result<(), String> {
    let mut want: Vec<(String, Vec<String>)> = Vec::new();
    let mut prev: Vec<String> = Vec::new();
    for inc in after {
        for l in &inc.lists {
            if !want.iter().any(|(w, _)| w == l) {
                want.push((l.clone(), prev.clone()));
            }
        }
        if !inc.lists.is_empty() {
            prev = inc.lists.clone();
        }
    }
    let planned: Vec<&String> = before.iter().chain(after).flat_map(|r| &r.lists).collect();
    for l in before.iter().flat_map(|r| &r.lists) {
        if !want.iter().any(|(w, _)| w == l) {
            if let Some(w) = store.worklist(l) {
                if w.after.iter().any(|a| planned.contains(&a)) {
                    want.push((l.clone(), w.after.iter().filter(|a| !planned.contains(a)).cloned().collect()));
                }
            }
        }
    }
    // The graph each list would stand in, read with the new edges in place.
    let edge = |id: &str| -> Vec<String> {
        match want.iter().find(|(w, _)| w == id) {
            Some((_, a)) => a.clone(),
            None => store.worklist(id).map(|w| w.after).unwrap_or_default(),
        }
    };
    for (l, a) in &want {
        let mut seen: Vec<String> = Vec::new();
        let mut todo: Vec<String> = a.clone();
        while let Some(x) = todo.pop() {
            if &x == l {
                return Err(format!("wsp: `{l}` after `{}` would wait for itself", a.join(", ")));
            }
            if seen.contains(&x) || seen.len() > 256 {
                continue;
            }
            todo.extend(edge(&x));
            seen.push(x);
        }
    }
    store.locked(|| -> Result<(), String> {
        for (l, a) in &want {
            let Some(mut w) = store.worklist(l) else { continue };
            if &w.after == a {
                continue;
            }
            w.after = a.clone();
            w.log(&match a.is_empty() {
                false => format!("after {} (initiative plan)", a.join(", ")),
                true => "no longer after any list (initiative plan)".to_string(),
            });
            store.save_worklist(&w).map_err(|e| format!("wsp: write failed: {e}"))?;
            store.log_event("worklist-edited", json!({ "id": w.id, "what": "after", "after": w.after }));
        }
        Ok(())
    })
}

/// The plan as one graph (`wsp-731`): increments → worklists → groups → rows,
/// each node with what the web chrome draws, so it needs no second call per
/// list. A list linked to the initiative and in no increment is `unplanned`.
pub fn plan_graph(store: &Store, i: &Initiative) -> serde_json::Value {
    let crit = i.criteria();
    let incs = i.increments();
    let (mut n_lists, mut n_groups, mut n_rows) = (0usize, 0usize, 0usize);
    let task_caption = |m: &str| store.task(m).map(|t| crate::caption::of_task(&t));
    let mut list_json = |id: &str, last_of_increment: bool| -> serde_json::Value {
        let Some(w) = store.worklist(id) else {
            return json!({ "id": id, "missing": true, "groups": [] });
        };
        n_lists += 1;
        let groups = w.groups();
        let ng = groups.len();
        let gs: Vec<_> = groups
            .iter()
            .enumerate()
            .map(|(gi, g)| {
                n_groups += 1;
                let rows: Vec<_> = g
                    .members
                    .iter()
                    .map(|m| {
                        n_rows += 1;
                        let pol = g.policy_for(m);
                        let tier = pol.as_ref().map(|p| json!({ "kind": p.kind, "model": p.model, "effort": p.effort }));
                        match store.task(m) {
                            Some(t) => json!({
                                "id": t.id, "title": t.title, "caption": crate::caption::of_task(&t).text,
                                "tier": tier, "model": pol.as_ref().map(|p| p.label()),
                                "status": t.status().as_str(), "after": t.after,
                            }),
                            None => json!({
                                "id": m, "title": null, "caption": m, "tier": tier, "model": pol.as_ref().map(|p| p.label()),
                                "status": "missing", "after": [],
                            }),
                        }
                    })
                    .collect();
                let end = last_of_increment && gi + 1 == ng;
                json!({
                    "id": g.id, "ordinal": gi + 1,
                    "caption": crate::caption::of_group(g, &task_caption).text,
                    "kind": g.kind, "agent": g.agent,
                    "gate": if end { GATE_ALIGN } else { GATE_BARRIER }, "increment_end": end,
                    "rows": rows,
                })
            })
            .collect();
        json!({
            "id": w.id, "title": w.title, "caption": crate::caption::of_worklist(&w).text,
            "status": w.status().as_str(), "after": w.after, "groups": gs,
        })
    };
    let mut out_incs = Vec::new();
    for (k, inc) in incs.iter().enumerate() {
        let lists: Vec<_> = inc.lists.iter().enumerate().map(|(li, l)| list_json(l, li + 1 == inc.lists.len())).collect();
        let criteria: Vec<_> = inc
            .criteria
            .iter()
            .map(|c| match crit.iter().find(|r| &r.id == c) {
                Some(r) => json!({ "id": r.id, "text": r.text, "status": r.status }),
                None => json!({ "id": c, "text": null, "status": "missing" }),
            })
            .collect();
        out_incs.push(json!({ "id": inc.id, "ordinal": k + 1, "caption": inc.caption, "criteria": criteria, "lists": lists, "align": align_json(store, i, inc) }));
    }
    let planned: Vec<&String> = incs.iter().flat_map(|r| &r.lists).collect();
    let unplanned: Vec<_> = i.worklists.iter().filter(|w| !planned.contains(w)).map(|w| list_json(w, false)).collect();
    json!({
        "initiative": i.id, "title": i.title, "state": i.state,
        "increments": out_incs,
        "unplanned": unplanned,
        "totals": { "increments": incs.len(), "lists": n_lists, "groups": n_groups, "rows": n_rows },
    })
}

/// The planner's work order lives in docs (`wsp-733`), compiled in: its
/// `## The order` section, to the end of the file, is what the seat reads.
const PLAN_DOC: &str = include_str!("../../../docs/initiative-plan.md");
/// The model a planner sits on when `--model` names none.
pub const PLANNER_MODEL: &str = "opus";

/// The body of `doc`'s `## The order` section: every line after that heading
/// up to the next `## ` heading (its `###` subheads stay in).
pub fn order_section(doc: &str) -> String {
    let mut out = Vec::new();
    let mut on = false;
    for l in doc.lines() {
        if l.starts_with("## ") {
            if on {
                break;
            }
            on = l.trim_end() == "## The order";
            continue;
        }
        if on {
            out.push(l);
        }
    }
    out.join("\n").trim().to_string()
}

/// The planner's work order for `i`: the doc's order, then the brief, then a
/// pending change request, which makes it a replan of the plan that exists.
pub fn plan_order(store: &Store, i: &Initiative, changes: Option<&str>) -> String {
    let mut o = format!("# Plan · {} · {}\n\n", i.id, i.title);
    let replan = changes.is_some_and(|c| !c.trim().is_empty());
    if replan {
        o.push_str(&format!("This is a replan of {}: the person asked for changes (below). Edit the lists, rows and increments that exist; file nothing that duplicates them.\n\n", i.id));
    } else {
        o.push_str(&format!("You plan {}. Nothing here is a task to implement.\n\n", i.id));
    }
    o.push_str(&order_section(PLAN_DOC));
    o.push_str("\n\n---\n\n## The brief (wsp initiative brief ");
    o.push_str(&i.id);
    o.push_str(")\n\n");
    o.push_str(brief_text(store, i).trim_end());
    o.push('\n');
    if replan {
        o.push_str(&format!("\n## The plan now (wsp initiative plan show {})\n\n", i.id));
        o.push_str(plan_text(&plan_graph(store, i)).trim_end());
        o.push_str("\n\n## Change request\n\n");
        o.push_str(changes.unwrap_or("").trim());
        o.push('\n');
    }
    o
}

/// `wsp initiative plan <id> [--changes "…"|-] [--model M] [--dry-run]`: file
/// a planning row carrying the order, link it, and seat a planner on it.
fn plan_seat(store: &Store, args: &Args, needle: &str) -> i32 {
    let Some(mut i) = find(store, needle) else {
        eprintln!("wsp: no initiative `{needle}`");
        return 1;
    };
    let mut stored: Option<usize> = None;
    let changes = match args.get("changes") {
        None => match newest_change(&i) {
            Some((n, text)) => {
                stored = Some(n);
                Some(text)
            }
            None => None,
        },
        Some(c) if c == "true" => {
            eprintln!("wsp: --changes takes the request: --changes \"…\" or --changes - (stdin)");
            return 2;
        }
        Some(c) if c == "-" => match util::read_source("-") {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("wsp: cannot read stdin: {e}");
                return 1;
            }
        },
        Some(c) => Some(c),
    };
    if matches!(i.state.as_str(), "building" | "closed") {
        eprintln!("wsp: {} is {} — a plan is made before the build, not during it", i.id, i.state);
        return 1;
    }
    let model = args.get("model").filter(|m| !m.is_empty() && m != "true").unwrap_or_else(|| PLANNER_MODEL.to_string());
    let order = plan_order(store, &i, changes.as_deref());
    if args.has("dry-run") {
        print!("{order}");
        println!("\n(dry run: would file a planning row in {} and seat claude {model} on it; nothing filed, nothing seated)", i.projects.first().map(String::as_str).unwrap_or("the inbox"));
        return 0;
    }
    let project = i.projects.first().cloned();
    let id = match store.alloc_task_id(project.as_deref()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("wsp: cannot allocate id: {e}");
            return 1;
        }
    };
    let verb = if changes.is_some() { "Replan" } else { "Plan" };
    let mut t = crate::model::Task::new(&format!("{verb} {}: {}", i.id, i.title), &id);
    t.project = project.clone();
    t.tags = vec!["plan".into()];
    if project.is_none() {
        t.status_raw = "inbox".into();
    }
    crate::model::set_section_in(&mut t.body, "Overview", order.trim());
    if let Err(e) = store.save_task(&t) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("task-added", json!({ "id": t.id, "project": t.project, "title": t.title }));
    if !i.tasks.contains(&t.id) {
        i.tasks.push(t.id.clone());
    }
    if let Err(e) = save(store, &i) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    if let Some(n) = stored {
        let mut j = find(store, &i.id).unwrap_or_else(|| i.clone());
        mark_change_answered(&mut j, n, &t.id);
        if let Err(e) = save(store, &j) {
            eprintln!("wsp: write failed: {e}");
            return 1;
        }
    }
    store.log_event("initiative-plan", json!({ "id": i.id, "row": t.id, "replan": changes.is_some(), "model": model }));
    store.git_commit(&format!("wsp: {} {} — planner row {}", verb.to_lowercase(), i.id, t.id));
    println!("◇ {}  planner row {} · seating claude {model}", i.id, t.id);
    seat_planner(&t.id, &model)
}

const CHANGE_MARK: &str = "- change ";

/// The change requests `changes` stored in `## Questions`: `(n, answered, text)`.
/// One line each: `- change N: text`, or `- change N ✓ <row>: text` once a plan
/// row has taken it up.
fn change_requests(i: &Initiative) -> Vec<(usize, bool, String)> {
    let Some(q) = i.section("Questions") else { return vec![] };
    q.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix(CHANGE_MARK)?;
            let (head, text) = rest.split_once(": ")?;
            let n = head.split_whitespace().next()?.parse().ok()?;
            Some((n, head.contains('✓'), text.to_string()))
        })
        .collect()
}

/// The newest change request no plan row has taken up yet.
fn newest_change(i: &Initiative) -> Option<(usize, String)> {
    change_requests(i).into_iter().filter(|(_, done, _)| !done).max_by_key(|(n, _, _)| *n).map(|(n, _, t)| (n, t))
}

fn mark_change_answered(i: &mut Initiative, n: usize, row: &str) {
    let q = i.section("Questions").unwrap_or_default();
    let from = format!("{CHANGE_MARK}{n}: ");
    let to = format!("{CHANGE_MARK}{n} ✓ {row}: ");
    let text = q.lines().map(|l| if l.trim().starts_with(&from) { l.replacen(&from, &to, 1) } else { l.to_string() }).collect::<Vec<_>>().join("\n");
    crate::model::set_section_in_order(&mut i.body, "Questions", &text, &ALL_SECTIONS);
}

/// Store a change request in `## Questions`; its number.
fn add_change(i: &mut Initiative, text: &str) -> usize {
    let n = change_requests(i).into_iter().map(|(n, _, _)| n).max().unwrap_or(0) + 1;
    let mut q = i.section("Questions").unwrap_or_default().trim().to_string();
    if !q.is_empty() {
        q.push('\n');
    }
    q.push_str(&format!("{CHANGE_MARK}{n}: {text}"));
    crate::model::set_section_in_order(&mut i.body, "Questions", &q, &ALL_SECTIONS);
    n
}

/// `wsp initiative changes <id> "what to change"`: Ask for changes. Stores the
/// request as a question, leaves the state `planned`, starts nothing.
fn changes(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative changes <id> \"what to change\"";
    let mut i = match need(store, args, USAGE) {
        Ok(i) => i,
        Err(c) => return c,
    };
    let text = args.rest[2..].join(" ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        eprintln!("usage: {USAGE}");
        return 2;
    }
    if i.state != "planned" {
        eprintln!("wsp: {} is {} — changes are asked of a plan, and only a planned initiative has one waiting", i.id, i.state);
        return 1;
    }
    let n = add_change(&mut i, &text);
    if let Err(e) = save(store, &i) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("initiative-changes", json!({ "id": i.id, "n": n }));
    store.git_commit(&format!("wsp: changes asked of {} — {}", i.id, util::truncate(&text, 60)));
    if args.json() {
        println!("{}", json!({ "initiative": i.id, "change": n, "state": i.state }));
    } else {
        println!("◇ {}  changes asked (#{n}) — `wsp initiative plan {}` replans from it", i.id, i.id);
    }
    0
}

/// `wsp initiative approve <id> "verdict"`: approve for build. Records the
/// verdict, accepts the assumptions and confirms the criteria, moves the state
/// planned → building and marks the plan's lists `planned` for the pickup.
/// Nothing starts here: a list is `planned`, never `executing`, after it.
fn approve(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative approve <id> \"verdict\"";
    let mut i = match need(store, args, USAGE) {
        Ok(i) => i,
        Err(c) => return c,
    };
    let verdict = args.rest[2..].join(" ");
    let verdict = verdict.trim();
    if verdict.is_empty() {
        eprintln!("usage: {USAGE}");
        return 2;
    }
    if i.state != "planned" {
        eprintln!("wsp: {} is {} — only a planned initiative is approved for build", i.id, i.state);
        return 1;
    }
    // The lists in plan order (increment 1's first), then any linked but unplanned.
    let mut lists: Vec<String> = Vec::new();
    for l in i.increments().iter().flat_map(|r| r.lists.iter()).chain(i.worklists.iter()) {
        if !lists.contains(l) {
            lists.push(l.clone());
        }
    }
    for l in &lists {
        if let Some(w) = store.worklist(l) {
            if w.status() != crate::model::WorklistStatus::Draft {
                eprintln!("wsp: {} has a list that already started ({l}) — approve a plan before its build begins", i.id);
                return 1;
            }
        }
    }
    let crit: Vec<Rec> = i.criteria().into_iter().map(|mut r| { if r.status == "draft" { r.status = "confirmed".into(); } r }).collect();
    let asm: Vec<Rec> = i.assumptions().into_iter().map(|mut r| { if r.status == "assumed" { r.status = "accepted".into(); } r }).collect();
    if !crit.is_empty() {
        i.put_recs(CRIT_SEC, &crit);
    }
    if !asm.is_empty() {
        i.put_recs(ASSUME_SEC, &asm);
    }
    crate::model::append_decision(&mut i.body, &format!("approved for build: {verdict}"), &[]);
    i.state = "building".into();
    let mut marked: Vec<String> = Vec::new();
    let res = store.locked(|| -> Result<(), String> {
        for l in &lists {
            let Some(mut w) = store.worklist(l) else { continue };
            if w.is_planned() {
                continue;
            }
            w.set_planned();
            w.log(&format!("status draft → planned — {} approved for build", i.id));
            store.save_worklist(&w).map_err(|e| format!("wsp: write failed: {e}"))?;
            store.log_event("worklist-edited", json!({ "id": w.id, "what": "status", "status": "planned" }));
            marked.push(l.clone());
        }
        Ok(())
    });
    if let Err(e) = res {
        eprintln!("{e}");
        return 1;
    }
    if let Err(e) = save(store, &i) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("initiative-approved", json!({ "id": i.id, "lists": marked }));
    store.git_commit(&format!("wsp: approve {} — {}", i.id, util::truncate(verdict, 60)));
    if args.json() {
        println!("{}", json!({ "initiative": i.id, "state": i.state, "planned": marked }));
    } else {
        println!("◆ {}  approved for build — {} list(s) planned", i.id, marked.len());
    }
    0
}

/// `wsp spawn <row> --agent --model M` as a child of this binary, the way the
/// run starts a seat ([`crate::cycle`]'s `start`): the engine does not call up
/// into `cmd_spawn` (tests/layering.rs), and the child's output is the caller's.
fn seat_planner(row: &str, model: &str) -> i32 {
    // A test's binary is the harness: running it would run the suite again.
    if cfg!(test) {
        return 0;
    }
    let exe = match wsp_base::util::self_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("wsp: no path to this binary: {e}");
            return 1;
        }
    };
    match std::process::Command::new(exe).args(["spawn", row, "--agent", "--model", model]).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("wsp: could not start `wsp spawn {row}`: {e}");
            1
        }
    }
}

/// `wsp initiative plan show <id> [--json]` · `wsp initiative plan <id> …`
fn plan(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative plan show <id> [--json] | plan <id> [--changes \"…\"|-] [--model M] [--dry-run]";
    let (Some("show"), Some(needle)) = (args.rest.get(1).map(|s| s.as_str()), args.rest.get(2)) else {
        if let Some(needle) = args.rest.get(1).filter(|s| s.as_str() != "show") {
            return plan_seat(store, args, needle);
        }
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let Some(i) = find(store, needle) else {
        eprintln!("wsp: no initiative `{needle}`");
        return 1;
    };
    let g = plan_graph(store, &i);
    if args.json() {
        println!("{}", serde_json::to_string_pretty(&g).unwrap_or_default());
        return 0;
    }
    print!("{}", plan_text(&g));
    0
}

/// The "planned" board, compact: an increment a block, a list a line, a group
/// a line under it, its rows indented below.
pub fn plan_text(g: &serde_json::Value) -> String {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("").to_string();
    let t = &g["totals"];
    let mut o = format!(
        "{}  {}  [{}] · {} increments · {} lists · {} groups · {} rows\n",
        s(&g["initiative"]), s(&g["title"]), s(&g["state"]), t["increments"], t["lists"], t["groups"], t["rows"]
    );
    let list = |o: &mut String, l: &serde_json::Value| {
        if l["missing"] == json!(true) {
            o.push_str(&format!("  {}  (no such worklist)\n", s(&l["id"])));
            return;
        }
        let on: Vec<&str> = l["after"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
        let after = if on.is_empty() { String::new() } else { format!(" · after {}", on.join(", ")) };
        o.push_str(&format!("  {}  {} [{}]{after}\n", s(&l["id"]), s(&l["caption"]), s(&l["status"])));
        for gr in l["groups"].as_array().into_iter().flatten() {
            let kind = if s(&gr["kind"]).is_empty() { String::new() } else { format!(" · {}", s(&gr["kind"])) };
            let agent = if s(&gr["agent"]).is_empty() { String::new() } else { format!(" · {}", s(&gr["agent"])) };
            o.push_str(&format!("    g{}  {}{kind}{agent}\n", gr["ordinal"], s(&gr["caption"])));
            for r in gr["rows"].as_array().into_iter().flatten() {
                let after: Vec<String> = r["after"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
                let after = if after.is_empty() { String::new() } else { format!(" · after {}", after.join(",")) };
                o.push_str(&format!("      {}  {} [{}]{after}\n", s(&r["id"]), s(&r["caption"]), s(&r["status"])));
            }
            o.push_str(&format!("      ── {}\n", s(&gr["gate"])));
        }
    };
    for inc in g["increments"].as_array().into_iter().flatten() {
        let cs: Vec<String> = inc["criteria"].as_array().into_iter().flatten().map(|c| s(&c["id"])).collect();
        let cs = if cs.is_empty() { String::new() } else { format!("  delivers {}", cs.join(" ")) };
        o.push_str(&format!("\n{}  {}{cs}\n", s(&inc["id"]), s(&inc["caption"])));
        for l in inc["lists"].as_array().into_iter().flatten() {
            list(&mut o, l);
        }
    }
    let un = g["unplanned"].as_array().cloned().unwrap_or_default();
    if !un.is_empty() {
        o.push_str("\nunplanned (linked, in no increment)\n");
        for l in &un {
            list(&mut o, l);
        }
    }
    o
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

// ---- the align (wsp-793) ------------------------------------------------
//
// When every list of increment n has finished, n stands at its align: its
// stop list is n's criteria, and the lists of n+1 wait for the person's word.
// The word is kept on n's `## Increments` line ([`Align`]); whether n is
// running, standing or has been answered is read, never stored.

/// Where an increment stands at its align.
pub struct AlignState {
    /// `running`, `standing`, `passed` or `sent back`.
    pub word: &'static str,
    /// When it came to stand there, where that is known.
    pub since: Option<String>,
    pub done: usize,
    pub total: usize,
}

fn list_complete(store: &Store, id: &str) -> bool {
    use crate::worklist::ListStatus;
    store
        .worklist(id)
        .and_then(|w| crate::worklist::listing(store, vec![w]).into_iter().next())
        .is_some_and(|l| matches!(l.phase(store).status, ListStatus::Complete | ListStatus::Closed))
}

/// The newest dated line in a list's log: when its last thing happened.
fn last_stamp(store: &Store, id: &str) -> Option<String> {
    let w = store.worklist(id)?;
    w.section("Log")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- ").and_then(|r| r.split_once(' ')).map(|(s, _)| s.to_string()))
        .filter(|s| util::is_stamp(s))
        .max_by_key(|s| util::epoch_of(s))
}

pub fn align_state(store: &Store, inc: &Increment) -> AlignState {
    let total = inc.lists.len();
    let done = inc.lists.iter().filter(|l| list_complete(store, l)).count();
    let (word, since) = match &inc.aligned {
        Some(a) if a.state == "passed" => ("passed", Some(a.at.clone())),
        _ if done < total => ("running", None),
        Some(a) => ("sent back", Some(a.at.clone())),
        None => ("standing", inc.lists.iter().filter_map(|l| last_stamp(store, l)).max_by_key(|s| util::epoch_of(s))),
    };
    AlignState { word, since, done, total }
}

/// The reason a list may not start though everything it is `after` is done:
/// the align of the increment before its own has not been passed. `None` for
/// a list in no increment, in the first, or behind a passed align.
pub fn align_hold(store: &Store, list: &str) -> Option<String> {
    for i in all(store) {
        let incs = i.increments();
        let Some(k) = incs.iter().position(|r| r.lists.iter().any(|l| l == list)) else { continue };
        let prev = incs[..k].iter().rev().find(|r| !r.lists.is_empty())?;
        if prev.aligned.as_ref().is_some_and(|a| a.state == "passed") {
            return None;
        }
        return Some(format!("align {} of {}", prev.id, i.id));
    }
    None
}

/// A short stamp of the criteria an increment delivers, text and checks: it
/// changes when either does, so a record says which criteria it was said on.
fn criteria_version(i: &Initiative, inc: &Increment) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for r in i.criteria().iter().filter(|r| inc.criteria.contains(&r.id)) {
        for b in r.json().to_string().bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    format!("{:08x}", h & 0xffff_ffff)
}

/// The commits the increment's lists put on the trunk, in list order.
fn align_commits(store: &Store, inc: &Increment) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in &inc.lists {
        let Some(w) = store.worklist(l) else { continue };
        for g in w.groups() {
            for c in g.landed.iter().filter_map(|e| e.commit.clone()) {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// One increment's align as JSON: its state, its stop list (each criterion
/// with its checks and `covered`), its lists and commits, and the record.
pub fn align_json(store: &Store, i: &Initiative, inc: &Increment) -> serde_json::Value {
    let st = align_state(store, inc);
    let crit = i.criteria();
    let criteria: Vec<_> = inc
        .criteria
        .iter()
        .map(|c| match crit.iter().find(|r| &r.id == c) {
            Some(r) => json!({ "id": r.id, "text": r.text, "status": r.status, "checks": r.checks, "covered": !r.checks.is_empty() }),
            None => json!({ "id": c, "text": null, "status": "missing", "checks": [], "covered": false }),
        })
        .collect();
    let lists: Vec<_> = inc
        .lists
        .iter()
        .map(|l| json!({ "id": l, "status": store.worklist(l).map(|w| w.status().as_str().to_string()), "complete": list_complete(store, l) }))
        .collect();
    json!({
        "increment": inc.id, "caption": inc.caption,
        "state": st.word, "since": st.since,
        "lists": lists, "lists_complete": st.done,
        "commits": align_commits(store, inc),
        "criteria": criteria,
        "criteria_version": criteria_version(i, inc),
        "record": inc.aligned.as_ref().map(|a| a.json()),
    })
}

/// The initiative as JSON with each increment's align state beside it.
fn json_with_aligns(store: &Store, i: &Initiative) -> serde_json::Value {
    let mut v = i.json();
    for (k, inc) in i.increments().iter().enumerate() {
        v["increments"][k]["align"] = align_json(store, i, inc);
    }
    v
}

fn align_text(a: &serde_json::Value) -> String {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("").to_string();
    let mut o = format!("{} · {}", s(&a["increment"]), s(&a["caption"]));
    o.push_str(&format!("\n  state {}", s(&a["state"])));
    if let Some(since) = a["since"].as_str() {
        o.push_str(&format!(" since {since}"));
    }
    o.push_str(&format!(" · {} of {} lists complete", a["lists_complete"], a["lists"].as_array().map_or(0, |l| l.len())));
    o.push_str("\n  stop list (criteria version ");
    o.push_str(&s(&a["criteria_version"]));
    o.push(')');
    for c in a["criteria"].as_array().into_iter().flatten() {
        let mark = if c["covered"] == json!(true) { "covered" } else { "not covered" };
        o.push_str(&format!("\n  - {}  {}  [{mark}]", s(&c["id"]), s(&c["text"])));
        for k in c["checks"].as_array().into_iter().flatten() {
            o.push_str(&format!("\n      check: {}", s(k)));
        }
    }
    for l in a["lists"].as_array().into_iter().flatten() {
        o.push_str(&format!("\n  list {}  {}", s(&l["id"]), if l["complete"] == json!(true) { "complete" } else { "not complete" }));
    }
    let commits: Vec<String> = a["commits"].as_array().into_iter().flatten().map(s).collect();
    if !commits.is_empty() {
        o.push_str(&format!("\n  commits {}", commits.iter().map(|c| c.chars().take(10).collect::<String>()).collect::<Vec<_>>().join(" ")));
    }
    if let Some(r) = a["record"].as_object() {
        let g = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        o.push_str(&format!("\n  {} by {} at {} (criteria {}): {}", if g("state") == "passed" { "passed" } else { "sent back" }, g("by"), g("at"), g("criteria_version"), g("text")));
    }
    o.push('\n');
    o
}

/// The word typed after the increment, or stdin where it is `-`.
fn word_from(args: &Args, at: usize) -> Result<String, i32> {
    let t = args.text(at);
    let t = if t.trim() == "-" {
        match util::read_source("-") {
            Ok(t) => t,
            Err(e) => {
                eprintln!("wsp: cannot read stdin: {e}");
                return Err(1);
            }
        }
    } else {
        t
    };
    Ok(t.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// `wsp initiative align show <id> [I<n>] [--json] | pass <id> I<n> "verdict"|- [--fix-next "note"] | back <id> I<n> "note"|-`
fn align(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative align show <id> [I<n>] [--json] | pass <id> I<n> \"verdict\"|- [--fix-next \"note\"] | back <id> I<n> \"note\"|-";
    let (Some(verb), Some(needle)) = (args.rest.get(1).map(|s| s.as_str()), args.rest.get(2)) else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let Some(mut i) = find(store, needle) else {
        eprintln!("wsp: no initiative `{needle}`");
        return 1;
    };
    if !matches!(verb, "show" | "pass" | "back") {
        eprintln!("wsp initiative align: unknown `{verb}` — {USAGE}");
        return 2;
    }
    let mut incs = i.increments();
    let ids = || if incs.is_empty() { "none".to_string() } else { incs.iter().map(|r| r.id.clone()).collect::<Vec<_>>().join(" ") };
    let want = args.rest.get(3).map(|s| s.to_uppercase());
    let pos = match (&want, verb) {
        (Some(w), _) => match incs.iter().position(|r| &r.id == w) {
            Some(p) => p,
            None => {
                eprintln!("wsp: no increment `{w}` on {} — it has {}", i.id, ids());
                return 1;
            }
        },
        (None, "show") => {
            // The one standing now: the first not passed, else the last.
            match incs.iter().position(|r| !r.aligned.as_ref().is_some_and(|a| a.state == "passed")).or(incs.len().checked_sub(1)) {
                Some(p) => p,
                None => {
                    eprintln!("wsp: {} has no increments", i.id);
                    return 1;
                }
            }
        }
        (None, _) => {
            eprintln!("usage: {USAGE}");
            return 2;
        }
    };
    if verb == "show" {
        let a = align_json(store, &i, &incs[pos]);
        if args.json() {
            println!("{}", serde_json::to_string_pretty(&a).unwrap_or_default());
        } else {
            print!("{}  {}", i.id, align_text(&a));
        }
        return 0;
    }

    let text = match word_from(args, 4) {
        Ok(t) => t,
        Err(c) => return c,
    };
    if text.is_empty() {
        eprintln!("usage: {USAGE}");
        return 2;
    }
    if let Some(why) = util::terminal_output(&text) {
        eprintln!("wsp: {why}");
        return 2;
    }
    let st = align_state(store, &incs[pos]);
    let id = incs[pos].id.clone();
    if st.word == "passed" {
        eprintln!("wsp: {id} of {} has passed its align already", i.id);
        return 1;
    }
    if verb == "pass" && st.word == "running" {
        eprintln!("wsp: {id} of {} is still running ({} of {} lists complete) — an align is passed when every list of it has finished", i.id, st.done, st.total);
        return 1;
    }
    if verb == "back" && st.word == "running" {
        eprintln!("wsp: {id} of {} is still running ({} of {} lists complete) — it stands at its align when every list has finished", i.id, st.done, st.total);
        return 1;
    }
    let fix = match args.get("fix-next") {
        Some(f) if verb == "pass" => Some(f).filter(|f| f != "true"),
        Some(_) => {
            eprintln!("wsp: --fix-next goes with `pass`");
            return 2;
        }
        None => None,
    };
    if args.get("fix-next").is_some_and(|f| f == "true") {
        eprintln!("wsp: --fix-next takes the note: --fix-next \"…\"");
        return 2;
    }

    let record = Align {
        state: if verb == "pass" { "passed".into() } else { "back".into() },
        by: crate::cmd_whoami::person(),
        at: util::now_iso(),
        criteria: criteria_version(&i, &incs[pos]),
        commits: align_commits(store, &incs[pos]),
        text: text.clone(),
    };
    incs[pos].aligned = Some(record);
    let mut filed: Option<String> = None;
    if verb == "pass" {
        if let Some(note) = &fix {
            match file_fix_next(store, &mut i, &mut incs, pos, note) {
                Ok(row) => filed = Some(row),
                Err(why) => {
                    eprintln!("{why}");
                    return 1;
                }
            }
        }
        crate::model::append_decision(&mut i.body, &format!("aligned {id}: {text}"), &[]);
    } else {
        // The change request is the one `wsp initiative changes` stores; a
        // build is past the state that verb takes, so it is stored here.
        let n = add_change(&mut i, &format!("{id} sent back at its align: {text}"));
        crate::model::append_decision(&mut i.body, &format!("sent {id} back at its align (change #{n}): {text}"), &[]);
    }
    i.put_increments(&incs);
    if let Err(e) = save(store, &i) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("initiative-align", json!({ "id": i.id, "increment": id, "what": verb, "fix_row": filed }));
    store.git_commit(&format!("wsp: {} {id} align {} — {}", i.id, if verb == "pass" { "passed" } else { "sent back" }, util::truncate(&text, 60)));
    let next = incs[pos + 1..].iter().find(|r| !r.lists.is_empty());
    if args.json() {
        let a = align_json(store, &i, &incs[pos]);
        println!("{}", json!({ "initiative": i.id, "increment": id, "state": a["state"], "align": a, "fix_row": filed, "next": next.map(|n| json!({ "id": n.id, "lists": n.lists })) }));
    } else if verb == "pass" {
        println!("◆ {}  {id} passed its align{}", i.id, match next {
            Some(n) => format!(" — {} starts: {}", n.id, n.lists.join(", ")),
            None => String::new(),
        });
        if let Some(row) = &filed {
            println!("  fix filed as {row}");
        }
    } else {
        println!("◇ {}  {id} sent back — change asked; {} stays held until {id} passes", i.id, next.map(|n| n.id.as_str()).unwrap_or("what follows"));
    }
    0
}

/// File `note` as a row in the first group of the first list of the increment
/// after `incs[k]`; with none after it, as a one-row list in a new last
/// increment. The row's id comes back.
fn file_fix_next(store: &Store, i: &mut Initiative, incs: &mut Vec<Increment>, k: usize, note: &str) -> Result<String, String> {
    let before = incs.clone();
    let from = incs[k].id.clone();
    let next = incs[k + 1..].iter().position(|r| !r.lists.is_empty()).map(|p| k + 1 + p);
    let home = next.and_then(|n| incs[n].lists.first()).and_then(|l| store.worklist(l)).and_then(|w| w.project).or_else(|| i.projects.first().cloned());
    let project = home.or_else(|| incs[k].lists.iter().filter_map(|l| store.worklist(l)).find_map(|w| w.project));
    let id = store.alloc_task_id(project.as_deref()).map_err(|e| format!("wsp: cannot allocate id: {e}"))?;
    let mut t = crate::model::Task::new(&format!("Fix from {from} align: {}", util::truncate(note, 60)), &id);
    t.project = project.clone();
    if project.is_none() {
        t.status_raw = "inbox".into();
    }
    crate::model::set_section_in(&mut t.body, "Overview", &format!("Filed at the {from} align of {}:\n\n{note}", i.id));
    match next {
        Some(n) => {
            let list = incs[n].lists[0].clone();
            let Some(mut w) = store.worklist(&list) else { return Err(format!("wsp: no worklist `{list}`")) };
            let mut groups = w.groups();
            let Some(first) = groups.first_mut() else { return Err(format!("wsp: `{list}` has no group to file the fix in")) };
            first.members.push(id.clone());
            store.save_task(&t).map_err(|e| format!("wsp: write failed: {e}"))?;
            store.log_event("task-added", json!({ "id": t.id, "project": t.project, "title": t.title }));
            w.log(&format!("{id} → group 1 · fix filed at the {from} align of {}", i.id));
            w.set_groups(&groups);
            store.save_worklist(&w).map_err(|e| format!("wsp: write failed: {e}"))?;
            store.log_event("worklist-edited", json!({ "id": w.id, "what": "add" }));
        }
        None => {
            let mut slug = util::slugify(&format!("{}-fix-{}", i.id, from));
            let mut n = 1;
            while store.scope_taken(&slug, None).is_some() {
                n += 1;
                slug = util::slugify(&format!("{}-fix-{}-{n}", i.id, from));
            }
            let agent = incs[k].lists.iter().rev().filter_map(|l| store.worklist(l)).find_map(|w| w.groups().last().map(|g| (g.agent.clone(), g.kind.clone())));
            let mut w = crate::model::Worklist::new(&slug, &format!("Fixes from the {from} align of {}", i.id));
            w.project = project.clone();
            let (agent, kind) = agent.unwrap_or_default();
            w.set_groups(&[crate::model::Group { members: vec![id.clone()], agent, kind, ..Default::default() }]);
            w.log(&format!("{id} → group 1 (new) · fix filed at the {from} align of {}", i.id));
            if i.state == "building" {
                w.set_planned();
            }
            store.save_task(&t).map_err(|e| format!("wsp: write failed: {e}"))?;
            store.log_event("task-added", json!({ "id": t.id, "project": t.project, "title": t.title }));
            store.save_worklist(&w).map_err(|e| format!("wsp: write failed: {e}"))?;
            store.log_event("worklist-added", json!({ "id": w.id }));
            let nid = incs.iter().filter_map(|r| r.id.get(1..)?.parse::<u32>().ok()).max().unwrap_or(0) + 1;
            incs.push(Increment { id: format!("I{nid}"), caption: format!("Fixes from {from}"), lists: vec![slug.clone()], ..Increment::default() });
            if !i.worklists.contains(&slug) {
                i.worklists.push(slug);
            }
            sync_after(store, &before, incs)?;
        }
    }
    if !i.tasks.contains(&id) {
        i.tasks.push(id.clone());
    }
    Ok(id)
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
                                    | increment add <id> ["caption"] [--criteria C1,C2] [--list L]…
                                    | increment edit <id> I1 ["caption"] [--criteria …] [--list …] | increment rm <id> I1
                                    | plan show <id> [--json]  increments → lists → groups → rows
                                    | approve <id> "verdict"  | changes <id> "what to change"
                                    | align show <id> [I<n>] [--json]  the stop list, lists, commits, state
                                    | align pass <id> I<n> "verdict"|- [--fix-next "note"]  start n+1
                                    | align back <id> I<n> "note"|-  a change request; n+1 stays held
                                    | run <id> [--json]  the build's feed: group running, rows, spend
                                    | plan <id> [--changes "…"|-] [--model M] [--dry-run]  seat a
                                    planner (opus) on the order in docs/initiative-plan.md
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

    /// `wsp-733`: the planner's order is the doc's `## The order` plus the
    /// brief, a change request makes it a replan with the plan in it, and
    /// `--dry-run` prints it, filing and seating nothing.
    #[test]
    fn plan_assembles_the_order_from_the_doc_and_a_dry_run_seats_nothing() {
        let store = crate::testkit::store("initiative-planner");
        let mut prod = Project::new("compound");
        prod.kind = "product".into();
        store.save_project(&prod).unwrap();
        let mut ui = Project::new("ui");
        ui.parent = Some("compound".into());
        store.save_project(&ui).unwrap();
        assert_eq!(run(&store, &["add", "Fixture"], &[("product", "compound"), ("project", "ui")]), 0);
        assert_eq!(run(&store, &["criterion", "add", "init-1", "a board shows the plan"], &[]), 0);

        let doc = order_section(PLAN_DOC);
        for want in ["### 1. Read", "### 5. Finish", "state=planned", "--initiative", "wsp after", "### Replanning"] {
            assert!(doc.contains(want), "the order lacks {want}");
        }
        assert!(!doc.contains("## The order") && !doc.contains("Plan `wsp-733`"), "only the section, not the preamble");

        let i = find(&store, "init-1").unwrap();
        let o = plan_order(&store, &i, None);
        assert!(o.starts_with("# Plan · init-1 · Fixture"), "{o}");
        assert!(o.contains("### 2. Criteria") && o.contains("C1 [draft") && o.contains("wsp project show ui --handbook"), "{o}");
        assert!(!o.contains("## Change request"), "{o}");
        let r = plan_order(&store, &i, Some("split the board into two lists"));
        assert!(r.contains("This is a replan") && r.contains("## The plan now") && r.contains("split the board into two lists"), "{r}");
        assert_eq!(plan_order(&store, &i, Some("  ")), o, "an empty request is no replan");

        let cases: &[(&[&str], &[(&str, &str)], i32)] = &[
            (&["plan", "init-1"], &[("dry-run", "true")], 0),
            (&["plan", "init-1"], &[("dry-run", "true"), ("changes", "fewer rows"), ("model", "sonnet")], 0),
            (&["plan", "init-1"], &[("changes", "true")], 2),
            (&["plan", "init-9"], &[("dry-run", "true")], 1),
            (&["plan"], &[], 2),
            (&["plan", "show"], &[], 2),
        ];
        for (rest, flags, want) in cases {
            assert_eq!(run(&store, rest, flags), *want, "{rest:?} {flags:?}");
        }
        assert!(store.tasks().is_empty(), "a dry run files no row");
        assert!(find(&store, "init-1").unwrap().tasks.is_empty(), "a dry run links nothing");

        run(&store, &["set", "init-1", "state=building"], &[]);
        assert_eq!(run(&store, &["plan", "init-1"], &[("dry-run", "true")]), 1, "no plan during the build");
    }

    /// `wsp-731`: increments over 2 lists, 5 groups, 2 increments — the verbs'
    /// refusals, the `after` they keep, and the plan graph's JSON shape.
    /// Two increments of one list each, `n1` and `n2`, and an initiative
    /// `init-1` with a criterion that has a check; `n1`'s list `done` as asked.
    fn two_increments(tag: &str, n1_done: bool) -> Store {
        use crate::model::{Group, Task, Worklist};
        let store = crate::testkit::store(tag);
        let mut prod = Project::new("compound");
        prod.kind = "product".into();
        store.save_project(&prod).unwrap();
        assert_eq!(run(&store, &["add", "Plan"], &[("product", "compound")]), 0);
        assert_eq!(run(&store, &["criterion", "add", "init-1", "it works"], &[("check", "cargo test works")]), 0);
        assert_eq!(run(&store, &["criterion", "add", "init-1", "it is fast"], &[]), 0);
        for (l, m) in [("n1", "q-1"), ("n2", "q-2")] {
            store.save_task(&Task::new(m, m)).unwrap();
            let mut w = Worklist::new(l, l);
            w.set_groups(&[Group { members: vec![m.into()], agent: "claude sonnet".into(), ..Group::default() }]);
            store.save_worklist(&w).unwrap();
        }
        assert_eq!(run(&store, &["increment", "add", "init-1", "First"], &[("criteria", "C1,C2"), ("list", "n1")]), 0);
        assert_eq!(run(&store, &["increment", "add", "init-1", "Second"], &[("list", "n2")]), 0);
        if n1_done {
            let mut w = store.worklist("n1").unwrap();
            w.status_raw = "done".into();
            store.save_worklist(&w).unwrap();
        }
        store
    }

    #[test]
    fn align_show_reads_the_stop_list_the_lists_and_the_state_and_pass_is_refused_while_running() {
        let store = two_increments("align-show", false);
        let i = find(&store, "init-1").unwrap();
        let a = align_json(&store, &i, &i.increments()[0]);
        assert_eq!(a["state"], json!("running"), "{a}");
        assert_eq!(a["criteria"][0]["covered"], json!(true), "{a}");
        assert_eq!(a["criteria"][0]["checks"], json!(["cargo test works"]));
        assert_eq!(a["criteria"][1]["covered"], json!(false), "{a}");
        assert_eq!(a["lists"][0]["complete"], json!(false));
        assert_eq!(align_hold(&store, "n2").as_deref(), Some("align I1 of init-1"));
        // Refused while n's lists are running, and says so.
        assert_eq!(run(&store, &["align", "pass", "init-1", "I1", "fine"], &[]), 1);
        assert_eq!(run(&store, &["align", "back", "init-1", "I1", "no"], &[]), 1);
        assert!(find(&store, "init-1").unwrap().increments()[0].aligned.is_none());
        // show: text and --json, with and without the increment named.
        assert_eq!(run(&store, &["align", "show", "init-1"], &[]), 0);
        assert_eq!(run(&store, &["align", "show", "init-1", "I2"], &[("json", "true")]), 0);
        assert_eq!(run(&store, &["align", "show", "init-1", "I9"], &[]), 1);
        assert_eq!(run(&store, &["align", "show"], &[]), 2);
        assert_eq!(run(&store, &["align", "bogus", "init-1"], &[]), 2);
        // initiative show --json and plan show --json carry each increment's state.
        let v = json_with_aligns(&store, &i);
        assert_eq!(v["increments"][0]["align"]["state"], json!("running"));
        assert_eq!(plan_graph(&store, &i)["increments"][1]["align"]["state"], json!("running"));
        assert_eq!(plan_graph(&store, &i)["increments"][0]["lists"][0]["groups"][0]["gate"], json!(GATE_ALIGN), "the last group of an increment ends at its align");
    }

    #[test]
    fn a_standing_align_holds_the_next_increment_and_pass_records_who_when_and_what_and_releases_it() {
        let store = two_increments("align-pass", true);
        let i = find(&store, "init-1").unwrap();
        let st = align_state(&store, &i.increments()[0]);
        assert_eq!(st.word, "standing");
        assert_eq!(align_hold(&store, "n2").as_deref(), Some("align I1 of init-1"), "n+1 is held while n stands");
        assert_eq!(align_hold(&store, "n1"), None, "the first increment waits on no align");
        assert_eq!(run(&store, &["align", "pass", "init-1", "I1"], &[]), 2, "a verdict is asked for");
        assert_eq!(run(&store, &["align", "pass", "init-1", "I1", "looks", "right"], &[]), 0);
        let i = find(&store, "init-1").unwrap();
        let rec = i.increments()[0].aligned.clone().unwrap();
        assert_eq!((rec.state.as_str(), rec.text.as_str()), ("passed", "looks right"));
        assert!(rec.by.starts_with("person") && !rec.at.is_empty() && rec.criteria.len() == 8, "{rec:?}");
        assert_eq!(align_hold(&store, "n2"), None, "pass releases n+1");
        assert_eq!(align_state(&store, &i.increments()[0]).word, "passed");
        assert!(crate::model::decisions_of(&i.body).iter().any(|d| d.text.contains("aligned I1: looks right")));
        assert_eq!(run(&store, &["align", "pass", "init-1", "I1", "again"], &[]), 1, "passed once");
        // The wait the queue tells.
        assert_eq!(crate::waits::Wait::Align("align I1 of init-1".into()).text(), "waiting on align I1 of init-1");
    }

    #[test]
    fn back_stores_a_change_request_and_keeps_the_next_increment_held_until_it_passes() {
        let store = two_increments("align-back", true);
        assert_eq!(run(&store, &["align", "back", "init-1", "I1", "C2", "is", "not", "met"], &[]), 0);
        let i = find(&store, "init-1").unwrap();
        assert_eq!(align_state(&store, &i.increments()[0]).word, "sent back");
        let ch = change_requests(&i);
        assert_eq!(ch.len(), 1);
        assert!(ch[0].2.contains("C2 is not met") && ch[0].2.contains("I1"), "{ch:?}");
        assert_eq!(align_hold(&store, "n2").as_deref(), Some("align I1 of init-1"), "n+1 stays held");
        assert_eq!(run(&store, &["align", "pass", "init-1", "I1", "--", "fixed"], &[]), 0, "a sent-back align can be passed once the lists have finished");
        assert_eq!(align_hold(&store, "n2"), None);
    }

    #[test]
    fn fix_next_files_a_row_in_the_next_increments_first_group_or_a_new_last_increment() {
        let store = two_increments("align-fix", true);
        assert_eq!(run(&store, &["align", "pass", "init-1", "I1", "ok"], &[("fix-next", "tighten the C2 check")]), 0);
        let w = store.worklist("n2").unwrap();
        let first = &w.groups()[0];
        assert_eq!(first.members.len(), 2, "{:?}", first.members);
        let row = first.members.last().unwrap();
        let t = store.task(row).unwrap();
        assert!(t.title.contains("tighten the C2 check"), "{}", t.title);
        assert!(find(&store, "init-1").unwrap().tasks.contains(row));
        // The last increment: a new one with a one-row list.
        let mut n2 = store.worklist("n2").unwrap();
        n2.status_raw = "done".into();
        store.save_worklist(&n2).unwrap();
        assert_eq!(run(&store, &["align", "pass", "init-1", "I2", "ok"], &[("fix-next", "one more thing")]), 0);
        let i = find(&store, "init-1").unwrap();
        let incs = i.increments();
        assert_eq!(incs.len(), 3, "{incs:?}");
        assert_eq!(incs[2].lists.len(), 1);
        let nl = store.worklist(&incs[2].lists[0]).unwrap();
        assert_eq!(nl.groups().len(), 1);
        assert_eq!(nl.groups()[0].members.len(), 1);
        assert_eq!(nl.groups()[0].agent, "claude sonnet");
        assert_eq!(nl.after, ["n2"], "the new increment waits on the one before it");
        assert_eq!(align_hold(&store, &incs[2].lists[0]), None, "I2 passed, so the list filed behind it is not held by it");
        assert_eq!(run(&store, &["align", "back", "init-1", "I1", "x"], &[("fix-next", "no")]), 1, "--fix-next goes with pass");
    }

    #[test]
    fn increments_order_their_lists_and_plan_show_is_one_graph() {
        use crate::model::{Group, Task, Worklist};
        let store = crate::testkit::store("initiative-plan");
        let mut prod = Project::new("compound");
        prod.kind = "product".into();
        store.save_project(&prod).unwrap();
        assert_eq!(run(&store, &["add", "Plan"], &[("product", "compound")]), 0);
        assert_eq!(run(&store, &["criterion", "add", "init-1", "one"], &[]), 0);
        assert_eq!(run(&store, &["criterion", "add", "init-1", "two"], &[]), 0);
        let mut t = Task::new("Write the first thing, with a clause", "p-1");
        t.caption = Some("first".into());
        store.save_task(&t).unwrap();
        let mut t = Task::new("Second", "p-2");
        t.after = vec!["p-1".into()];
        store.save_task(&t).unwrap();
        for id in ["p-3", "p-4", "p-5", "p-6"] {
            store.save_task(&Task::new(id, id)).unwrap();
        }
        let g = |m: &[&str], kind: &str, agent: &str| Group {
            members: m.iter().map(|s| s.to_string()).collect(),
            kind: kind.into(),
            agent: agent.into(),
            ..Group::default()
        };
        let mut a = Worklist::new("pa", "List A");
        a.set_groups(&[g(&["p-1", "p-2"], "contract", "claude opus high"), g(&["p-3"], "", "claude sonnet")]);
        store.save_worklist(&a).unwrap();
        let mut b = Worklist::new("pb", "List B");
        b.set_groups(&[g(&["p-4"], "wiring", "claude"), g(&["p-5"], "", ""), g(&["p-6", "p-gone"], "", "")]);
        store.save_worklist(&b).unwrap();
        // `worklist new --initiative` links as it makes.
        let wl = |rest: &[&str], flags: &[(&str, &str)]| crate::cmd_worklist::dispatch(&store, &Args::synth("worklist", rest, flags));
        assert_eq!(wl(&["new", "pc", "List C"], &[("initiative", "init-9")]), 1);
        assert!(store.worklist("pc").is_none(), "a refused initiative writes no list");
        assert_eq!(wl(&["new", "pc", "List C"], &[("initiative", "init-1")]), 0);
        assert_eq!(find(&store, "init-1").unwrap().worklists, vec!["pc"]);

        let cases: &[(&[&str], &[(&str, &str)], i32)] = &[
            (&["increment", "add", "init-1", "Contract first"], &[("criteria", "c1"), ("list", "pa")], 0),
            (&["increment", "add", "init-1", "Then wiring"], &[("criteria", "C2"), ("list", "pb,pc")], 0),
            (&["increment", "add", "init-1", "bad"], &[("criteria", "C9")], 1),
            (&["increment", "add", "init-1", "bad"], &[("list", "nope")], 1),
            (&["increment", "add", "init-1", "dup"], &[("list", "pa")], 1),
            (&["increment", "add", "init-9", "x"], &[], 1),
            (&["increment", "edit", "init-1", "I7"], &[], 1),
            (&["increment", "edit", "init-1", "i2"], &[("list", "pb")], 0),
            (&["increment", "add", "init-1", "spare"], &[], 0),
            (&["increment", "rm", "init-1", "I3"], &[], 0),
            (&["increment", "bogus", "init-1"], &[], 2),
            (&["increment"], &[], 2),
            (&["plan", "show"], &[], 2),
            (&["plan", "show", "init-9"], &[], 1),
            (&["plan", "show", "init-1"], &[], 0),
            (&["plan", "show", "init-1"], &[("json", "true")], 0),
        ];
        for (rest, flags, want) in cases {
            assert_eq!(run(&store, rest, flags), *want, "{rest:?} {flags:?}");
        }
        let i = find(&store, "init-1").unwrap();
        assert_eq!(i.increments().iter().map(|r| (r.id.as_str(), r.lists.clone())).collect::<Vec<_>>(), vec![("I1", vec!["pa".to_string()]), ("I2", vec!["pb".to_string()])]);
        assert!(i.worklists.contains(&"pa".to_string()) && i.worklists.contains(&"pb".to_string()), "an increment's lists are linked");
        assert_eq!(store.worklist("pb").unwrap().after, ["pa"], "increment 2 waits on increment 1");
        assert!(store.worklist("pa").unwrap().after.is_empty());
        assert!(store.worklist("pc").unwrap().after.is_empty(), "a list let go of the plan is let go by its after");

        let v = plan_graph(&store, &i);
        assert_eq!(v["totals"], json!({ "increments": 2, "lists": 3, "groups": 5, "rows": 7 }), "pc is counted, unplanned: {v}");
        assert_eq!(v["unplanned"][0]["id"], json!("pc"));
        // Every node carries what the chrome draws, at every level.
        let shape: &[(&str, &serde_json::Value, &[&str])] = &[
            ("graph", &v, &["initiative", "title", "state", "increments", "unplanned", "totals"]),
            ("increment", &v["increments"][0], &["id", "ordinal", "caption", "criteria", "lists"]),
            ("criterion", &v["increments"][0]["criteria"][0], &["id", "text", "status"]),
            ("list", &v["increments"][1]["lists"][0], &["id", "title", "caption", "status", "after", "groups"]),
            ("group", &v["increments"][0]["lists"][0]["groups"][0], &["id", "ordinal", "caption", "kind", "agent", "gate", "increment_end", "rows"]),
            ("row", &v["increments"][0]["lists"][0]["groups"][0]["rows"][1], &["id", "title", "caption", "tier", "model", "status", "after"]),
        ];
        for (what, node, keys) in shape {
            for k in *keys {
                assert!(node.get(k).is_some(), "{what} lacks {k}: {node}");
            }
        }
        let inc1 = &v["increments"][0];
        assert_eq!(inc1["criteria"][0]["text"], json!("one"));
        let ga = &inc1["lists"][0]["groups"];
        assert_eq!((ga[0]["kind"].clone(), ga[0]["gate"].clone(), ga[0]["increment_end"].clone()), (json!("contract"), json!(GATE_BARRIER), json!(false)));
        assert_eq!(ga[1]["increment_end"], json!(true), "the last group of an increment is where an align will stand");
        assert_eq!(ga[0]["caption"], json!("first and 1 more"));
        let r = &ga[0]["rows"];
        assert_eq!((r[0]["caption"].clone(), r[0]["status"].clone()), (json!("first"), json!("todo")));
        assert_eq!(r[1]["after"], json!(["p-1"]));
        assert_eq!(r[0]["tier"], json!({ "kind": "claude", "model": "opus", "effort": "high" }));
        let gb = &v["increments"][1]["lists"][0]["groups"];
        assert_eq!(v["increments"][1]["lists"][0]["after"], json!(["pa"]));
        assert_eq!(gb.as_array().unwrap().len(), 3);
        assert_eq!(gb[2]["rows"][1]["status"], json!("missing"), "a member no task answers to is drawn as missing");
        assert_eq!(gb[0]["rows"][0]["tier"], json!({ "kind": "claude", "model": null, "effort": null }));
        assert_eq!(gb[1]["rows"][0]["tier"], json!(null), "a group with no agent line is run by hand");

        let text = plan_text(&v);
        for want in ["2 increments · 3 lists · 5 groups · 7 rows", "I1  Contract first  delivers C1", "pb  List B [draft] · after pa", "p-2  Second [todo] · after p-1", "── barrier", "unplanned"] {
            assert!(text.contains(want), "text lacks `{want}`:\n{text}");
        }
        assert!(i.json()["increments"][1]["lists"] == json!(["pb"]));

        // Two lists in one increment run side by side, and the next increment's
        // lists each wait on both (`wsp-734`).
        assert_eq!(run(&store, &["add", "Chain"], &[("product", "compound")]), 0);
        for id in ["qa", "qb", "qc", "qd"] {
            store.save_worklist(&Worklist::new(id, id)).unwrap();
        }
        let after = |id: &str| store.worklist(id).unwrap().after;
        assert_eq!(run(&store, &["increment", "add", "init-2", "one"], &[("list", "qa"), ("list", "qb")]), 0);
        assert!(after("qa").is_empty() && after("qb").is_empty(), "an increment's lists wait on nothing in it");
        assert_eq!(run(&store, &["increment", "add", "init-2", "two"], &[("list", "qc,qd")]), 0);
        assert_eq!((after("qc"), after("qd")), (vec!["qa".to_string(), "qb".to_string()], vec!["qa".to_string(), "qb".to_string()]), "the next increment waits on all of the first");
        let v = plan_graph(&store, &find(&store, "init-2").unwrap());
        assert_eq!(v["increments"][1]["lists"][1]["after"], json!(["qa", "qb"]), "--json shows every edge");
        assert!(plan_text(&v).contains("qc") && plan_text(&v).contains("· after qa, qb"), "plan show shows every edge:\n{}", plan_text(&v));
        assert_eq!(run(&store, &["increment", "edit", "init-2", "I1"], &[("list", "qb")]), 0);
        assert_eq!((after("qc"), after("qa")), (vec!["qb".to_string()], Vec::<String>::new()), "an edit of the first increment moves the second's edges; the list let go keeps none");
        assert_eq!(run(&store, &["increment", "edit", "init-2", "I1"], &[("list", "qa,qb")]), 0);
        assert_eq!(after("qd"), ["qa", "qb"]);
    }

    /// `wsp-732`: approve for build and ask for changes, table-driven.
    #[test]
    fn approve_plans_the_lists_changes_are_stored_and_plan_reads_them() {
        use crate::model::{Group, Task, Worklist};
        let store = crate::testkit::store("initiative-approve");
        let mut prod = Project::new("compound");
        prod.kind = "product".into();
        store.save_project(&prod).unwrap();
        assert_eq!(run(&store, &["add", "Appr"], &[("product", "compound")]), 0);
        assert_eq!(run(&store, &["criterion", "add", "init-1", "one"], &[]), 0);
        assert_eq!(run(&store, &["assume", "add", "init-1", "users have git"], &[]), 0);
        store.save_task(&Task::new("T1", "p-1")).unwrap();
        for (id, m) in [("pa", "p-1"), ("pb", "p-1")] {
            let mut w = Worklist::new(id, id);
            w.set_groups(&[Group { members: vec![m.to_string()], ..Group::default() }]);
            store.save_worklist(&w).unwrap();
        }
        assert_eq!(run(&store, &["increment", "add", "init-1", "first"], &[("list", "pa")]), 0);
        assert_eq!(run(&store, &["increment", "add", "init-1", "second"], &[("list", "pb")]), 0);
        let state = |s: &Store| find(s, "init-1").unwrap().state;

        // Not planned: refused, nothing changes.
        assert_eq!(run(&store, &["approve", "init-1", "go"], &[]), 1);
        assert_eq!(run(&store, &["changes", "init-1", "x"], &[]), 1);
        assert_eq!(run(&store, &["set", "init-1", "state=planned"], &[]), 0);
        let cases: &[(&[&str], i32)] = &[
            (&["approve"], 2),
            (&["approve", "init-1"], 2),
            (&["approve", "init-9", "ok"], 1),
            (&["changes", "init-1"], 2),
            (&["changes", "init-1", "split", "the", "first list"], 0),
            (&["changes", "init-1", "and fewer rows"], 0),
        ];
        for (rest, want) in cases {
            assert_eq!(run(&store, rest, &[]), *want, "{rest:?}");
        }
        assert_eq!(state(&store), "planned", "changes leave the state");
        assert!(store.worklist("pa").unwrap().status() == crate::model::WorklistStatus::Draft && !store.worklist("pa").unwrap().is_planned(), "changes start nothing");
        let i = find(&store, "init-1").unwrap();
        assert_eq!(newest_change(&i), Some((2, "and fewer rows".into())));
        // plan with no --changes reads the newest unanswered request.
        assert_eq!(run(&store, &["plan", "init-1"], &[("dry-run", "true")]), 0);

        // A list already started refuses approve, whole.
        let mut w = store.worklist("pb").unwrap();
        w.set_status(crate::model::WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
        assert_eq!(run(&store, &["approve", "init-1", "ok"], &[]), 1);
        assert_eq!(state(&store), "planned");
        assert!(!store.worklist("pa").unwrap().is_planned(), "a refusal marks nothing");
        let mut w = store.worklist("pb").unwrap();
        w.set_status(crate::model::WorklistStatus::Draft);
        store.save_worklist(&w).unwrap();

        assert_eq!(run(&store, &["approve", "init-1", "looks right"], &[]), 0);
        let i = find(&store, "init-1").unwrap();
        assert_eq!(i.state, "building");
        assert_eq!(i.criteria()[0].status, "confirmed");
        assert_eq!(i.assumptions()[0].status, "accepted");
        assert!(crate::model::decisions_of(&i.body).iter().any(|d| d.text == "approved for build: looks right"));
        for l in ["pa", "pb"] {
            let w = store.worklist(l).unwrap();
            assert!(w.is_planned() && w.status() != crate::model::WorklistStatus::Running, "{l} planned, not executing");
        }
        assert_eq!(store.worklist("pb").unwrap().after, ["pa"], "later lists wait on earlier ones");
        assert_eq!(run(&store, &["approve", "init-1", "again"], &[]), 1, "approved once");
    }
}
