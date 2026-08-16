//! Agent binding, context resolution, and the views that join the store to
//! herdr's live panes.

use serde_json::json;

use crate::herdr;
use crate::model::{Status, Task};
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::sync;
use crate::util::{self, Paint};
use crate::Args;

/// Every pane holding `task` that is not `me`, and that a live agent is
/// driving.
///
/// The one definition of "somebody else has this". `claim` refuses on it and
/// `next` declines to name it, and the two have to be the same rule: when they
/// disagreed, `next` named work `claim` would refuse, so an agent told to find
/// its own work was sent straight into a wall with nothing to try instead.
///
/// A *dead* pane's binding is not a holder — that is exactly the stale state a
/// re-claim exists to clear. Nor is a shell's: a person at a terminal can be
/// asked to move, and refusing on their behalf helps nobody.
pub fn live_holders<'a>(
    bindings: &std::collections::BTreeMap<String, serde_json::Value>,
    panes_now: &'a [herdr::Pane],
    task: &str,
    me: Option<&str>,
) -> Vec<&'a herdr::Pane> {
    panes_now
        .iter()
        .filter(|p| !p.agent.is_empty())
        .filter(|p| Some(p.pane_id.as_str()) != me)
        .filter(|p| {
            bindings
                .get(&p.pane_id)
                .and_then(|b| b.get("task_id"))
                .and_then(|x| x.as_str())
                == Some(task)
        })
        .collect()
}

/// The project the caller is standing in. `-p` always wins; otherwise the
/// precedence chain is pin > binding > mandate > cwd > workspace label.
pub fn current_project(
    store: &Store,
    args: &Args,
    index: &Index,
) -> Result<Option<String>, i32> {
    if let Some(p) = args.get("project") {
        if p == "none" || p == "inbox" {
            return Ok(None);
        }
        return match index.find(&p) {
            Some(found) => Ok(Some(found.id.clone())),
            None => {
                eprintln!("wsp: no such project `{p}`");
                Err(1)
            }
        };
    }

    let env = herdr::Env::read();
    let pins = store.pins();

    let bound_project = env.pane_id.as_ref().and_then(|pane| {
        store
            .bindings()
            .get(pane)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| store.task(id))
            .and_then(|t| t.project)
    });

    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());

    // A mandate is a statement about what this workspace is *for*, so it beats
    // the directory the shell happens to be sitting in — but not a pin, which
    // is a statement about what the workspace *is*, and not a binding, which is
    // the work actually in hand. Checked here rather than inside `resolve` so
    // that the panel and `overlap` go on placing panes by where they stand:
    // standing direction says nothing about which tree a pane is in.
    let mandate = crate::cmd_mandate::current(store, env.workspace_id.as_deref());

    let r = resolve::resolve(
        index,
        &pins,
        resolve::Held {
            binding: bound_project,
            claim: resolve::claimed_project(
                &store.claims(),
                &store.tasks(),
                env.workspace_id.as_deref(),
                None,
            ),
        },
        env.workspace_id.as_deref(),
        None,
        cwd.as_deref(),
    );
    // A claim is work in hand, like a binding, so it stands with the binding
    // above the mandate: what this workspace is *doing* beats what it is *for*
    // for as long as it is holding it.
    if matches!(r.source, "pin" | "binding" | "claim") && r.project.is_some() {
        return Ok(r.project);
    }
    if mandate.is_some() {
        return Ok(mandate);
    }
    if r.project.is_some() {
        return Ok(r.project);
    }

    // Last resort: ask herdr for this workspace's label.
    if let Some(ws) = env.workspace_id.as_deref() {
        if herdr::available() {
            if let Ok(list) = herdr::workspaces() {
                if let Some(w) = list.iter().find(|w| w.id == ws) {
                    return Ok(index.project_for_label(&w.label));
                }
            }
        }
    }
    Ok(None)
}

fn pane_id(args: &Args) -> Option<String> {
    args.get("pane").or_else(|| herdr::Env::read().pane_id)
}

/// The name a task lends the workspace and pane holding it, or `None` if it
/// lends none.
///
/// Capped at the 44 characters `sync` already gives the `task` token. A herdr
/// sidebar is 26 columns and draws its own ellipsis, so this is not about what
/// fits — it is about not putting a paragraph on the wire as a name.
///
/// `wsp` and `wsp:view` are withheld. They are how the panel finds its own
/// panes, and `install` adopts a stray pane labelled `wsp` as a panel it lost
/// track of — a task called "wsp" would hand it an agent instead.
fn task_label(title: &str) -> Option<String> {
    let label = util::truncate(title.trim(), 44);
    match label.as_str() {
        "" | crate::panel::PANEL_LABEL | crate::panel::VIEW_LABEL => None,
        _ => Some(label),
    }
}

/// Put the task's name on the workspace and the pane that took it up. Returns
/// the workspace's new label, if the workspace took it.
///
/// herdr has no name of its own for a workspace nobody named — `workspace.list`
/// answers with the agent standing in it, or the folder leaf — so three agents
/// in one tree all read as `claude`, which is the one thing about them you
/// already knew. A claim is the moment wsp knows better.
///
/// It renames over a name typed by hand, by decision on t-260815-041, and the
/// claim prints what it overwrote so `herdr workspace rename` can put it back.
/// The cost is `resolve`'s last resort: a workspace whose project was inferred
/// from a label like `Trance Video` loses that inference once the label is a
/// task title. Only ever last resort — a pin, this binding, or the cwd all beat
/// it, and a workspace that has just claimed has a binding by definition.
fn name_after_task(pane: &str, workspace: &str, title: &str) -> Option<String> {
    let label = task_label(title)?;
    if !herdr::available() {
        return None;
    }
    if !pane.is_empty() {
        let _ = herdr::rename_pane(pane, &label);
    }
    if workspace.is_empty() {
        return None;
    }
    herdr::rename_workspace(workspace, &label).ok().map(|_| label)
}

/// Take the task's name back off the pane and the workspace that held it.
///
/// The other half of [`name_after_task`], and the reason it is needed: a claim
/// writes the task's title over both, and until now nothing ever wrote it back.
/// An agent that handed its work in went on reading as that work — in the
/// sidebar, in the panel's rows, in `workspace.list` — so the one place you look
/// to find somebody free said the opposite.
///
/// Only what the claim wrote. A label that is no longer the task's title is a
/// name somebody typed since, and blanking that would be this function deciding
/// something it was never asked to. Empty rather than a guess at what was there
/// before: the overwritten name is printed by `claim` and kept nowhere, and
/// "nobody has named this" is at least true.
///
/// The workspace waits on the last binding in it. Two agents in one tree are
/// rare and one of them finishing is not a reason to unname the room they are
/// both standing in.
fn unname_after_task(store: &Store, pane: &str, task_id: &str) {
    if !herdr::available() {
        return;
    }
    let Some(label) = store.task(task_id).and_then(|t| task_label(&t.title)) else { return };
    let Ok(panes) = herdr::panes() else { return };
    let Some(p) = panes.iter().find(|p| p.pane_id == pane) else { return };

    if p.label == label {
        let _ = herdr::rename_pane(pane, "");
    }
    if p.workspace_id.is_empty() {
        return;
    }
    let still_bound = store.bindings().keys().any(|other| {
        other != pane
            && panes.iter().any(|q| q.pane_id == *other && q.workspace_id == p.workspace_id)
    });
    if still_bound {
        return;
    }
    if herdr::workspaces()
        .unwrap_or_default()
        .iter()
        .any(|w| w.id == p.workspace_id && w.label == label)
    {
        let _ = herdr::rename_workspace(&p.workspace_id, "");
    }
}

/// `wsp say "<what you are doing>"` — an agent says where it has got to.
///
/// The pane takes the sentence; the workspace keeps the task. That division is
/// the whole design: a workspace answers *what is this work*, which changes
/// when the task changes, and a pane answers *what is happening in there right
/// now*, which changes all the time. Putting both on the workspace would mean
/// the sidebar losing the name of the work every time somebody started a build.
///
/// `--clear`, or an empty sentence, puts the task's own name back — so there is
/// always a way home that does not need the agent to remember what it was
/// called. A claim resets it too, which is why `claim` names the pane as well
/// as the workspace rather than leaving it to this.
pub fn say(store: &Store, args: &Args) -> i32 {
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane to name — run this inside a herdr pane, or pass --pane");
        return 2;
    };
    if !herdr::available() {
        eprintln!("wsp: no herdr socket at {}", herdr::socket_path().display());
        return 1;
    }

    let said = args.text(0);
    let said = said.trim();

    // Home is the task this pane holds. Without one there is nothing to fall
    // back to, so the label is cleared outright and herdr goes back to naming
    // the pane whatever it named it before.
    let home = store
        .bindings()
        .get(&pane)
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .and_then(|id| store.task(id))
        .and_then(|t| task_label(&t.title));

    let label = match (said.is_empty() || args.has("clear"), &home) {
        (true, Some(h)) => Some(h.clone()),
        (true, None) => None,
        (false, _) => task_label(said),
    };

    let r = match &label {
        Some(l) => herdr::rename_pane(&pane, l),
        None => herdr::call("pane.rename", json!({ "pane_id": pane, "label": null })).map(|_| ()),
    };
    if let Err(e) = r {
        eprintln!("wsp: {e}");
        return 1;
    }

    if args.json() {
        println!("{}", json!({ "pane": pane, "label": label }));
    } else {
        let p = Paint::new();
        match &label {
            Some(l) => println!("{} {}", p.dim(&format!("{pane} ·")), l),
            None => println!("{}", p.dim(&format!("{pane} · cleared"))),
        }
    }
    0
}

