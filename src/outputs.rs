//! What a worklist hands back: the outputs it declares, where they go, and the
//! gates that wait on them (wsp-375), moved down from `cmd_report` (wsp-436) so
//! `cmd_worklist` names no timeline module for them.
//!
//! The writers stay in timeline: the effort and work reports, `wsp worklist
//! report` and `wsp worklist cost`. They reach the engine as [`Reports`], which
//! `main` installs; `cmd_report` re-exports what moved at its old paths.

use std::path::PathBuf;

use serde_json::json;

use crate::base::args::Args;
use crate::model::{Worklist, WorklistStatus};
use crate::store::Store;

/// The report writers and verbs that live above the engine, in timeline
/// (`cmd_report::REPORTS`). Installed by `main` with [`install`]; a process
/// without them writes no report at the last barrier and refuses the two verbs.
pub struct Reports {
    pub write_effort: fn(&Store, &Worklist, Option<&str>, &str) -> Result<(PathBuf, PathBuf), String>,
    pub write_work: fn(&Store, &Worklist, Option<&str>) -> Result<Option<PathBuf>, String>,
    /// `wsp worklist report <slug>`.
    pub report: fn(&Store, &Args) -> i32,
    /// `wsp worklist cost <slug>`.
    pub cost: fn(&Store, &Args, &str) -> i32,
}

static REPORTS: std::sync::OnceLock<Reports> = std::sync::OnceLock::new();

pub fn install(r: Reports) {
    let _ = REPORTS.set(r);
}

fn reports() -> Option<&'static Reports> {
    #[cfg(test)]
    test_reports::install();
    REPORTS.get()
}

/// `wsp worklist report`, through the installed writer.
pub fn report(store: &Store, args: &Args) -> i32 {
    match reports() {
        Some(r) => (r.report)(store, args),
        None => no_writer("report"),
    }
}

/// `wsp worklist cost <slug>`, through the installed reader.
pub fn cost(store: &Store, args: &Args, slug: &str) -> i32 {
    match reports() {
        Some(r) => (r.cost)(store, args, slug),
        None => no_writer("cost"),
    }
}

fn no_writer(verb: &str) -> i32 {
    eprintln!("wsp: worklist {verb}: this process installed no report writer");
    1
}

pub const USAGE: &str = "usage:
  wsp worklist output <slug> [add <name> [--from <member>|--barrier|--wsp] [--kind html|md|json|png]]
  wsp worklist output <slug> rm <name>
  wsp worklist output <slug> --none
  wsp worklist report <slug> [--baseline <slug>] [--json]";

/// Who writes an output.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// wsp, no agent: the effort report.
    Wsp,
    /// The last barrier check: the work report.
    Barrier,
    /// A file this member writes into the list's artefacts folder.
    Member(String),
}

