//! `wsp publish <row|list> --to shared:<name>` (wsp-185): move a private row or
//! list into a shared workspace.
//!
//! A list goes with its member rows; a row goes with its sub-tasks (`parent:`)
//! and its pieces (`<row>-<n>`). Legs are a section of the row file and travel
//! with it. Each row takes a new id from the hub's counter for its project
//! (`ids.alloc`, so two spokes never hand out the same one); the old id keeps
//! resolving through `ids.json`. A piece keeps its number under its row's new id.
//!
//! The hub is asked before anything is written: a refusal changes nothing. The
//! rows are then renamed and stamped `workspace:` ([`Store::renumber_into`]);
//! the sync worker pushes them on its next turn, and publish does not.
//!
//! Refused: a logged-out spoke (`wsp login`), a record already shared (there is
//! no unpublish in v1), a sub-task or piece on its own (it lives in its row's
//! workspace), a claimed row (the hub's lease verbs are wsp-466's, and a claimed
//! row published unleased would be a row two seats could move), and a workspace
//! the hub says the caller is not a member of.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use wsp_client::{Client, Credential, Error as HubError};
use wsp_objects::WorkspaceId as HubWorkspace;

use crate::base::args::Args;
use crate::model::{Task, WorkspaceId};
use crate::store::{split_piece_id, Store, INBOX_CODE};

const WAIT: Duration = Duration::from_secs(10);

/// How many times an allocation that came back with ids already used here is asked again.
const ROUNDS: usize = 8;

/// What a publish would move.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The list, when the target is one.
    pub(crate) list: Option<String>,
    /// Rows that take a new id, parents before children, by project code.
    pub(crate) rows: Vec<(String, String)>,
    /// Pieces, each with the row it belongs to.
    pub(crate) pieces: Vec<(String, String)>,
}

fn code_of(store: &Store, t: &Task) -> String {
    t.project.as_deref().and_then(|p| store.project(p)).map(|p| p.code().to_string()).unwrap_or_else(|| INBOX_CODE.to_string())
}

/// What `target` takes with it, or why it cannot be published. Reads only.
pub(crate) fn plan(store: &Store, target: &str) -> Result<Plan, String> {
    let mut list = None;
    let mut roots: Vec<Task> = Vec::new();
    if let Some(w) = store.worklist(target) {
        if !w.workspace.is_private() {
            return Err(format!("{} is already in {}: there is no unpublish in v1", w.id, w.workspace));
        }
        for g in w.groups() {
            for m in &g.members {
                let t = store.find_task(m).ok_or_else(|| format!("{} names {m}, which is no row here", w.id))?;
                roots.push(t);
            }
        }
        list = Some(w.id);
    } else {
        let t = store.task_or_why(target)?;
        if let Some(p) = t.parent.as_deref().filter(|p| !p.is_empty()) {
            return Err(format!("{} is a sub-task of {p}, and lives in its workspace: publish {p}", t.id));
        }
        if let Some((row, _)) = split_piece_id(&t.id) {
            return Err(format!("{} is a piece of {row}, and lives in its workspace: publish {row}", t.id));
        }
        roots.push(t);
    }
    if roots.is_empty() {
        return Err(format!("{target} has no rows to publish"));
    }

    // The closure: sub-tasks and pieces of anything in it, until nothing is added.
    let mut set: BTreeMap<String, Task> = roots.into_iter().map(|t| (t.id.clone(), t)).collect();
    let all = store.tasks();
    loop {
        let before = set.len();
        for t in &all {
            let kin = t.parent.as_deref().is_some_and(|p| set.contains_key(p))
                || split_piece_id(&t.id).is_some_and(|(row, _)| set.contains_key(row));
            if kin && !set.contains_key(&t.id) {
                set.insert(t.id.clone(), t.clone());
            }
        }
        if set.len() == before {
            break;
        }
    }

    for t in set.values() {
        if !t.workspace.is_private() {
            return Err(format!("{} is already in {}: there is no unpublish in v1", t.id, t.workspace));
        }
    }
    let claims = store.claims();
    if let Some(t) = set.values().find(|t| claims.contains_key(&t.id)) {
        return Err(format!(
            "{} is claimed: publishing it would put a row two seats could move, and the hub's lease verbs (wsp-466) are not built; release it first",
            t.id
        ));
    }

    let depth = |t: &Task| {
        let (mut d, mut at) = (0usize, t.id.clone());
        while let Some(p) = set.get(&at).and_then(|t| t.parent.clone()).filter(|p| set.contains_key(p)) {
            d += 1;
            at = p;
            if d > 64 {
                break;
            }
        }
        d
    };
    let mut rows: Vec<&Task> = set.values().filter(|t| split_piece_id(&t.id).is_none()).collect();
    rows.sort_by_key(|t| (depth(t), t.id.clone()));
    let pieces = set
        .values()
        .filter_map(|t| split_piece_id(&t.id).map(|(row, _)| (t.id.clone(), row.to_string())))
        .collect();
    Ok(Plan { list, rows: rows.iter().map(|t| (t.id.clone(), code_of(store, t))).collect(), pieces })
}

/// The highest number under `code` this store has used: a row live or archived,
/// the project's counter, and an id `ids.json` knows.
fn highest_local(store: &Store, code: &str) -> u64 {
    let prefix = format!("{code}-");
    let num = |id: &str| id.strip_prefix(&prefix).filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).and_then(|n| n.parse::<u64>().ok());
    let mut top = store.projects().iter().filter(|p| p.code() == code).map(|p| p.seq as u64).max().unwrap_or(0);
    for t in store.tasks().iter().chain(store.archived_tasks().iter()) {
        top = top.max(num(&t.id).unwrap_or(0));
    }
    for (k, v) in store.renamed_ids() {
        top = top.max(num(&k).unwrap_or(0)).max(num(&v).unwrap_or(0));
    }
    top
}