/// What a pane wears while its agent is looking for a task.
///
/// State first, project second, because the ellipsis falls on the right: a
/// herdr sidebar draws 26 columns, and `looking for work in stra…` still says
/// the thing a person scanning a column of panes is scanning for. The project
/// is the half they can usually read off the workspace row anyway.
fn looking_label(project: Option<&str>, found: bool) -> String {
    match (found, project) {
        (true, Some(p)) => format!("looking for work in {p}"),
        (true, None) => "looking for work".into(),
        (false, Some(p)) => format!("nothing actionable in {p}"),
        (false, None) => "nothing actionable".into(),
    }
}

/// Say, on the pane, that this agent has nothing in hand and is looking.
///
/// The gap this fills is the one between being sent to find work and having
/// found it. `claim` names the pane after the task, so everything from the
/// claim onwards is visible from outside — but the minute or two before it, in
/// which an agent reads a backlog, writes an overview and decides, showed
/// whatever the pane was called last. From herdr that is indistinguishable
/// from an agent that read its instruction and did nothing.
///
/// Two conditions, and both are about the caller rather than the answer:
///
/// *An agent.* A person running `wsp next` in a shell is asking a question,
/// not reporting a state, and their pane is not ours to rename.
///
/// *Holding nothing.* A bound pane running `next` is peeking at what comes
/// after the thing it is in the middle of — and `next` deliberately keeps that
/// pane's own `doing` task in the running, so the answer is often the task it
/// already has. Renaming there would replace a true name with a false one. It
/// is also why this needs no separate hook for the panel's `f`: `find_work`
/// refuses on an agent that still holds a task, so an agent sent looking is
/// unbound by the time it asks.
///
/// Nothing here is worth failing a command over, so every failure is silent —
/// the same bargain `claim`'s rename makes.
pub fn say_looking(store: &Store, panes: &[herdr::Pane], project: Option<&str>, found: bool) {
    if !herdr::available() {
        return;
    }
    let Some(pane) = herdr::Env::read().pane_id else { return };
    let driven = panes.iter().any(|p| p.pane_id == pane && !p.agent.is_empty());
    if !driven {
        return;
    }
    let holds = store
        .bindings()
        .get(&pane)
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .is_some_and(|id| store.task(id).is_some());
    if holds {
        return;
    }
    let _ = herdr::rename_pane(&pane, &looking_label(project, found));
}

/// `Trance Video · 3h12m` — a claim as one line.
///
/// Both this and `worked_line` join what they have and skip what they do not,
/// because every part is optional: a claim made outside herdr has no label, and
/// one made before the clock was recorded has no duration. Formatting them with
/// fixed separators left `" · 3s · to t-260815-002"` hanging off nothing.
pub fn claim_line(c: &serde_json::Value) -> String {
    let get = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match (get("workspace_label"), get("workspace_id")) {
        ("", "") => {}
        ("", id) => parts.push(id.to_string()),
        (label, "") => parts.push(label.to_string()),
        (label, id) => parts.push(format!("{label} ({id})")),
    }
    let held = util::since(get("claimed_at"));
    if held > 0 {
        parts.push(util::duration_human(held));
    }
    parts.join(" · ")
}

/// `Trance Video · 3h12m · to t-260814-026` — the claim that ended.
pub fn worked_line(w: &serde_json::Value) -> String {
    let get = |k: &str| w.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match (get("workspace_label"), get("workspace_id")) {
        ("", id) if !id.is_empty() => parts.push(id.to_string()),
        (label, _) if !label.is_empty() => parts.push(label.to_string()),
        _ => {}
    }
    let spent = w.get("seconds").and_then(|x| x.as_i64()).unwrap_or(0);
    if spent > 0 {
        parts.push(util::duration_human(spent));
    }
    match get("handed_to") {
        "" => parts.push(get("reason").to_string()),
        next => parts.push(format!("to {next}")),
    }
    parts.join(" · ")
}

/// End a claim and leave the trace behind.
///
/// An agent works several tasks in sequence, so every way a claim can end —
/// handed to the next task, released by hand, finished — has to answer what
/// becomes of the one being left. It keeps its status, because `doing` with
/// nobody on it is a true and useful state: it is the work that is underway
/// and waiting for you. What it loses is the claim, and what it gains is the
/// record of who had it and for how long.
///
/// Does nothing when there was no claim: `done` on a task nobody ever picked
/// up must not write a line saying it was released.
pub fn hand_off(store: &Store, task_id: &str, to: Option<&str>, reason: &str) {
    let Some(claim) = store.claims().get(task_id).cloned() else {
        return;
    };
    let get = |k: &str| claim.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let from = get("claimed_at");
    let secs = util::since(&from);

    store.set_worked(
        task_id,
        json!({
            "workspace_id": get("workspace_id"),
            "workspace_label": get("workspace_label"),
            "cwd": get("cwd"),
            "host": get("host"),
            "from": from,
            "to": util::now_iso(),
            "seconds": secs,
            "handed_to": to,
            "reason": reason,
        }),
    );
    store.clear_claim(task_id);

    if let Some(mut t) = store.task(task_id) {
        let spent = if secs > 0 { format!(" after {}", util::duration_human(secs)) } else { String::new() };
        match to {
            Some(next) => t.log(&format!("handed off to {next}{spent}")),
            None => t.log(&format!("released{spent}")),
        }
        t.touch();
        let _ = store.save_task(&t);
    }
    store.log_event(
        if to.is_some() { "task-handoff" } else { "task-released" },
        json!({ "id": task_id, "to": to, "reason": reason, "seconds": secs }),
    );
}

/// Every task this pane, or the workspace it sits in, is about to stop working.
///
/// Two lists rather than one, because a claim outlives the pane that made it:
/// after a herdr restart the binding is gone and only the claim says the
/// workspace was on that task, so migrating there without looking would leave
/// the old claim standing and `reconcile` would bind the agent straight back
/// to the task it had left.
fn leaving(store: &Store, pane: &str, workspace: &str, taking: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(prev) = store
        .bindings()
        .get(pane)
        .and_then(|b| b.get("task_id"))
        .and_then(|x| x.as_str())
    {
        if prev != taking {
            out.push(prev.to_string());
        }
    }
    if !workspace.is_empty() {
        for (task, c) in store.claims() {
            if task == taking || out.contains(&task) {
                continue;
            }
            if c.get("workspace_id").and_then(|x| x.as_str()) == Some(workspace) {
                out.push(task);
            }
        }
    }
    out
}

/// The question a `wsp block` left behind, if it can still be found.
///
/// `block` records it as a `blocked: …` line in the task's log and nowhere
/// else, and until now nothing read it back. The refusal below is where it is
/// worth the most: "someone owes you an answer" is only actionable if it says
/// what was asked, and the alternative is making the reader open the task to
/// find out why the command they just ran did nothing.
///
/// The last one wins — a task blocked twice is waiting on the second question.
/// The date is stepped over rather than matched, because `Task::log` is what
/// writes it; anything that is not `- <stamp> blocked: …` is somebody else's
/// log line and is not treated as one.
fn blocked_question(t: &Task) -> Option<String> {
    t.section("Log")?.lines().rev().find_map(|l| {
        let (_stamp, rest) = l.trim().strip_prefix("- ")?.split_once(' ')?;
        let q = rest.strip_prefix("blocked:")?.trim();
        (!q.is_empty()).then(|| q.to_string())
    })
}

