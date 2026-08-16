//! `wsp spawn` — put a terminal, or an agent, on a piece of work.
//!
//! The panel could always open a workspace for a task and claim it there, and
//! then it stopped: somebody still had to walk over to the new pane and type
//! `claude`. And the whole gesture existed only as a key, so an agent could not
//! hand work to a new agent and neither could a script.
//!
//! One verb does both halves, and the panel's `O` and `S` run it rather than
//! keeping a second copy — the rule [`crate::panel::Effect::Run`] already
//! follows for every other command the panel issues.
//!
//! The order matters and is the whole design: **workspace, claim, agent,
//! sentence**. The claim has to land before the agent starts, because a Claude
//! Code session runs `wsp brief` from its `SessionStart` hook and reads the
//! claim on the way in. Started first, it would open knowing nothing and the
//! sentence would be the only thing it ever heard about the work.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::cmd_agent;
use crate::herdr;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// What an agent is told about work it has just been handed.
///
/// `wsp brief` rather than the id alone: the brief is what a session gets on
/// the way in, so an agent handed work mid-session lands in the same place it
/// would have started from — its project, its claim, the decisions that bind
/// it, and who else is in the tree. A fresh agent has had it from the hook
/// already; asking again costs a second and covers the machine where the hook
/// was never installed.
///
/// One sentence, defined once: the panel says this to an agent it claims a task
/// onto, and `spawn` says it to the agent it just started. Two wordings would
/// be two contracts.
pub fn claimed_text(task: &str) -> String {
    format!("You have been claimed onto {task}. Run `wsp brief`, then work it.")
}

