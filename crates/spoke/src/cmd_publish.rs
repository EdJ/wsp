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
//! workspace), and a workspace the hub says the caller is not a member of.
//!
//! A claimed row is published under a hub lease (wsp-466): once the hub has
//! given its new id, `lease.take` is asked for it before anything is written,
//! and a refusal (another user's lease, an allocation to someone else) leaves
//! every row as it was. The lease's token and expiry go into the claim, which
//! follows the row to its new id with its seat.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::hub_client::{self, Client, Credential, Error as HubError};
use wsp_objects::WorkspaceId as HubWorkspace;

use wsp_engine::base::args::Args;
use wsp_engine::model::{Task, WorkspaceId};
use wsp_engine::store::{split_piece_id, Store, INBOX_CODE};

const WAIT: Duration = Duration::from_secs(10);

/// How many times an allocation that came back with ids already used here is asked again.
const ROUNDS: usize = 8;

/// What a publish would move.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// The list, when the target is one.
    pub list: Option<String>,
    /// Rows that take a new id, parents before children, by project code.
    pub rows: Vec<(String, String)>,
    /// Pieces, each with the row it belongs to.
    pub pieces: Vec<(String, String)>,
}

fn code_of(store: &Store, t: &Task) -> String {
    t.project.as_deref().and_then(|p| store.project(p)).map(|p| p.code().to_string()).unwrap_or_else(|| INBOX_CODE.to_string())
}