pub fn claim(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp claim <id>   (inside a herdr pane)");
        return 2;
    };
    let Some(t) = store.find_task(&needle) else {
        eprintln!("wsp: no task matching `{needle}`");
        return 1;
    };
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane to bind — run this inside a herdr pane, or pass --pane");
        return 2;
    };

    let env = herdr::Env::read();
    let session = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();

    // Claiming on behalf of another pane — from the panel, say — means the
    // environment describes the caller, not the target. Ask herdr what that
    // pane actually belongs to, or the claim records a workspace that nothing
    // can later resolve.
    let panes_now = herdr::panes().unwrap_or_default();
    let target = panes_now.iter().find(|p| p.pane_id == pane).cloned();
    let workspace = target
        .as_ref()
        .map(|p| p.workspace_id.clone())
        .filter(|w| !w.is_empty())
        .or_else(|| env.workspace_id.clone())
        .unwrap_or_default();
    let cwd = match &target {
        Some(p) if !p.cwd.is_empty() => p.cwd.clone(),
        _ => std::env::current_dir().map(|c| util::contract(&c)).unwrap_or_default(),
    };

    // The other direction: a task taken off another agent. Two panes bound to
    // one task is not a state anything downstream can read — the tree hangs a
    // pane under the task it is bound to and takes the first it finds, so the
    // second would simply not be drawn.
    let displaced: Vec<String> =
        store.panes_for_task(&t.id).into_iter().filter(|p| p != &pane).collect();

    let bindings_now = store.bindings();

    // Claiming finished work reopens it, silently: the status goes back to
    // `doing` and the task rejoins every open list on the machine. That is
    // occasionally what you want — work comes back — but never by accident,
    // and a bare suffix like `005` resolving to something already done is
    // exactly how the accident happens. Found the hard way: this command was
    // pointed at a completed task while testing the refusal below, and quietly
    // undid somebody else's finished work.
    if !t.status().is_open() && !args.has("force") {
        let p = Paint::new();
        eprintln!("{} {}  {}", p.yellow("✗"), p.bold(&t.id), t.title);
        eprintln!("  {}", p.dim(&format!("already {} — claiming it would reopen it", t.status_raw)));
        eprintln!("  {}", p.dim(&format!("wsp claim {} --force   to pick it back up", t.id)));
        return 1;
    }

    // `blocked` is open — `is_open` is `!Done` — so the guard above never
    // covered it, and a claim on a blocked task ran to the end, where the
    // status is set to `doing`. The status is the only structured record that a
    // decision is owed: `block` writes its question to the log and nothing
    // reads it back, so the block was cleared by the claim and what remained
    // was one log line no list, count or panel row shows. The whole content of
    // `block` is "stop, someone owes you an answer", and this is the command
    // that was walking past it — twice over from the panel, which claims a task
    // and then types "work it" into the agent's pane.
    //
    // Refused rather than warned about, for the same reason as the two guards
    // around it: a line above the work still leaves the block gone.
    if t.status() == Status::Blocked && !args.has("force") {
        let p = Paint::new();
        eprintln!("{} {}  {}", p.yellow("✗"), p.bold(&t.id), t.title);
        match blocked_question(&t) {
            Some(q) => eprintln!("  {}", p.dim(&format!("blocked, waiting on an answer: {q}"))),
            None => eprintln!("  {}", p.dim("blocked — a decision is owed before it is worked")),
        }
        eprintln!("  {}", p.dim(&format!("wsp claim {} --force   to work it anyway", t.id)));
        return 1;
    }

    // Taking work off a *live* agent is almost always a mistake, and it used
    // to be silent: the binding was cleared, the other agent went on editing
    // files for a task the store said was ours, and the first anyone knew was
    // two commits fighting over the same lines. A dead pane's binding is
    // another matter — that is exactly the stale state a re-claim is for.
    //
    // Refused before anything is written, so a refusal costs nothing: this
    // agent has not yet let go of whatever it was holding.
    let held_by = live_holders(&bindings_now, &panes_now, &t.id, Some(&pane));
    if !held_by.is_empty() && !args.has("force") {
        let held = store
            .claims()
            .get(&t.id)
            .and_then(|c| c.get("claimed_at"))
            .and_then(|c| c.as_str())
            .map(|c| util::duration_human(util::since(c)))
            .unwrap_or_default();
        if args.json() {
            println!(
                "{}",
                json!({
                    "error": "held",
                    "task": t.id,
                    "held_by": held_by.iter().map(|p| json!({
                        "pane": p.pane_id, "agent": p.agent, "state": p.agent_status,
                    })).collect::<Vec<_>>(),
                    "held_for": held,
                })
            );
        } else {
            let p = Paint::new();
            eprintln!("{} {}  {}", p.yellow("✗"), p.bold(&t.id), t.title);
            for h in &held_by {
                eprintln!(
                    "  {}",
                    p.dim(&format!(
                        "held by {} in {} · {}{}",
                        if h.agent.is_empty() { "a shell" } else { &h.agent },
                        h.pane_id,
                        h.agent_status,
                        if held.is_empty() { String::new() } else { format!(" · {held}") }
                    ))
                );
            }
            eprintln!("  {}", p.dim(&format!("wsp claim {} --force   to take it anyway", t.id)));
        }
        return 1;
    }

    // Whatever this agent was on, it is not on it after this. The claim moves
    // with the agent; the trace stays with the task.
    let left = leaving(store, &pane, &workspace, &t.id);
    for task in &left {
        hand_off(store, task, Some(&t.id), "handoff");
    }

    let ws_label = herdr::workspaces()
        .unwrap_or_default()
        .into_iter()
        .find(|w| w.id == workspace)
        .map(|w| w.label)
        .unwrap_or_default();

    // Name the workspace and the pane after the work, before the claim is
    // written: the claim records the label the workspace is to be *found* by
    // when its id is gone, so it has to record the name it is about to have and
    // not the one it is losing.
    let named = name_after_task(&pane, &workspace, &t.title);

    // One lock around the state files a claim touches, so a claim
    // arriving in the middle of this one cannot read a half-made state: a
    // binding cleared and not yet replaced, or a claim recorded against a task
    // whose binding has not landed. The task file and its git commit are
    // outside it deliberately — a commit can take longer than any other agent
    // should be made to wait, and `wsp reconcile` already rebuilds bindings
    // from claims if the two ever part company.
    store.locked(|| {
        for other in &displaced {
            store.clear_binding(other);
        }
        store.set_binding(
            &pane,
            json!({
                "task_id": t.id,
                "pane_id": pane,
                "workspace_id": workspace,
                "agent_session_id": session,
                "cwd": cwd,
                "started_at": util::now_iso(),
            }),
        );

        // The durable half. A pane id is worthless the moment the pane dies,
        // so record the workspace instead — by id, and by the label and cwd
        // herdr keeps in its own session file, which survive the id being
        // reissued. The label is looked up before the lock: asking herdr is a
        // socket round-trip, and nothing else should wait on it.
        store.set_claim(
            &t.id,
            json!({
                "workspace_id": workspace,
                "workspace_label": named.clone().unwrap_or_else(|| ws_label.clone()),
                "cwd": cwd,
                "host": util::hostname(),
                "claimed_at": util::now_iso(),
            }),
        );
    });

    // `hand_off` wrote to the tasks it released, and one of them may be this
    // one — a re-claim of work the same agent put down. Re-read rather than
    // saving the copy taken before all that.
    let mut t = store.task(&t.id).unwrap_or(t);
    if t.status() != Status::Doing {
        t.set_status(Status::Doing);
    }
    for other in &displaced {
        t.log(&format!("taken over from pane {other}"));
    }
    match left.first() {
        Some(prev) => t.log(&format!("claimed by pane {pane}, taken up from {prev}")),
        None => t.log(&format!("claimed by pane {pane}")),
    }
    let _ = store.save_task(&t);
    store.log_event(
        "task-claimed",
        json!({ "id": t.id, "pane": pane, "from": left, "took_over": displaced }),
    );
    store.git_commit(&format!("wsp: claim {} — {}", t.id, t.title));

    // Reflect it in the sidebar immediately rather than waiting for the daemon.
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);

    if args.json() {
        println!(
            "{}",
            json!({
                "task": t.json(),
                "pane": pane,
                "from": left,
                "took_over": displaced,
                "named": named,
                "was": ws_label,
            })
        );
    } else {
        let p = Paint::new();
        println!("{} {}  {}", p.cyan("▸"), p.bold(&t.id), t.title);
        println!("  {}", p.dim(&format!("bound to {pane}")));
        // A rename is not free to the person who typed the old name, so say
        // what it was: that line is the whole of the undo.
        match &named {
            Some(l) if *l == ws_label => {}
            Some(_) if ws_label.is_empty() => println!("  {}", p.dim(&format!("named {workspace}"))),
            Some(_) => println!("  {}", p.dim(&format!("named {workspace} · was {ws_label}"))),
            None => {}
        }
        // Naming what was put down is the whole point of a migration being one
        // command: the agent moved, and you can see what it moved off.
        for prev in &left {
            let title = store.task(prev).map(|x| x.title).unwrap_or_default();
            println!("  {}", p.dim(&format!("left {prev}  {}", util::truncate(&title, 48))));
        }
        for other in &displaced {
            println!("  {}", p.dim(&format!("taken from {other}")));
        }
        // A title and nothing under it. The brief asks every agent to write the
        // work down because a bare title is unreadable a day later, and then the
        // claim hands it exactly that and says nothing — so the first move is to
        // reconstruct an overview from a sentence, silently, and get it wrong.
        // Said here because this is the one moment it is still cheap: the task
        // is in hand, nothing has been built on a guess about it yet, and the
        // person who knows what it meant may still be at the keyboard.
        if t.section("Overview").is_none() {
            println!(
                "  {}",
                p.dim(&format!(
                    "no overview — please write one before you start: wsp edit {} --overview -",
                    t.id
                ))
            );
        }
        // Claiming is the moment an agent commits to a tree, and the moment it
        // is cheapest to be told it is not alone in one. On 2026-08-15 two
        // agents worked this repo for twenty minutes without knowing, and this
        // line is where the first of them would have found out.
        let w = crate::overlap::World::live(store);
        let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
        let near: Vec<_> = crate::overlap::standing_beside(&w, &pane, cwd.as_deref())
            .into_iter()
            .filter(|s| s.relation.is_near())
            .collect();
        if !near.is_empty() {
            println!();
            println!(
                "  {}",
                p.yellow(&format!(
                    "{} else in this tree — commit with explicit paths",
                    near.len()
                ))
            );
            for s in near.iter().take(4) {
                println!(
                    "  {}",
                    p.dim(&format!("{}  {}", util::pad(&s.pane, 7), util::truncate(&s.name(), 52)))
                );
            }
            if near.len() > 4 {
                println!("  {}", p.dim(&format!("{} more · wsp overlap", near.len() - 4)));
            }
        }
    }
    0
}