fn taken_here(store: &Store, id: &str, ids: &BTreeMap<String, String>) -> bool {
    store.task_path(id).exists() || ids.contains_key(id) || store.archived_tasks().iter().any(|t| t.id == id)
}

/// Ids from the hub for `n` rows of `code`, none of them one this store already
/// has: those are dropped and more asked for, above what came back.
pub(crate) fn allocate(
    store: &Store,
    client: &mut Client,
    ws: &HubWorkspace,
    code: &str,
    n: usize,
) -> Result<Vec<String>, String> {
    let ids = store.renamed_ids();
    let mut past = highest_local(store, code);
    let mut got: Vec<String> = Vec::new();
    for _ in 0..ROUNDS {
        let want = (n - got.len()) as u32;
        let back = client.ids_alloc(ws, code, want, Some(past)).map_err(say)?;
        for id in back {
            if let Some(num) = id.rsplit_once('-').and_then(|(_, n)| n.parse::<u64>().ok()) {
                past = past.max(num);
            }
            if !taken_here(store, &id, &ids) && !got.contains(&id) {
                got.push(id);
            }
        }
        if got.len() >= n {
            return Ok(got);
        }
    }
    Err(format!("the hub kept handing out ids this store already has under {code}"))
}

fn say(e: HubError) -> String {
    match &e {
        HubError::Refused(r) if format!("{r:?}").contains("Forbidden") => format!("forbidden: you are not a member of that workspace ({r:?})"),
        HubError::Failed(f) if format!("{:?}", f.code) == "Forbidden" => format!("forbidden: {}", f.text),
        _ => e.to_string(),
    }
}

/// The hub's client for this machine's login: the URL, token and CA the way the
/// sync worker reads them.
fn dial(home: &Path) -> Result<Client, String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let not_in = |why: String| format!("not logged in to a hub ({why}): `wsp login <hub-url>` first");
    let url = wsp_client::hub_url_in(env(wsp_client::HUB_URL_ENV), home).map_err(|e| not_in(e.to_string()))?;
    let cred = Credential::for_hub_in(env(wsp_client::HUB_TOKEN_ENV), home).map_err(|e| not_in(e.to_string()))?;
    let ca = wsp_client::hub_ca_in(env(wsp_client::HUB_CA_ENV), home).map_err(|e| e.to_string())?;
    Client::dial_hub_ca(&url, cred, ca.as_deref(), WAIT).map_err(|e| format!("the hub did not answer: {e}"))
}

/// What a publish did: old id → new id, in the order made.
pub(crate) fn publish(store: &Store, target: &str, ws: &WorkspaceId, home: &Path) -> Result<Vec<(String, String)>, String> {
    let plan = plan(store, target)?;
    let mut client = dial(home)?;
    let hub_ws = HubWorkspace(ws.to_string());
    let mut by_code: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (id, code) in &plan.rows {
        by_code.entry(code.clone()).or_default().push(id);
    }
    // The hub first, and every code before any write: a refusal changes nothing.
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (code, olds) in &by_code {
        let fresh = allocate(store, &mut client, &hub_ws, code, olds.len())?;
        for (old, new) in olds.iter().zip(fresh) {
            map.insert(old.to_string(), new);
        }
    }
    let mut made: Vec<(String, String)> = plan.rows.iter().map(|(id, _)| (id.clone(), map[id].clone())).collect();
    for (piece, row) in &plan.pieces {
        let n = split_piece_id(piece).map(|(_, n)| n).unwrap_or(0);
        let new = format!("{}-{n}", map[row]);
        map.insert(piece.clone(), new.clone());
        made.push((piece.clone(), new));
    }
    let lists: Vec<String> = plan.list.into_iter().collect();
    store.renumber_into(&map, ws, &lists).map_err(|e| format!("the hub gave the ids, but the renumbering failed part-way: {e}"))?;
    Ok(made)
}

pub fn run(store: &Store, args: &Args) -> i32 {
    let usage = "usage: wsp publish <row|list> --to shared:<name>";
    let [target] = args.rest.as_slice() else {
        eprintln!("{usage}");
        return 2;
    };
    let Some(to) = args.get("to") else {
        eprintln!("{usage}");
        return 2;
    };
    let ws: WorkspaceId = match to.trim().parse() {
        Ok(w) if !WorkspaceId::is_private(&w) => w,
        _ => {
            eprintln!("wsp: publish: --to `{to}` is not a shared workspace: it is `shared:<name>`");
            return 2;
        }
    };
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        eprintln!("wsp: publish: HOME is not set, so there is no login to read");
        return 1;
    };
    match publish(store, target, &ws, &home) {
        Ok(made) => {
            if args.json() {
                let rows: Vec<_> = made.iter().map(|(a, b)| serde_json::json!({ "from": a, "to": b })).collect();
                println!("{}", serde_json::json!({ "workspace": ws.to_string(), "renumbered": rows }));
            } else {
                for (old, new) in &made {
                    println!("{old} -> {new}");
                }
                println!("published into {ws}: the sync worker pushes it on its next turn");
            }
            0
        }
        Err(why) => {
            eprintln!("wsp: publish: {why}");
            1
        }
    }
}

/// The verbs this module answers to.
pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["publish"],
    run,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Machines,
        order: 105,
        text: r##"  wsp publish <row|list> --to shared:<name>   move a private row (with its sub-tasks and pieces)
                                    or list (with its members) into a shared workspace:
                                    new ids from the hub, old ones still resolve. Refused
                                    when logged out, already shared, claimed, or not a member"##,
    }],
}];