impl Source {
    fn word(&self) -> String {
        match self {
            Source::Wsp => "wsp".into(),
            Source::Barrier => "barrier".into(),
            Source::Member(m) => format!("member={m}"),
        }
    }
    fn parse(s: &str) -> Source {
        match s {
            "wsp" => Source::Wsp,
            "barrier" => Source::Barrier,
            m => Source::Member(m.strip_prefix("member=").unwrap_or(m).to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub name: String,
    pub kind: String,
    pub source: Source,
}

impl Output {
    pub fn file(&self) -> String {
        format!("{}.{}", self.name, self.kind)
    }
}

/// `~/.compound/artefacts/<slug>/`.
pub fn dir(slug: &str) -> PathBuf {
    crate::artefact::root().join(slug)
}

fn defaults() -> Vec<Output> {
    vec![
        Output { name: "work".into(), kind: "html".into(), source: Source::Barrier },
        Output { name: "effort".into(), kind: "html".into(), source: Source::Wsp },
    ]
}

/// What the list declares: the defaults unless `none`, less any `-name`, plus
/// every `name:kind:source` it adds.
pub fn declared(w: &Worklist) -> Vec<Output> {
    let raw = w.outputs.clone().unwrap_or_default();
    let items: Vec<&str> = raw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if items.contains(&"off") {
        return Vec::new();
    }
    let mut out = defaults();
    for i in &items {
        if let Some(name) = i.strip_prefix('-') {
            out.retain(|o| o.name != name);
            continue;
        }
        let mut p = i.splitn(3, ':');
        let (Some(name), kind, source) = (p.next(), p.next().unwrap_or("html"), p.next().unwrap_or("barrier")) else { continue };
        out.retain(|o| o.name != name);
        out.push(Output { name: name.into(), kind: kind.into(), source: Source::parse(source) });
    }
    out
}

fn encode(outs: &[Output], none: bool) -> Option<String> {
    if none {
        return Some("off".into());
    }
    let mut items: Vec<String> = Vec::new();
    for d in defaults() {
        if !outs.iter().any(|o| o.name == d.name) {
            items.push(format!("-{}", d.name));
        }
    }
    for o in outs {
        let is_default = defaults().iter().any(|d| *d == *o);
        if !is_default {
            items.push(format!("{}:{}:{}", o.name, o.kind, o.source.word()));
        }
    }
    (!items.is_empty()).then(|| items.join(","))
}

/// What the mirror writes a list's `outputs` and `outputs_dir` from: each
/// declared output with whether its file is on disk, and the directory.
pub fn list_outputs(w: &Worklist) -> (Vec<serde_json::Value>, String) {
    let d = dir(&w.id);
    let outs = declared(w)
        .iter()
        .map(|o| serde_json::json!({ "name": o.name, "kind": o.kind, "file": o.file(), "present": d.join(o.file()).is_file() }))
        .collect();
    (outs, d.display().to_string())
}

/// The files a declared output is still missing, by name.
pub fn missing(w: &Worklist) -> Vec<String> {
    let d = dir(&w.id);
    declared(w).iter().filter(|o| !d.join(o.file()).is_file()).map(|o| o.file()).collect()
}

/// One line for `wsp worklist show`.
pub fn line(w: &Worklist) -> String {
    let outs = declared(w);
    if outs.is_empty() {
        return "none declared".into();
    }
    let miss = missing(w);
    let names: Vec<String> = outs.iter().map(|o| if miss.contains(&o.file()) { format!("{} (not yet)", o.file()) } else { o.file() }).collect();
    format!("{} · {}", names.join(", "), dir(&w.id).display())
}

/// At the last barrier: effort first, then the work report, then whatever
/// declared output is still missing. `Err` names them and holds the barrier.
pub fn at_last_barrier(store: &Store, w: &Worklist, said: &str, dry: bool) -> Result<(), Vec<String>> {
    let outs = declared(w);
    if dry || outs.is_empty() {
        return Ok(());
    }
    if let Some(r) = reports() {
        if outs.iter().any(|o| o.name == "effort") {
            let _ = (r.write_effort)(store, w, None, "effort report, at the last barrier");
        }
        if outs.iter().any(|o| o.name == "work") {
            let _ = (r.write_work)(store, w, Some(said));
        }
    }
    let miss = missing(w);
    if miss.is_empty() {
        Ok(())
    } else {
        Err(miss)
    }
}

pub fn output(store: &Store, args: &Args) -> i32 {
    let Some(slug) = args.rest.get(1) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let Some(mut w) = store.worklist(slug) else {
        eprintln!("wsp: no worklist {slug}");
        return 1;
    };
    let mut outs = declared(&w);
    let mut none = declared(&w).is_empty() && w.outputs.as_deref().is_some_and(|o| o.split(',').any(|x| x.trim() == "off"));
    let did = if args.has("none") {
        none = true;
        "no outputs".to_string()
    } else {
        match args.rest.get(2).map(String::as_str) {
            None | Some("ls") => {
                if args.json() {
                    println!("{}", json!({ "list": w.id, "dir": dir(&w.id), "outputs": outs.iter().map(|o| json!({ "name": o.name, "kind": o.kind, "source": o.source.word(), "file": o.file(), "present": dir(&w.id).join(o.file()).is_file() })).collect::<Vec<_>>() }));
                } else {
                    println!("{}", line(&w));
                }
                return 0;
            }
            Some("add") => {
                let Some(name) = args.rest.get(3) else {
                    eprintln!("{USAGE}");
                    return 2;
                };
                if name.is_empty() || name.contains(['/', ':', ',']) || name.starts_with(['.', '-']) {
                    eprintln!("wsp: `{name}` is not a usable output name");
                    return 2;
                }
                let kind = args.get("kind").unwrap_or_else(|| "html".into());
                if !["html", "md", "json", "png"].contains(&kind.as_str()) {
                    eprintln!("wsp: kind is html, md, json or png, not `{kind}`");
                    return 2;
                }
                let source = if let Some(m) = args.get("from") {
                    Source::Member(m)
                } else if args.has("wsp") {
                    Source::Wsp
                } else {
                    Source::Barrier
                };
                none = false;
                outs.retain(|o| o.name != *name);
                outs.push(Output { name: name.clone(), kind, source });
                format!("added {name}")
            }
            Some("rm" | "remove") => {
                let Some(name) = args.rest.get(3) else {
                    eprintln!("{USAGE}");
                    return 2;
                };
                if !outs.iter().any(|o| o.name == *name) {
                    eprintln!("wsp: {} declares no output `{name}`", w.id);
                    return 1;
                }
                outs.retain(|o| o.name != *name);
                format!("removed {name}")
            }
            Some(other) => {
                eprintln!("wsp: worklist output: `{other}` is not add, rm or ls\n{USAGE}");
                return 2;
            }
        }
    };
    w.outputs = encode(&outs, none);
    w.log(&format!("outputs — {did} · by {}", crate::cmd_message::caller(store)));
    if let Err(e) = store.save_worklist(&w) {
        eprintln!("wsp: {e}");
        return 1;
    }
    store.git_commit(&format!("wsp: worklist {} outputs: {did}", w.id));
    println!("{} {}", w.id, line(&w));
    0
}

/// `done` over a missing output: refuse unless forced with a why.
pub fn gate_done(w: &Worklist, force: Option<&str>) -> Result<Option<String>, Vec<String>> {
    if w.status() == WorklistStatus::Done {
        return Ok(None);
    }
    let miss = missing(w);
    if miss.is_empty() {
        return Ok(None);
    }
    match force {
        Some(why) if !why.trim().is_empty() => Ok(Some(format!("forced past missing output(s) {} — {}", miss.join(", "), why.trim()))),
        _ => Err(miss),
    }
}

/// A test build installs the timeline writers itself, as `main` does, so a
/// test that passes a last barrier writes what the binary would.
#[cfg(test)]
mod test_reports {
    pub(super) fn install() {
        super::install(crate::cmd_report::REPORTS);
    }
}