/// Rebuild bindings from claims and whatever herdr currently has open.
///
/// This is what makes a binding disposable. herdr restores workspaces, their
/// layouts and their agent sessions across a restart, but pane ids are not
/// stable across it and nothing outside wsp knows which task a pane was on.
/// So: for every claim belonging to this host, find the workspace it names —
/// by id, or failing that by the label and cwd herdr persists — and bind its
/// most plausible pane.
///
/// What one pass put right.
#[derive(Debug, Default, Clone, Copy)]
pub struct Reconciled {
    /// Bindings re-established from claims.
    pub bound: usize,
    /// Workspaces and panes given the name of the task they hold.
    pub named: usize,
    /// Claims dropped because the workspace holding them is gone.
    pub reaped: usize,
}

/// Returns what it put right.
///
/// `reap` additionally ends every claim whose workspace herdr no longer knows —
/// the other half of retiring a set of workspaces, because a claim naming one
/// that has been closed goes on saying work is being done in it, and herdr
/// hands ids out again. Asked for rather than automatic: a claim outliving its
/// *pane* is an accident of process lifetime and must stand, and a daemon
/// starting before herdr has finished restoring a session would otherwise read
/// a half-built world as a mass closure.
pub fn reconcile(store: &Store, reap: bool) -> Reconciled {
    let mut out = Reconciled::default();
    let claims = store.claims();
    if claims.is_empty() {
        return out;
    }
    let Ok(panes) = herdr::panes() else { return out };
    let workspaces = herdr::workspaces().unwrap_or_default();
    let host = util::hostname();

    // Nothing at all is a herdr that is not answering properly, not a machine
    // with no workspaces on it: there is one open to have asked from.
    if reap && !workspaces.is_empty() {
        for (task_id, c) in &claims {
            let get = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("");
            if !get("host").is_empty() && get("host") != host {
                continue;
            }
            let alive = workspaces.iter().any(|w| {
                w.id == get("workspace_id")
                    || (!get("workspace_label").is_empty() && w.label == get("workspace_label"))
            });
            if alive {
                continue;
            }
            hand_off(store, task_id, None, "workspace closed");
            out.reaped += 1;
        }
        // Same reason as `release`: the log lines are written here, so the
        // commit that carries them is this one. Nothing is written when
        // nothing was reaped, and `git_commit` is a no-op on that.
        store.git_commit(&format!("wsp: release {} task(s) whose workspace closed", out.reaped));
    }
    let claims = if out.reaped > 0 { store.claims() } else { claims };

    let bindings = store.bindings();
    let already: Vec<String> = bindings
        .values()
        .filter_map(|b| b.get("task_id").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .collect();

    // A pane holds one task, here as everywhere else. Two claims naming the
    // same workspace used to pick the same pane and the second quietly
    // overwrote the first — and claims are walked in id order, so an agent came
    // back from a restart bound to the *older* task, the one it had left.
    let mut taken: Vec<String> = bindings.keys().cloned().collect();

    for (task_id, c) in &claims {
        if already.iter().any(|t| t == task_id) {
            continue;
        }
        let get = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("");
        // A claim made on another machine says nothing about this one.
        if !get("host").is_empty() && get("host") != host {
            continue;
        }

        let want_id = get("workspace_id");
        let want_label = get("workspace_label");
        let want_cwd = get("cwd");

        // The id if it still names a workspace; otherwise the label and cwd,
        // which is what survives a workspace being rebuilt under a new id.
        let ws = workspaces
            .iter()
            .find(|w| w.id == want_id)
            .or_else(|| {
                workspaces.iter().find(|w| !want_label.is_empty() && w.label == want_label)
            })
            .map(|w| w.id.clone());
        let Some(ws) = ws else { continue };

        // Prefer a pane running an agent, then any pane that is not one of our
        // own panels. A workspace normally has exactly one candidate.
        let mut candidates: Vec<&herdr::Pane> = panes
            .iter()
            .filter(|p| p.workspace_id == ws && p.label != crate::panel::PANEL_LABEL)
            .collect();
        candidates.sort_by_key(|p| u8::from(p.agent.is_empty()));
        let Some(pane) = candidates.iter().find(|p| !taken.contains(&p.pane_id)) else { continue };
        taken.push(pane.pane_id.clone());

        store.set_binding(
            &pane.pane_id,
            json!({
                "task_id": task_id,
                "pane_id": pane.pane_id,
                "workspace_id": ws,
                "agent_session_id": pane.session_id,
                "cwd": if want_cwd.is_empty() { pane.cwd.clone() } else { want_cwd.to_string() },
                "started_at": util::now_iso(),
                "reconciled": true,
            }),
        );
        store.log_event(
            "task-reconciled",
            json!({ "id": task_id, "pane": pane.pane_id, "workspace": ws }),
        );
        out.bound += 1;
    }

    out.named = name_bound(store, &panes, &workspaces);
    out
}

/// Give every bound pane, and the workspace it stands in, the name of the task
/// it is holding.
///
/// `claim` does this at the moment it happens, which covers everything claimed
/// since — but only that. A pane that took its task up before any of this
/// existed keeps whatever herdr called it, and a claim whose rename was
/// dropped on a slow socket keeps it too, silently, because the rename is not
/// worth failing a claim over. This is where both are put right.
///
/// Deliberately not in `sync`: that runs every tick, and a name reasserted
/// every tick is a name you cannot change by hand. Here it runs when the
/// daemon starts and when somebody asks — so a name you type survives until
/// the next reconcile, which is the trade the user picked on t-260815-041.
fn name_bound(
    store: &Store,
    panes: &[herdr::Pane],
    workspaces: &[herdr::Workspace],
) -> usize {
    let tasks = store.tasks();
    let mut named = 0;
    for (pane_id, b) in store.bindings() {
        let Some(label) = b
            .get("task_id")
            .and_then(|t| t.as_str())
            .and_then(|id| tasks.iter().find(|t| t.id == id))
            .and_then(|t| task_label(&t.title))
        else {
            continue;
        };
        let Some(pane) = panes.iter().find(|p| p.pane_id == pane_id) else { continue };

        let mut touched = false;
        if pane.label != label {
            touched |= herdr::rename_pane(&pane.pane_id, &label).is_ok();
        }
        // A workspace nobody named reads back as the agent or the folder, so
        // the comparison can never say "already right" — it says "not the task
        // title", which is the same answer and the one that matters.
        if workspaces.iter().any(|w| w.id == pane.workspace_id && w.label != label) {
            touched |= herdr::rename_workspace(&pane.workspace_id, &label).is_ok();
        }
        if touched {
            named += 1;
        }
    }
    named
}

pub fn release(store: &Store, args: &Args) -> i32 {
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane — pass --pane or run inside herdr");
        return 2;
    };
    let had = store.bindings().get(&pane).cloned();
    let removed = store.clear_binding(&pane);
    if removed {
        if let Some(task_id) = had.as_ref().and_then(|b| b.get("task_id")).and_then(|t| t.as_str()) {
            // The name goes back before the log line does, and while the task
            // is still readable: `unname_after_task` needs the title to know
            // whether the label it is looking at is one we wrote.
            unname_after_task(store, &pane, task_id);
            // Releasing is a decision, so it clears the durable claim too —
            // unlike a pane exiting, which is only ever an accident of process
            // lifetime and must leave the intent standing. It ends the same way
            // a migration does, and leaves the same record behind.
            hand_off(store, task_id, None, "release");
            // `hand_off` writes the release into the task's log, and until
            // commits were scoped to what a command wrote, that line waited
            // for some later command to sweep it up — under that command's
            // message. It belongs to this one.
            store.git_commit(&format!("wsp: release {task_id}"));
        }
        let mut cache = sync::Cache::default();
        let _ = sync::sync(store, &mut cache, true);
    }
    if args.json() {
        println!("{}", json!({ "pane": pane, "released": removed }));
    } else if removed {
        println!("released {pane}");
    } else {
        println!("nothing bound to {pane}");
    }
    0
}

pub fn pin(store: &Store, args: &Args) -> i32 {
    // `--top` marks a workspace as belonging to no project on purpose: the
    // home for whatever runs the whole space, and for terminals that are not
    // work. Without it, "no project" only ever means "nothing resolved", and
    // the two are not the same thing.
    if args.has("top") {
        let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
            eprintln!("wsp: no workspace — pass -w, or run inside herdr");
            return 2;
        };
        store.set_pin(&ws, crate::resolve::TOP_LEVEL);
        if args.json() {
            println!("{}", json!({ "workspace": ws, "project": null, "top": true }));
        } else {
            println!("workspace {ws} pinned outside the project tree");
        }
        return 0;
    }
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp pin <project> [-w workspace] | wsp pin --top");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(proj) = index.find(&needle) else {
        eprintln!("wsp: no such project `{needle}`");
        return 1;
    };
    let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };

    store.set_pin(&ws, &proj.id);
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);

    if args.json() {
        println!("{}", json!({ "workspace": ws, "project": proj.id }));
    } else {
        println!("workspace {ws} pinned to {}", proj.id);
    }
    0
}

pub fn unpin(store: &Store, args: &Args) -> i32 {
    let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };
    let removed = store.clear_pin(&ws);
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);
    if args.json() {
        println!("{}", json!({ "workspace": ws, "unpinned": removed }));
    } else if removed {
        println!("workspace {ws} unpinned");
    } else {
        println!("workspace {ws} was not pinned");
    }
    0
}