/// What `target` takes with it, or why it cannot be published. Reads only.
pub fn plan(store: &Store, target: &str) -> Result<Plan, String> {
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

/// The highest number under `code` this store has used: the import door's
/// reading, which renumbers a private row out of a shared id's way too.
pub use wsp_engine::door::highest_local;

pub fn taken_here(store: &Store, id: &str, ids: &BTreeMap<String, String>) -> bool {
    store.task_path(id).exists() || ids.contains_key(id) || store.archived_tasks().iter().any(|t| t.id == id)
}

/// Ids from the hub for `n` rows of `code`, none of them one this store already
/// has: those are dropped and more asked for, above what came back.
pub fn allocate(
    store: &Store,
    client: &mut Client,
    ws: &HubWorkspace,
    code: &str,
    n: usize,
) -> Result<Vec<String>, String> {
    allocate_or(store, client, ws, code, n).map_err(|e| match e {
        Alloc::Hub(e) => say(*e),
        Alloc::Exhausted(why) => why,
    })
}

/// Why [`allocate_or`] gave no ids: the hub's error (a refusal, or no answer),
/// or ids it kept handing out that this store already has.
#[derive(Debug)]
pub enum Alloc {
    Hub(Box<HubError>),
    Exhausted(String),
}

impl Alloc {
    /// The hub did not answer (as against refusing): try again later.
    pub fn unreachable(&self) -> bool {
        matches!(self, Alloc::Hub(e) if crate::hub_sync::unreachable(e))
    }

    pub fn said(self) -> String {
        match self {
            Alloc::Hub(e) => say(*e),
            Alloc::Exhausted(why) => why,
        }
    }
}

/// [`allocate`] with the hub's error kept, so a caller can tell a refusal from
/// a hub that did not answer.
pub fn allocate_or(store: &Store, client: &mut Client, ws: &HubWorkspace, code: &str, n: usize) -> Result<Vec<String>, Alloc> {
    let ids = store.renamed_ids();
    let mut past = highest_local(store, code);
    let mut got: Vec<String> = Vec::new();
    for _ in 0..ROUNDS {
        let want = (n - got.len()) as u32;
        let back = client.ids_alloc(ws, code, want, Some(past)).map_err(|e| Alloc::Hub(Box::new(e)))?;
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
    Err(Alloc::Exhausted(format!("the hub kept handing out ids this store already has under {code}")))
}

fn say(e: HubError) -> String {
    match &e {
        HubError::Refused(r, _) if format!("{r:?}").contains("Forbidden") => format!("forbidden: you are not a member of that workspace ({r:?})"),
        HubError::Failed(f) if format!("{:?}", f.code) == "Forbidden" => format!("forbidden: {}", f.text),
        _ => e.to_string(),
    }
}

/// The hub's client for this machine's login: the URL, token and CA the way the
/// sync worker reads them.
pub fn dial(home: &Path) -> Result<Client, String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let not_in = |why: String| format!("not logged in to a hub ({why}): `wsp login <hub-url>` first");
    let url = hub_client::hub_url_in(env(hub_client::HUB_URL_ENV), home).map_err(|e| not_in(e.to_string()))?;
    let cred = Credential::for_hub_in(env(hub_client::HUB_TOKEN_ENV), home).map_err(|e| not_in(e.to_string()))?;
    let ca = hub_client::hub_ca_in(env(hub_client::HUB_CA_ENV), home).map_err(|e| e.to_string())?;
    Client::dial_hub_ca(&url, cred, ca.as_deref(), WAIT).map_err(|e| format!("the hub did not answer: {e}"))
}

/// What a publish did: old id → new id, in the order made.
pub fn publish(store: &Store, target: &str, ws: &WorkspaceId, home: &Path) -> Result<Vec<(String, String)>, String> {
    let plan = plan(store, target)?;
    let mut client = dial(home)?;
    let map = allocate_map(store, &mut client, ws, &plan).map_err(Alloc::said)?;
    renumber_with(store, &mut client, ws, &plan, map)
}

/// The hub's new ids for every row of `plan`, by its project's counter: old →
/// new. Every code before any write, so a refusal changes nothing.
pub fn allocate_map(store: &Store, client: &mut Client, ws: &WorkspaceId, plan: &Plan) -> Result<BTreeMap<String, String>, Alloc> {
    let hub_ws = HubWorkspace(ws.to_string());
    let mut by_code: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (id, code) in &plan.rows {
        by_code.entry(code.clone()).or_default().push(id);
    }
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (code, olds) in &by_code {
        let fresh = allocate_or(store, client, &hub_ws, code, olds.len())?;
        for (old, new) in olds.iter().zip(fresh) {
            map.insert(old.to_string(), new);
        }
    }
    Ok(map)
}

/// Renames `plan`'s rows by `map` (pieces keep their number under their row's
/// new id) and files them in `ws`; a claimed row is leased at its new id
/// first, and a refusal leaves every row as it was. The claim and the seat's
/// binding follow the row, the lease in the claim. Safe to run again with the
/// same `map` after a crash part-way: a row already renamed is found under its
/// new id, and its claim is leased there.
pub fn renumber_with(
    store: &Store,
    client: &mut Client,
    ws: &WorkspaceId,
    plan: &Plan,
    mut map: BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    let hub_ws = HubWorkspace(ws.to_string());
    // A claimed row goes under a hub lease at its new id, before any write.
    let claims = store.claims();
    let mut leased: Vec<(String, String, crate::hub_lease::Held)> = Vec::new();
    for (old, _) in &plan.rows {
        if !claims.contains_key(old) && !claims.contains_key(&map[old]) {
            continue;
        }
        match crate::hub_lease::take(client, &hub_ws.0, &map[old]) {
            Ok(held) => leased.push((old.clone(), map[old].clone(), held)),
            Err(why) => {
                for (_, new, held) in &leased {
                    let _ = client.lease_release(&wsp_store_contract::RowId(new.clone()), held.token);
                }
                return Err(format!("{old} is claimed and the hub would not lease it, so nothing was published: {why}"));
            }
        }
    }
    let mut made: Vec<(String, String)> = plan.rows.iter().map(|(id, _)| (id.clone(), map[id].clone())).collect();
    for (piece, row) in &plan.pieces {
        let n = split_piece_id(piece).map(|(_, n)| n).unwrap_or(0);
        let new = format!("{}-{n}", map[row]);
        map.insert(piece.clone(), new.clone());
        made.push((piece.clone(), new));
    }
    let lists: Vec<String> = plan.list.iter().cloned().collect();
    store.renumber_into(&map, ws, &lists).map_err(|e| format!("the hub gave the ids, but the renumbering failed part-way: {e}"))?;
    // The claim, and the seat's binding, follow the row to its new id, with the lease in it.
    for (old, new, held) in &leased {
        let Some(mut claim) = claims.get(old).or_else(|| claims.get(new)).cloned() else { continue };
        crate::hub_lease::stamp(&mut claim, held);
        store.locked(|| {
            store.clear_claim(old);
            store.set_claim(new, claim);
            for pane in store.panes_for_task(old) {
                if let Some(mut b) = store.bindings().get(&pane).cloned() {
                    b["task_id"] = serde_json::json!(new);
                    store.set_binding(&pane, b);
                }
            }
        });
    }
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
    let Some(home) = hub_client::hub_home() else {
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
pub const VERBS: &[wsp_engine::verbs::Verb] = &[wsp_engine::verbs::Verb {
    names: &["publish"],
    run,
    help: &[wsp_engine::verbs::Help {
        section: wsp_engine::verbs::Section::Machines,
        order: 105,
        text: r##"  wsp publish <row|list> --to shared:<name>   move a private row (with its sub-tasks and pieces)
                                    or list (with its members) into a shared workspace:
                                    new ids from the hub, old ones still resolve. Refused
                                    when logged out, already shared, or not a member; a claimed
                                    row takes the hub's lease first and is refused if it is held"##,
    }],
}];