/// Open a workspace for a piece of work, rooted where that work lives.
///
/// `WSP_PROJECT` and `WSP_TASK` go into the workspace environment, so every
/// pane inside it knows what it is for without anyone having to infer it from
/// a path. herdr does not persist env across a restart, which is why the
/// durable answer is a claim rather than this — but for the life of the
/// session it is exact, and exactness is what the cwd heuristic lacks.
///
/// Returns the workspace and the pane it opened with — the latter is what
/// `claim` needs, since a claim speaks in panes and knows nothing about
/// workspaces.
pub fn open_workspace(
    label: &str,
    cwd: Option<&str>,
    project: Option<&str>,
    task: Option<&str>,
    focus: bool,
) -> Result<(String, String), String> {
    // The store first, then what this workspace is for — the latter wins if
    // someone has both, which is right: it is more specific.
    let mut env = util::store_env();
    if let Some(p) = project {
        env.insert("WSP_PROJECT".into(), json!(p));
    }
    if let Some(t) = task {
        env.insert("WSP_TASK".into(), json!(t));
    }
    let mut params = json!({ "label": label, "env": env, "focus": focus });
    if let Some(c) = cwd {
        params["cwd"] = json!(util::expand(c).display().to_string());
    }
    let r = herdr::call("workspace.create", params).map_err(|e| e.to_string())?;
    let ws = r
        .get("workspace")
        .and_then(|w| w.get("workspace_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "workspace.create returned no id".to_string())?;
    let pane = r
        .get("root_pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "workspace.create returned no pane".to_string())?;
    Ok((ws, pane))
}

/// herdr's default when nobody says which agent. Every other kind it knows is
/// spelt the way its own CLI spells it and passed straight through — an
/// unknown one is refused by herdr with the whole catalogue in the message,
/// which is a better list than one kept here and left to go stale.
const DEFAULT_KIND: &str = "claude";

/// How long to give the new workspace's shell before deciding it is never
/// coming, and how long to give the agent after it. A cold Claude Code measured
/// four seconds to readiness on this machine; herdr's own default for the same
/// wait is thirty.
const SHELL_MS: u64 = 5_000;
const READY_MS: u64 = 30_000;
/// If nothing has appeared in the pane by here, the command that was typed did
/// not survive being typed.
const RETYPE_MS: u64 = 6_000;
const POLL_MS: u64 = 150;

/// The agent herdr sees in this pane, if it sees one.
fn agent_of(pane: &str) -> Option<serde_json::Value> {
    herdr::call("agent.get", json!({ "target": pane })).ok()?.get("agent").cloned()
}

/// Whether the agent in this pane will accept a prompt yet.
///
/// **Not** whether it is idle, which is the trap this walked into. herdr reports
/// `agent_status: idle` while the agent is still starting — `launch_pending` is
/// true and `interactive_ready` is absent — and `agent.prompt` refuses in that
/// window with `agent_not_ready`. Waiting for `idle` therefore returned in half
/// a second, every time, and the work order went into a pane that was still
/// drawing its banner. `interactive_ready` is the field that means what `idle`
/// looks like it means.
fn ready(pane: &str) -> bool {
    agent_of(pane)
        .and_then(|a| a.get("interactive_ready").and_then(|r| r.as_bool()))
        .unwrap_or(false)
}

/// Type the agent's name at the pane's shell prompt.
///
/// Retried while herdr says the pane has no shell to type at. `agent.start`
/// refuses a pane whose shell has not finished starting — `agent_pane_busy`,
/// "not an available shell" — and the pane here is a workspace's root pane,
/// measured at ten milliseconds old. So the first attempt lands inside that
/// window as a matter of course, and only a refusal that is not that one is a
/// real failure.
fn launch(pane: &str, kind: &str, name: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(SHELL_MS);
    loop {
        match herdr::call("agent.start", json!({ "pane_id": pane, "kind": kind, "name": name })) {
            Ok(_) => return Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("agent_pane_busy") || Instant::now() >= deadline {
                    return Err(msg);
                }
                std::thread::sleep(Duration::from_millis(POLL_MS));
            }
        }
    }
}

/// Start an agent in a pane and wait until it will take a sentence.
///
/// Typed twice if nothing appears at all. herdr types the agent's name at the
/// prompt and a shell that is not quite ready eats the front of it — the
/// observed failure was ` mclaude`, `command not found`, and a minute of
/// waiting for an agent that was never going to exist. `ctrl-u` clears whatever
/// landed on the line before typing again.
///
/// It retypes only while herdr can see no agent in the pane at all. A slow
/// start is the other reason for the wait to run on, and typing `claude` at a
/// Claude Code that is merely still booting would leave the word sitting in its
/// input box for somebody to find later.
fn start_agent(pane: &str, kind: &str, name: &str) -> Result<(), String> {
    launch(pane, kind, name)?;
    let began = Instant::now();
    let mut retyped = false;
    loop {
        if ready(pane) {
            return Ok(());
        }
        let waited = began.elapsed();
        if waited >= Duration::from_millis(READY_MS) {
            return Err(match agent_of(pane) {
                Some(_) => format!("{kind} started but never became ready for input"),
                None => format!("nothing that looks like {kind} appeared in {pane}"),
            });
        }
        if !retyped && waited >= Duration::from_millis(RETYPE_MS) && agent_of(pane).is_none() {
            retyped = true;
            let _ = herdr::call("pane.send_text", json!({ "pane_id": pane, "text": "\x15" }));
            launch(pane, kind, name)?;
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// What `spawn` resolved its argument to.
struct Work {
    task: Option<String>,
    project: Option<String>,
    /// The workspace's opening name. A claim renames it after the task a
    /// moment later — to the same thing, for a task, so the window does not
    /// change its name under whoever was already looking at it. A project
    /// keeps this one.
    label: String,
}

/// A task, or a project, or nothing that resolves.
///
/// `-p` forces the project reading. Without it a task is tried first and a
/// project second, which is the order the ids themselves suggest: `t-260815-033`
/// can only be a task, and a project slug can only be a project, so the two
/// collide solely on a title substring — where the task is what was meant, that
/// being the thing you were just reading.
fn resolve(store: &Store, args: &Args, index: &Index) -> Result<Work, String> {
    if let Some(p) = args.get("project") {
        let proj = index.find(&p).ok_or_else(|| format!("no project matching `{p}`"))?;
        return Ok(Work {
            task: None,
            project: Some(proj.id.clone()),
            label: proj.name.clone(),
        });
    }
    let needle = args
        .rest
        .first()
        .cloned()
        .ok_or_else(|| "usage: wsp spawn <task|-p project> [--agent]".to_string())?;
    if let Some(t) = store.find_task(&needle) {
        return Ok(Work {
            project: t.project.clone(),
            label: cmd_agent::task_label(&t).unwrap_or_else(|| t.title.clone()),
            task: Some(t.id),
        });
    }
    match index.find(&needle) {
        Some(proj) => Ok(Work {
            task: None,
            project: Some(proj.id.clone()),
            label: proj.name.clone(),
        }),
        None => Err(format!("no task or project matching `{needle}`")),
    }
}

pub fn spawn(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let p = Paint::new();
    let index = Index::new(store.projects());
    let work = match resolve(store, args, &index) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    let cwd = args
        .get("cwd")
        .or_else(|| work.project.as_deref().and_then(|p| index.root_of(p)));

    let (ws, pane) = match open_workspace(
        &work.label,
        cwd.as_deref(),
        work.project.as_deref(),
        work.task.as_deref(),
        !args.has("no-focus"),
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };

    // The claim, through the one implementation of it. It refuses on work that
    // is done, work that is blocked, and work a live agent is holding, and each
    // refusal is a reason not to start an agent here — a spawn onto a blocked
    // task is precisely what that guard exists to prevent. The workspace is
    // left standing either way: it is a terminal in the right tree, which is
    // what you would have opened by hand, and closing something that already
    // has a shell in it to tidy up after a refusal is a worse trade.
    let claimed = match &work.task {
        Some(t) => {
            let mut flags: Vec<(&str, &str)> = vec![("pane", pane.as_str())];
            if args.has("force") {
                flags.push(("force", "true"));
            }
            if args.json() {
                flags.push(("json", "true"));
            }
            cmd_agent_claim(store, t, &flags) == 0
        }
        None => false,
    };
    if work.task.is_some() && !claimed {
        eprintln!("wsp: opened {ws} — but the claim was refused, so no agent was started");
        return 1;
    }

    let mut started: Option<String> = None;
    let mut told = false;
    if args.has("agent") {
        let kind = args.get("kind").unwrap_or_else(|| DEFAULT_KIND.to_string());
        let name = work.task.clone().or_else(|| work.project.clone()).unwrap_or_default();
        match start_agent(&pane, &kind, &name) {
            Ok(()) => {
                started = Some(kind.clone());
                // Only a task gives an agent something to be told. A project
                // workspace is a place to work, not an instruction, and `f` in
                // the panel is the key that turns one into the other.
                if let Some(t) = &work.task {
                    // `agent.prompt` rather than typing into the pane: the
                    // start above has already established the agent is ready
                    // for input, and this is herdr's own submit — it does not
                    // depend on a sleep being long enough for a TUI that takes
                    // a burst of keystrokes for a paste.
                    match herdr::call_for(
                        "agent.prompt",
                        json!({ "target": pane, "text": claimed_text(t) }),
                        Duration::from_secs(10),
                    ) {
                        Ok(_) => told = true,
                        Err(e) => eprintln!("wsp: agent started but not told: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("wsp: {kind} did not start in {pane}: {e}"),
        }
    }

    if args.json() {
        println!(
            "{}",
            json!({
                "workspace": ws,
                "pane": pane,
                "task": work.task,
                "project": work.project,
                "cwd": cwd,
                "agent": started,
                "told": told,
            })
        );
    } else {
        let what = match &started {
            Some(kind) => format!("{kind} in {ws}"),
            None => format!("a terminal in {ws}"),
        };
        println!("  {}", p.dim(&format!("opened {what}{}", match &cwd {
            Some(c) => format!(" · {}", util::contract(&util::expand(c))),
            None => String::new(),
        })));
        if told {
            println!("  {}", p.dim("told it what it is holding"));
        }
    }
    // An agent asked for and not started is a failure however well the
    // workspace went: the caller wanted somebody working, and there is nobody.
    if args.has("agent") && started.is_none() {
        return 1;
    }
    0
}

/// `wsp claim <task> --pane <pane>`, called rather than shelled out to.
fn cmd_agent_claim(store: &Store, task: &str, flags: &[(&str, &str)]) -> i32 {
    crate::cmd_agent::claim(store, &Args::synth("claim", &[task], flags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    /// The two sub-projects the backlog is split into have no checkout of
    /// their own — `wsp/render` and `wsp/data` are two halves of one tree. A
    /// spawn that read only a project's own `roots` put the agent wherever the
    /// caller happened to be standing, which for a panel is wherever it was
    /// installed.
    #[test]
    fn a_project_with_no_root_of_its_own_inherits_one() {
        let mut parent = Project::new("wsp");
        parent.roots = vec!["~/claude/wsp".into()];
        let mut child = Project::new("render");
        child.parent = Some("wsp".into());
        let mut orphan = Project::new("tooling");
        orphan.parent = Some("nowhere".into());

        let index = Index::new(vec![parent, child, orphan]);
        assert_eq!(index.root_of("wsp").as_deref(), Some("~/claude/wsp"));
        assert_eq!(index.root_of("render").as_deref(), Some("~/claude/wsp"));
        assert_eq!(index.root_of("tooling"), None, "a missing parent ends the walk");
        assert_eq!(index.root_of("nothing-here"), None);
    }

    /// A cycle in `parent` is a store anyone can write by hand, and `doctor`
    /// reports it rather than the walk hanging on it.
    #[test]
    fn a_parent_cycle_does_not_spin() {
        let mut a = Project::new("a");
        a.parent = Some("b".into());
        let mut b = Project::new("b");
        b.parent = Some("a".into());
        assert_eq!(Index::new(vec![a, b]).root_of("a"), None);
    }

    /// The sentence an agent is handed work with has one definition. The panel
    /// says it to an agent it claims a task onto and `spawn` says it to the
    /// agent it just started, and an agent that heard a different sentence
    /// depending on which door it came through is two contracts.
    #[test]
    fn the_work_order_names_the_task_and_the_brief() {
        let s = claimed_text("t-260815-033");
        assert!(s.contains("t-260815-033"));
        assert!(s.contains("wsp brief"), "the brief is how it finds everything else");
    }
}