pub fn where_am_i(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let env = herdr::Env::read();
    let pins = store.pins();
    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());

    let binding = env.pane_id.as_ref().and_then(|p| store.bindings().get(p).cloned());
    let bound_task = binding
        .as_ref()
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .and_then(|id| store.task(id));

    let label = match (&env.workspace_id, herdr::available()) {
        (Some(ws), true) => herdr::workspaces()
            .ok()
            .and_then(|list| list.into_iter().find(|w| &w.id == ws).map(|w| w.label)),
        _ => None,
    };

    let r = resolve::resolve(
        &index,
        &pins,
        resolve::Held {
            binding: bound_task.as_ref().and_then(|t| t.project.clone()),
            claim: resolve::claimed_project(
                &store.claims(),
                &store.tasks(),
                env.workspace_id.as_deref(),
                label.as_deref(),
            ),
        },
        env.workspace_id.as_deref(),
        label.as_deref(),
        cwd.as_deref(),
    );

    let tags = r.project.as_ref().map(|p| index.effective_tags(p)).unwrap_or_default();
    // What cwd alone would have said — worth showing, because a claimed pane
    // keeps its project even after you cd somewhere else.
    let by_cwd = cwd.as_deref().and_then(|c| index.project_for_cwd(c));

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": r.project,
                "source": r.source,
                "tags": tags,
                "by_cwd": by_cwd,
                "workspace_id": env.workspace_id,
                "workspace_label": label,
                "pane_id": env.pane_id,
                "cwd": cwd,
                "task": bound_task.as_ref().map(|t| t.json()),
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    match &r.project {
        Some(proj) => {
            println!("{}  {}", p.bold(proj), p.dim(&format!("via {}", r.source)));
            if !tags.is_empty() {
                println!("{}", p.dim(&tags.join(" ")));
            }
        }
        None => println!("{}", p.dim("no project resolved for this pane")),
    }
    if let Some(t) = &bound_task {
        println!("\n{} {}  {}", p.cyan("▸"), p.bold(&t.id), t.title);
    }
    if let Some(c) = &by_cwd {
        if Some(c) != r.project.as_ref() {
            println!("\n{}", p.dim(&format!("cwd alone would say {c} — `wsp release` to follow the directory instead")));
        }
    }
    0
}

pub fn wip(store: &Store, args: &Args) -> i32 {
    let index = Index::new(store.projects());
    let tasks = store.tasks();
    let bindings = store.bindings();
    let claims = store.claims();
    let pins = store.pins();

    let agents = if herdr::available() { herdr::agents().unwrap_or_default() } else { Vec::new() };
    let workspaces = if herdr::available() { herdr::workspaces().unwrap_or_default() } else { Vec::new() };

    struct Row {
        project: String,
        task: String,
        task_id: String,
        pane: String,
        workspace: String,
        state: String,
        needs_you: bool,
    }

    let mut rows: Vec<Row> = Vec::new();
    for a in &agents {
        let bound = bindings
            .get(&a.pane_id)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| tasks.iter().find(|t| t.id == id));

        let label = workspaces.iter().find(|w| w.id == a.workspace_id).map(|w| w.label.clone());
        let r = resolve::resolve(
            &index,
            &pins,
            resolve::Held {
                binding: bound.and_then(|t| t.project.clone()),
                claim: resolve::claimed_project(
                    &claims,
                    &tasks,
                    Some(&a.workspace_id),
                    label.as_deref(),
                ),
            },
            Some(&a.workspace_id),
            label.as_deref(),
            Some(&a.cwd),
        );

        let idle = a.agent_status == "idle";
        let needs_you = idle && bound.map(|t| t.status() == Status::Doing).unwrap_or(false);

        rows.push(Row {
            project: r.project.unwrap_or_else(|| "—".into()),
            task: bound
                .map(|t| t.title.clone())
                .unwrap_or_else(|| if a.title.is_empty() { "(unbound)".into() } else { format!("({})", a.title) }),
            task_id: bound.map(|t| t.id.clone()).unwrap_or_default(),
            pane: a.pane_id.clone(),
            workspace: label.unwrap_or_default(),
            state: a.agent_status.clone(),
            needs_you,
        });
    }
    rows.sort_by(|a, b| a.project.cmp(&b.project).then(a.pane.cmp(&b.pane)));

    let blocked: Vec<_> = tasks.iter().filter(|t| t.status() == Status::Blocked).collect();
    let in_review: Vec<_> = tasks.iter().filter(|t| t.status() == Status::Review).collect();
    let inbox = tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count();
    let needs = rows.iter().filter(|r| r.needs_you).count();

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "agents": rows.iter().map(|r| json!({
                    "project": r.project, "task": r.task, "task_id": r.task_id,
                    "pane": r.pane, "workspace": r.workspace, "state": r.state,
                    "needs_you": r.needs_you,
                })).collect::<Vec<_>>(),
                "needs_you": needs,
                "blocked": blocked.iter().map(|t| t.json()).collect::<Vec<_>>(),
                "review": in_review.iter().map(|t| t.json()).collect::<Vec<_>>(),
                "inbox": inbox,
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    if rows.is_empty() {
        println!("{}", p.dim("no agents running"));
    } else {
        println!(
            "{}  ·  {} agents  ·  {}",
            p.bold("WIP"),
            rows.len(),
            if needs > 0 { p.yellow(&format!("{needs} need you")) } else { p.dim("all busy") }
        );
        println!();
        let pw = rows.iter().map(|r| r.project.chars().count()).max().unwrap_or(7).max(7);
        let tw = 46;
        println!(
            "{}  {}  {}  {}",
            p.dim(&util::pad("PROJECT", pw)),
            p.dim(&util::pad("TASK", tw)),
            p.dim(&util::pad("PANE", 7)),
            p.dim("STATE")
        );
        for r in &rows {
            let state = match r.state.as_str() {
                "working" => p.green(&util::pad("working", 8)),
                "idle" => p.dim(&util::pad("idle", 8)),
                other => p.dim(&util::pad(other, 8)),
            };
            let flag = if r.needs_you { p.yellow("← needs you") } else { String::new() };
            println!(
                "{}  {}  {}  {} {}",
                util::pad(&r.project, pw),
                util::pad(&util::truncate(&r.task, tw), tw),
                p.dim(&util::pad(&r.pane, 7)),
                state,
                flag
            );
        }
    }

    // Blocked work, named. Under `--terse` the count alone: `wip` is asked
    // repeatedly through a session to see who is free, and the answer to that
    // moves every few minutes while this list does not — a task is blocked
    // because it is waiting on a person, which is the slowest thing here.
    // Still a line, because a count going up is the reason you would go and
    // read it.
    if !blocked.is_empty() {
        println!();
        if args.terse() {
            println!("{}  {}   {}", p.red(&util::pad("BLOCKED", 8)), blocked.len(), p.dim("wsp ls -s blocked"));
        } else {
            println!("{}  {}", p.red(&util::pad("BLOCKED", 8)), blocked.len());
            for t in &blocked {
                println!(
                    "  {}  {}  {}",
                    p.dim(&t.id),
                    util::pad(&t.project.clone().unwrap_or_else(|| "—".into()), 8),
                    util::truncate(&t.title, 56)
                );
            }
        }
    }

    // Work an agent has finished with. `review` is the agent's terminal verb —
    // it stops there and says so, and only a person says `done` — so this is
    // the list of things waiting on you rather than on anybody working.
    if !in_review.is_empty() {
        println!();
        println!("{}  {}   {}", p.yellow(&util::pad("REVIEW", 8)), in_review.len(), p.dim("wsp done <id> · wsp reopen <id>"));
        for t in &in_review {
            println!(
                "  {}  {}  {}",
                p.dim(&t.id),
                util::pad(&t.project.clone().unwrap_or_else(|| "—".into()), 8),
                util::truncate(&t.title, 56)
            );
        }
    }
    if inbox > 0 {
        println!("\n{}  {}   {}", p.dim(&util::pad("INBOX", 8)), inbox, p.dim("wsp inbox"));
    }
    0
}

/// `wsp overlap` — who else is standing in this tree.
///
/// The near set first and by itself, because that is the answer to the question
/// worth asking: who can reach the files under my hands. Everyone else follows
/// under a rule, as context — they cannot clobber this checkout, but knowing
/// the tree is busy is worth a line.
pub fn overlap(store: &Store, args: &Args) -> i32 {
    let w = crate::overlap::World::live(store);
    let me = pane_id(args).unwrap_or_default();
    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
    let all = crate::overlap::standing_beside(&w, &me, cwd.as_deref());
    let (near, far): (Vec<_>, Vec<_>) = all.iter().partition(|s| s.relation.is_near());

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "pane": me,
                "cwd": cwd,
                "near": near.iter().map(|s| s.json()).collect::<Vec<_>>(),
                "elsewhere": far.iter().map(|s| s.json()).collect::<Vec<_>>(),
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    if all.is_empty() {
        println!("{}", p.dim("nobody else is standing anywhere"));
        return 0;
    }

    let row = |s: &crate::overlap::Standing, warn: bool| {
        let who = util::truncate(&s.name(), 44);
        let name = if warn { p.yellow(&util::pad(&who, 44)) } else { p.dim(&util::pad(&who, 44)) };
        let held = match s.since {
            Some(secs) => util::duration_human(secs),
            None => String::new(),
        };
        println!(
            "  {} {}  {}  {}  {}{}",
            // A shell is marked rather than described: nobody is driving it,
            // but a person at one can overwrite a file as thoroughly as an
            // agent can, and the panel already means this by `▫`.
            p.dim(if s.agent { " " } else { crate::panel::glyph::SHELL }),
            p.dim(&util::pad(&s.pane, 7)),
            name,
            p.dim(&util::pad(s.relation.as_str(), 13)),
            p.dim(&held),
            if s.needs_you() { p.yellow("  ← needs you") } else { String::new() }
        );
    };

    if near.is_empty() {
        println!("{}", p.dim("nobody else is standing in this tree"));
    } else {
        println!(
            "{}  {}",
            p.yellow(&format!("{} in this tree", near.len())),
            p.dim("— they can reach the files you are editing")
        );
        for s in &near {
            row(s, true);
        }
    }

    // Elsewhere is context, not a warning, and most of it is idle shells that
    // have been sitting in a directory since Tuesday. Name the ones doing
    // something; count the rest, because twenty rows of pane id would bury the
    // two lines above that actually matter.
    let (busy, quiet): (Vec<_>, Vec<_>) =
        far.into_iter().partition(|s| s.agent || s.task.is_some());
    if !busy.is_empty() {
        println!("\n{}", p.dim(&format!("{} elsewhere", busy.len())));
        for s in &busy {
            row(s, false);
        }
    }
    if !quiet.is_empty() {
        println!(
            "\n{}",
            p.dim(&format!(
                "{} idle {} in other trees · wsp wip",
                quiet.len(),
                if quiet.len() == 1 { "shell" } else { "shells" }
            ))
        );
    }
    0
}

/// The pane's content out of a `pane.read` reply.
///
/// It arrives wrapped — `{"type": "pane_read", "read": {…}}` — which the
/// schema's own `PaneReadResult` does not say, and reading it flat returns an
/// empty string rather than an error: the command worked, the pane looked
/// blank, and nothing said why. So unwrap it if the wrapper is there and read
/// it flat if it is not, and keep working whichever shape a later herdr sends.
fn read_body(r: &serde_json::Value) -> &serde_json::Value {
    r.get("read").unwrap_or(r)
}

/// `wsp peek [<target>]` — read what is actually on a pane.
///
/// Every interface bug on 2026-08-15 cost a round trip through a person's eyes:
/// a key that did nothing because the binary was stale, a cursor scrolled off
/// the pane, a closed column still showing a shell prompt. In each case the
/// pane was displaying the answer and the agent that wrote the code could not
/// see it, so the loop was guess, build, install, ask, wait.
///
/// herdr has been able to answer this all along — `pane.read` returns the
/// characters on a pane. What was missing is a way to name the pane you mean.
/// Nobody reaches for it under pressure if reaching for it starts with listing
/// every pane on the machine and picking through the JSON.
///
/// So the target resolves the way everything else in wsp resolves: by what you
/// call the thing, not by the id herdr filed it under.
pub fn peek(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let env = herdr::Env::read();
    let here = args.get("workspace").or(env.workspace_id.clone());
    let panes = herdr::panes().unwrap_or_default();
    let mine = |label: &str| {
        panes
            .iter()
            .find(|p| p.label == label && Some(&p.workspace_id) == here.as_ref())
            .or_else(|| panes.iter().find(|p| p.label == label))
            .map(|p| p.pane_id.clone())
    };

    let needle = args.rest.first().cloned().unwrap_or_default();
    let (target, what) = match needle.as_str() {
        // The common case, and the reason there is a default at all: "what
        // does the sidebar look like right now".
        "" | "panel" => (mine(crate::panel::PANEL_LABEL), "the panel".to_string()),
        "view" | "detail" => (mine(crate::panel::VIEW_LABEL), "the view".to_string()),
        "me" => (env.pane_id.clone(), "this pane".to_string()),
        // A pane id, given as herdr writes them.
        n if n.contains(':') && panes.iter().any(|p| p.pane_id == n) => {
            (Some(n.to_string()), format!("pane {n}"))
        }
        // Otherwise a task: whichever pane holds it. This is how you look at
        // what another agent is doing without knowing where it is sitting.
        n => match store.find_task(n) {
            Some(t) => {
                let pane = store.panes_for_task(&t.id).into_iter().next();
                (pane, format!("{} — {}", t.id, util::truncate(&t.title, 44)))
            }
            None => {
                eprintln!("wsp: no pane, task or project matching `{n}`");
                return 1;
            }
        },
    };

    let Some(pane) = target else {
        // Name what is missing rather than what was asked for: "no view pane"
        // is a fact you can act on, "nothing to peek at for view" is not.
        let hint = match needle.as_str() {
            "" | "panel" => "no panel in this workspace — `wsp panel install`",
            "view" | "detail" => "no view pane open — `↵` on a row in the panel opens one",
            _ => "nothing holds that — `wsp wip` says who holds what",
        };
        eprintln!("wsp: {hint}");
        return 1;
    };

    // `visible` is what is on the pane now, which is what "what does it look
    // like" means. `recent` reaches back through what has scrolled past, for
    // when the question is what happened rather than what is showing.
    let source = args.get("source").unwrap_or_else(|| "visible".into());
    let mut params = json!({ "pane_id": pane, "source": source, "format": "text" });
    if let Some(n) = args.get("lines").and_then(|l| l.parse::<u64>().ok()) {
        params["lines"] = json!(n);
    }
    let Ok(r) = herdr::call("pane.read", params) else {
        eprintln!("wsp: herdr would not read {pane}");
        return 1;
    };
    let body = read_body(&r);
    let text = body.get("text").and_then(|t| t.as_str()).unwrap_or("");

    if args.json() {
        println!("{}", serde_json::to_string_pretty(&json!({
            "pane": pane, "what": what, "source": source, "text": text,
            "truncated": body.get("truncated").and_then(|t| t.as_bool()).unwrap_or(false),
        })).unwrap_or_default());
        return 0;
    }

    let p = Paint::new();
    println!("{}", p.dim(&format!("{what}  ({pane})")));
    if text.trim().is_empty() {
        println!("{}", p.dim("(nothing on it)"));
    } else {
        println!("{}", text.trim_end());
    }
    0
}

pub fn sync_once(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket at {}", herdr::socket_path().display());
        return 1;
    }
    // A one-shot sync always forces: there is no warm cache to trust.
    let mut cache = sync::Cache::default();
    match sync::sync(store, &mut cache, true) {
        Ok(r) => {
            if args.json() {
                println!(
                    "{}",
                    json!({ "workspaces": r.workspaces, "panes": r.panes, "reaped": r.reaped })
                );
            } else {
                println!(
                    "synced {} workspaces, {} panes{}",
                    r.workspaces,
                    r.panes,
                    if r.reaped > 0 { format!(", reaped {} stale binding(s)", r.reaped) } else { String::new() }
                );
            }
            0
        }
        Err(e) => {
            eprintln!("wsp: sync failed: {e}");
            1
        }
    }
}

/// Entry point for herdr `[[events]]` hooks. The event name arrives as an
/// argument, its payload in `HERDR_PLUGIN_EVENT_JSON`.
pub fn hook(store: &Store, args: &Args) -> i32 {
    let event = args
        .rest
        .first()
        .cloned()
        .or_else(|| std::env::var("HERDR_PLUGIN_EVENT").ok())
        .unwrap_or_default();
    let env = herdr::Env::read();

    match event.as_str() {
        "pane.exited" | "pane.closed" | "pane_exited" | "pane_closed" => {
            // Only the binding. The claim survives, because a pane exiting is
            // an accident of process lifetime and says nothing about whether
            // the work is still yours — this is precisely the cascade that
            // once cleared every binding on the machine at a stroke.
            if let Some(pane) = env.pane_id.clone() {
                if store.clear_binding(&pane) {
                    store.log_event("pane-exited", json!({ "pane": pane }));
                }
            }
        }
        "workspace.created" | "workspace_created" => {
            if let Some(ws) = env.workspace_id.clone() {
                crate::panel::install_if_adopted(store, &ws);
            }
        }
        "workspace.closed" | "workspace_closed" => {
            if let Some(ws) = env.workspace_id.clone() {
                store.clear_pin(&ws);
            }
        }
        _ => {}
    }

    if !herdr::available() {
        return 0;
    }
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, false);
    0
}

/// Lines the index would drop that HEAD has and the files on disk still have.
///
/// Both arguments are `git diff --numstat` output: the first HEAD against the
/// index, the second HEAD against the working tree. Per path, the index's
/// deletions minus the disk's are what staging that path would throw away and
/// nothing would still hold — the disk's own deletions are somebody's edit,
/// which is theirs to make.
///
/// That subtraction is what keeps this quiet for the ordinary case. An agent
/// halfway through the commit procedure has staged what it wrote, so the index
/// and the disk delete the same lines and the difference is zero; it stays zero
/// while they carry on editing, because everything the index drops the disk
/// drops too. Only an index holding something *older* than both — a `read-tree`
/// or a `reset` that landed on the shared file — deletes what the disk still
/// has.
fn index_loss(staged: &str, disk: &str) -> u64 {
    let deletions = |numstat: &str| -> std::collections::BTreeMap<String, u64> {
        numstat
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let (_add, del, path) = (f.next()?, f.next()?, f.next()?);
                // `-` for a binary file, where git counts no lines at all.
                Some((path.to_string(), del.parse::<u64>().ok()?))
            })
            .collect()
    };
    let disk = deletions(disk);
    deletions(staged)
        .iter()
        .map(|(path, del)| del.saturating_sub(disk.get(path).copied().unwrap_or(0)))
        .sum()
}

/// The same question, asked of a working tree.
///
/// `GIT_INDEX_FILE` is stripped deliberately. The commit procedure has every
/// agent working through a private index, so a `doctor` run from inside one
/// would otherwise inspect that agent's own staging and pronounce the shared
/// index — the one that is actually loaded — healthy.
///
/// `--no-renames` for the same reason a rename would break it: numstat writes
/// a rename as `old => new` in one field, which matches nothing in the other
/// listing and would count the whole file as loss.
fn tree_index_loss(root: &std::path::Path) -> Option<u64> {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    // One call answers three questions at once: a directory that is not a git
    // repo fails here, and so does a repo with no commit to be behind.
    let staged = git(&["diff", "--cached", "--numstat", "--no-renames"])?;
    if staged.trim().is_empty() {
        return Some(0);
    }
    let disk = git(&["diff", "--numstat", "--no-renames", "HEAD"])?;
    Some(index_loss(&staged, &disk))
}

/// What is sitting in the store that no command wrote.
///
/// Every `wsp` command commits the files it writes, so whatever is left is a
/// hand edit — `agents.md` is the usual one, a hook script the other. That
/// used to be invisible because the next command along swept it into its own
/// commit; now it waits, correctly, for the person who made it, and the only
/// thing missing is anybody being told.
fn store_uncommitted(root: &std::path::Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain"])
        .env_remove("GIT_INDEX_FILE")
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.get(3..).map(|p| p.trim().to_string()))
        .filter(|p| !p.is_empty())
        .collect()
}

pub fn doctor(store: &Store, args: &Args) -> i32 {
    let mut problems: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    if !store.exists() {
        problems.push(format!("no store at {} — run `wsp init`", util::contract(&store.root)));
    } else {
        notes.push(format!("store {}", util::contract(&store.root)));
    }
    if !store.root.join(".git").exists() {
        notes.push("store is not a git repo (history disabled)".into());
    } else {
        let loose = store_uncommitted(&store.root);
        if !loose.is_empty() {
            let named: Vec<&str> = loose.iter().take(3).map(|s| s.as_str()).collect();
            let rest = match loose.len() > named.len() {
                true => format!(" and {} more", loose.len() - named.len()),
                false => String::new(),
            };
            notes.push(format!(
                "uncommitted in the store: {}{rest} — wsp commits what it writes, so these are yours to commit",
                named.join(", ")
            ));
        }
    }

    let index = Index::new(store.projects());
    let tasks = store.tasks();
    notes.push(format!("{} projects, {} tasks", index.projects.len(), tasks.len()));

    for p in &index.projects {
        if let Some(parent) = &p.parent {
            if index.get(parent).is_none() {
                problems.push(format!("project {} has missing parent `{}`", p.id, parent));
            }
        }
        // Cycle detection: walking ancestors returns to self.
        if index.ancestors(&p.id).contains(&p.id) {
            problems.push(format!("project {} is in a parent cycle", p.id));
        }
        for r in &p.roots {
            if !util::expand(r).exists() {
                problems.push(format!("project {} root does not exist: {}", p.id, r));
            }
        }
    }

    // A loaded index in a tree agents share.
    //
    // `git add` writes to one `.git/index` for everybody standing in a tree,
    // and whoever commits takes whatever is in it — which the commit procedure
    // answers by having each agent work through a private `GIT_INDEX_FILE`. The
    // failure that leaves behind is silent: one `read-tree` or `reset` run
    // without that variable set puts an *older* tree in the shared index, where
    // it sits looking like nothing at all until somebody who skipped the
    // procedure — or a person at a shell — runs a plain `git commit` and takes
    // it. On 2026-08-15 that was 4,962 lines behind HEAD in `~/claude/wsp` for
    // most of an afternoon, and every fact needed to say so was already here.
    //
    // Declared roots only. wsp knows where a project's work lives because
    // somebody said so, and a check that went hunting for git repositories
    // would be reporting on trees nobody asked it about.
    let mut seen: Vec<std::path::PathBuf> = Vec::new();
    for p in &index.projects {
        for r in &p.roots {
            let root = util::real(r);
            if seen.contains(&root) || !root.exists() {
                continue;
            }
            seen.push(root.clone());
            match tree_index_loss(&root) {
                Some(lost) if lost > 0 => {
                    problems.push(format!(
                        "{}: the staged index would drop {lost} line(s) that HEAD has and the files still have",
                        util::contract(&root)
                    ));
                    problems.push(
                        "  a plain `git commit` there takes it as it stands — `git read-tree HEAD`, with no GIT_INDEX_FILE set, puts it back".into(),
                    );
                }
                _ => {}
            }
        }
    }

    for t in &tasks {
        if let Some(proj) = &t.project {
            if index.get(proj).is_none() {
                problems.push(format!("task {} references unknown project `{}`", t.id, proj));
            }
        }
        if t.title.trim().is_empty() {
            problems.push(format!("task {} has an empty title", t.id));
        }
        if let Some(parent) = &t.parent {
            if !tasks.iter().any(|x| &x.id == parent) {
                problems.push(format!("task {} references unknown parent `{}`", t.id, parent));
            }
            if parent == &t.id {
                problems.push(format!("task {} is its own parent", t.id));
            }
        }
        // A loop resolves at every step, so the "unknown parent" check above
        // sees nothing wrong while the tree hangs a row beneath itself. The
        // one-step case is already named above, and naming it twice reads as
        // two faults.
        let mut walk = t.parent.clone().filter(|p| p != &t.id);
        let mut seen: Vec<String> = vec![t.id.clone()];
        while let Some(id) = walk {
            if seen.contains(&id) {
                if id == t.id {
                    problems.push(format!("task {} is in a parent cycle", t.id));
                }
                break;
            }
            seen.push(id.clone());
            walk = tasks.iter().find(|x| x.id == id).and_then(|x| x.parent.clone());
        }
    }

    // One name, two pieces of work. Nothing downstream can tell them apart —
    // `show` answers with the live one while the log, the claim and any
    // `parent` pointing at it describe both.
    let archived = store.archived_ids();
    let claims = store.claims();
    let worked = store.worked();
    for t in &tasks {
        if !archived.contains(&t.id) {
            continue;
        }
        problems.push(format!(
            "task {} has the same id as an archived task — one of them needs renumbering",
            t.id
        ));
        // The blast radius, spelled out. Everything keyed on an id inherits
        // whatever else wore it: a claim made by the archived task now names
        // the live one, and nothing in the record says which it meant.
        let mut carried: Vec<&str> = Vec::new();
        if claims.contains_key(&t.id) {
            carried.push("a claim");
        }
        if worked.contains_key(&t.id) {
            carried.push("a worked record");
        }
        if !carried.is_empty() {
            problems.push(format!(
                "  …and {} still keyed on it, which may belong to either",
                carried.join(" and ")
            ));
        }
    }

    let bindings = store.bindings();
    for (pane, b) in &bindings {
        let id = b.get("task_id").and_then(|x| x.as_str()).unwrap_or("");
        if store.task(id).is_none() {
            problems.push(format!("binding on {pane} points at missing task `{id}`"));
        }
    }

    if herdr::available() {
        match herdr::agents() {
            Ok(agents) => {
                let live: Vec<String> = agents.iter().map(|a| a.pane_id.clone()).collect();
                let stale = bindings.keys().filter(|p| !live.contains(p)).count();
                if stale > 0 {
                    notes.push(format!("{stale} binding(s) on dead panes — `wsp sync` reaps them"));
                }
                notes.push(format!("herdr up, {} agents", agents.len()));
            }
            Err(e) => problems.push(format!("herdr socket present but unreachable: {e}")),
        }
    } else {
        notes.push("herdr socket not found (CLI still works, sidebar tokens will not update)".into());
    }

    if args.json() {
        println!("{}", json!({ "problems": problems, "notes": notes }));
        return if problems.is_empty() { 0 } else { 1 };
    }

    let p = Paint::new();
    for n in &notes {
        println!("{} {}", p.dim("·"), n);
    }
    for prob in &problems {
        println!("{} {}", p.red("✗"), prob);
    }
    if problems.is_empty() {
        println!("{} no problems", p.green("✓"));
        0
    } else {
        1
    }
}

/// Turn live herdr workspaces into tasks the store knows about.
///
/// The old workspaces carry their meaning in a hand-typed label — "Trance
/// Video", "TET -> EIN" — and nowhere else. Closing them without reading them
/// first throws away the only record that the work exists. So: for every
/// workspace with no claim on it, propose a task in whichever project the
/// label or the cwd points at, and claim it there.
///
/// Prints a plan and does nothing unless `--yes`.
pub fn adopt(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let index = Index::new(store.projects());
    let pins = store.pins();
    let claims = store.claims();
    let workspaces = herdr::workspaces().unwrap_or_default();
    let panes = herdr::panes().unwrap_or_default();
    let apply = args.has("yes");

    // A label that is only the folder's name says nothing the cwd does not.
    let uninformative = |label: &str, cwd: &str| -> bool {
        let leaf = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        label.eq_ignore_ascii_case(leaf) || label.is_empty()
    };

    let mut plan: Vec<(String, String, Option<String>, String)> = Vec::new();
    for w in &workspaces {
        let ws_panes: Vec<&herdr::Pane> = panes
            .iter()
            .filter(|p| p.workspace_id == w.id && p.label != crate::panel::PANEL_LABEL)
            .collect();
        let Some(pane) = ws_panes.first() else { continue };
        if claims.values().any(|c| c.get("workspace_id").and_then(|x| x.as_str()) == Some(&w.id)) {
            continue;
        }
        if uninformative(&w.label, &pane.cwd) {
            continue;
        }
        // A workspace deliberately outside the tree is not work to adopt.
        if pins.get(&w.id).map(|p| p == crate::resolve::TOP_LEVEL).unwrap_or(false) {
            continue;
        }
        // Label first here, deliberately: the folder is shared by ten
        // workspaces and the label is the only thing that separates them.
        let project = index
            .project_for_label(&w.label)
            .or_else(|| index.project_for_cwd(&pane.cwd))
            .or_else(|| {
                resolve::resolve(
                    &index,
                    &pins,
                    resolve::Held::default(),
                    Some(&w.id),
                    Some(&w.label),
                    Some(&pane.cwd),
                )
                    .project
            });
        plan.push((w.id.clone(), w.label.clone(), project, pane.pane_id.clone()));
    }

    if plan.is_empty() {
        println!("nothing to adopt — every workspace is claimed or unnamed");
        return 0;
    }

    let p = Paint::new();
    for (ws, label, project, _) in &plan {
        println!(
            "{}  {}  {}",
            p.dim(&util::pad(ws, 4)),
            util::pad(label, 22),
            p.dim(project.as_deref().unwrap_or("(inbox)"))
        );
    }
    if !apply {
        println!("\n{} workspace(s). Re-run with --yes to create the tasks and claim them.", plan.len());
        return 0;
    }

    let mut made = 0;
    for (ws, label, project, pane) in &plan {
        let Ok(id) = store.alloc_task_id() else { continue };
        let mut t = crate::model::Task::new(label, &id);
        t.project = project.clone();
        t.set_status(Status::Doing);
        t.log(&format!("adopted from herdr workspace {ws}"));
        if store.save_task(&t).is_err() {
            continue;
        }
        store.set_claim(
            &t.id,
            json!({
                "workspace_id": ws,
                "workspace_label": label,
                "cwd": panes.iter().find(|p| &p.pane_id == pane).map(|p| p.cwd.clone()).unwrap_or_default(),
                "host": util::hostname(),
                "claimed_at": util::now_iso(),
            }),
        );
        store.set_binding(
            pane,
            json!({
                "task_id": t.id,
                "pane_id": pane,
                "workspace_id": ws,
                "cwd": "",
                "started_at": util::now_iso(),
                "adopted": true,
            }),
        );
        store.log_event("task-adopted", json!({ "id": t.id, "workspace": ws, "label": label }));
        made += 1;
    }
    store.git_commit(&format!("wsp: adopt {made} workspace(s)"));
    println!("adopted {made} workspace(s)");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape that cost the first half hour of this command: a wrapped
    /// payload read flat gives `""`, which looks exactly like a pane with
    /// nothing on it. Both shapes have to work, and neither may be silent.
    #[test]
    fn a_pane_read_is_unwrapped_whichever_shape_it_arrives_in() {
        let wrapped = json!({ "type": "pane_read", "read": { "text": "on the pane" } });
        assert_eq!(read_body(&wrapped).get("text").unwrap(), "on the pane");

        let flat = json!({ "text": "on the pane" });
        assert_eq!(read_body(&flat).get("text").unwrap(), "on the pane");

        // And an empty pane is still an empty pane, not a missing wrapper.
        let empty = json!({ "type": "pane_read", "read": { "text": "" } });
        assert_eq!(read_body(&empty).get("text").unwrap(), "");
    }

    /// The two names a task may not lend a pane. `install` treats any pane
    /// labelled `wsp` as a panel it lost track of, so a task called "wsp"
    /// renaming its own pane would get that agent adopted as furniture and
    /// dropped from the tree — the panel filters its own panes out of it.
    #[test]
    fn a_task_never_lends_a_pane_the_panels_own_name() {
        assert_eq!(task_label("wsp"), None);
        assert_eq!(task_label("  wsp  "), None);
        assert_eq!(task_label("wsp:view"), None);
        assert_eq!(task_label(""), None);

        // Anything else is itself, and a long one is cut to the width `sync`
        // already uses for the same title.
        assert_eq!(task_label("wsp panel"), Some("wsp panel".to_string()));
        let long = "Agents should rename as they pick up new tasks, and say so";
        assert_eq!(task_label(long).unwrap().chars().count(), 44);
        assert!(task_label(long).unwrap().ends_with('…'));
    }

    /// The subtraction that separates a loaded index from an agent halfway
    /// through the commit procedure. Deletions the disk makes too are somebody
    /// editing; deletions only the index makes are work nothing else holds.
    #[test]
    fn only_the_index_deleting_what_the_disk_keeps_counts_as_loss() {
        // The incident: an old tree in the shared index. The disk has the file
        // as HEAD left it, and staging it would drop four lines.
        let staged = "0\t4\tsrc/panel.rs\n";
        assert_eq!(index_loss(staged, ""), 4);

        // An agent that staged what it wrote: both listings say the same
        // thing, because the index holds exactly what is on disk.
        assert_eq!(index_loss("2\t1\tsrc/panel.rs\n", "2\t1\tsrc/panel.rs\n"), 0);

        // …and carried on editing afterwards. The disk has moved further from
        // HEAD than the index has, which is the normal direction of travel.
        assert_eq!(index_loss("2\t1\tsrc/panel.rs\n", "9\t3\tsrc/panel.rs\n"), 0);

        // A deletion made on purpose — the file is gone from the disk as well,
        // so the index is not holding the only copy of anything.
        assert_eq!(index_loss("0\t40\tsrc/old.rs\n", "0\t40\tsrc/old.rs\n"), 0);

        // Per path, not in total: one file legitimately staged does not pay
        // for another the index would revert.
        let staged = "2\t1\tsrc/a.rs\n0\t30\tsrc/b.rs\n";
        let disk = "2\t1\tsrc/a.rs\n";
        assert_eq!(index_loss(staged, disk), 30);

        // A binary file counts no lines at all, and `-` is not a number.
        assert_eq!(index_loss("-\t-\tdoc/screenshot.png\n", ""), 0);

        // A clean index is the common answer and has no listing to read.
        assert_eq!(index_loss("", ""), 0);
    }

    /// The two states an agent with nothing in hand can be in, and the reason
    /// the project is not what the label leads with: a herdr sidebar draws 26
    /// columns and cuts the right-hand end, so a project-first label loses the
    /// state — which is the half a person scanning for a free agent is reading.
    #[test]
    fn a_looking_pane_says_the_state_before_the_project() {
        assert_eq!(looking_label(Some("render"), true), "looking for work in render");
        assert_eq!(looking_label(Some("render"), false), "nothing actionable in render");

        // Unscoped — `wsp next` outside any project. Still the two states.
        assert_eq!(looking_label(None, true), "looking for work");
        assert_eq!(looking_label(None, false), "nothing actionable");

        // What survives herdr's cut is the state, for the longest slug we have.
        let long = looking_label(Some("strata-prototype"), true);
        assert!(long.chars().take(26).collect::<String>().starts_with("looking for work"));

        // And nothing here can ever be a name the panel hunts for.
        for l in [looking_label(Some("wsp"), true), looking_label(Some("wsp"), false)] {
            assert_ne!(l, crate::panel::PANEL_LABEL);
            assert_ne!(l, crate::panel::VIEW_LABEL);
        }
    }

    /// The refusal says what is owed, and the log is the only place that has
    /// ever recorded it. Anything that is not a `blocked:` line written by
    /// `Task::log` is somebody else's note and must not be read as the
    /// question — a claim that answers "blocked, waiting on an answer: claimed
    /// by pane w0:p3" is worse than one that says nothing.
    #[test]
    fn the_refusal_finds_the_question_the_block_left_behind() {
        let mut t = Task::new("Per-project id counters", "t-024");
        t.log("claimed by pane w0:p3");
        assert_eq!(blocked_question(&t), None);

        t.log("blocked: wsp-014 or t-260815-014?");
        assert_eq!(blocked_question(&t), Some("wsp-014 or t-260815-014?".into()));

        // Blocked twice: the second question is the one still waiting.
        t.log("blocked: and what happens to the ids already written?");
        assert_eq!(
            blocked_question(&t),
            Some("and what happens to the ids already written?".into())
        );

        // A line that only mentions one is not one.
        let mut mentions = Task::new("…", "t-025");
        mentions.log("hand-off — t-024 is blocked: waiting on Ed");
        assert_eq!(blocked_question(&mentions), None);

        // And a block with no reason falls back rather than showing nothing.
        let mut bare = Task::new("…", "t-026");
        bare.log("blocked:");
        assert_eq!(blocked_question(&bare), None);
    }
}
