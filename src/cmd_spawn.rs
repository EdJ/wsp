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
//! Code session runs `wsp brief --session` from its `SessionStart` hook and
//! reads the claim on the way in. Started first, it would open knowing nothing
//! and the sentence would be the only thing it ever heard about the work.
//!
//! That order is also what lets the sentence stop asking for a brief: see
//! [`Handover`], and what the agent is *not* handed is [`TRIM`].

use std::time::{Duration, Instant};

use serde_json::json;

use crate::cmd_agent;
use crate::herdr;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// How an agent came by the work it is being told about — and it is the one
/// thing that changes what the work order should say.
///
/// [`Handover::Spawned`] is `spawn`'s case. The order is workspace, claim,
/// agent, sentence, so by the time this is said the agent's `SessionStart` hook
/// has already run `wsp brief --session` *with the claim in place*: the task,
/// what binds it, and what to read are sitting at the top of its context. A
/// sentence asking it to fetch that again costs a round-trip, and a round-trip
/// at request 1 is a full context re-read — measured at ~35K on
/// t-260816-096, against ~700 for the duplicated text itself.
///
/// [`Handover::Running`] is the panel's. That agent's session began before the
/// claim existed, so its brief is a brief about holding nothing. It has to
/// fetch, and one `--session` call is the whole payload in one round-trip
/// rather than the dozen `wsp show` calls it would otherwise make.
///
/// The duplication is therefore disposed of by construction rather than by
/// remembering — the caller that knows the hook has run is the caller that
/// stops asking.
#[derive(Clone, Copy)]
pub enum Handover {
    Spawned,
    Running,
}

/// What an agent is told about work it has just been handed.
///
/// One definition, two cases: the panel says this to an agent it claims a task
/// onto, and `spawn` says it to the agent it just started. Two wordings written
/// in two places would be two contracts.
pub fn claimed_text(task: &str, how: Handover) -> String {
    match how {
        Handover::Spawned => format!(
            "You have been claimed onto {task}. Your brief is already above — the task, \
             what binds it, and what to read. Begin work when you're ready."
        ),
        Handover::Running => format!(
            "You have been claimed onto {task}. Please run `wsp brief --session`, then begin \
             work on the task when you're ready."
        ),
    }
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
    machine: Option<&str>,
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
    // The one herdr call with nothing in it to route on: a workspace that does
    // not exist yet has no id to say where it should be. So the machine is
    // named, and this is the only place in `spawn` where it has to be —
    // everything after this is addressed by the pane id below, which comes back
    // already qualified and routes itself.
    let r = herdr::call_on(machine, "workspace.create", params, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    let id = |outer: &str, inner: &str| -> Result<String, String> {
        let bare = r
            .get(outer)
            .and_then(|w| w.get(inner))
            .and_then(|x| x.as_str())
            .ok_or_else(|| format!("workspace.create returned no {inner}"))?;
        // Qualified here, at the door, like everything else arriving from a far
        // herdr — which has never heard of `@mb2` and answers with its own bare
        // id. An id that went into a claim unqualified would name *this*
        // machine's workspace, and the claim would be about a workspace on the
        // wrong box for as long as it lived.
        Ok(match machine {
            Some(m) => format!("{bare}@{m}"),
            None => bare.to_string(),
        })
    };
    Ok((id("workspace", "workspace_id")?, id("root_pane", "pane_id")?))
}

/// Where a spawn is going: this machine unless `--on` says otherwise.
///
/// Asked, never inferred. There is no scheduler and no load model here on
/// purpose — auto-placement hides the thing you most want to see — and the
/// default is this machine so every existing caller and script keeps exactly
/// the behaviour it had.
///
/// A machine that is not in the store is a typo, and worth saying so rather
/// than letting it become a socket error about a path nobody typed. A machine
/// that is in the store but not answering is a different sentence, and carries
/// what the daemon last saw, because "why can I not spawn on mb2" is answered
/// by that line and nothing else.
fn placement(store: &Store, args: &Args) -> Result<Option<String>, String> {
    let Some(name) = args.get("on") else { return Ok(None) };
    let Some(m) = store.machine(&name) else {
        let known: Vec<String> = store.machines().into_iter().map(|m| m.name).collect();
        return Err(match known.is_empty() {
            true => format!("no machine `{name}` — this seat has none. wsp machine add <name> <ssh-target>"),
            false => format!("no machine `{name}` — there is {}", known.join(", ")),
        });
    };
    if !m.is_active() {
        return Err(format!("`{name}` is retired — wsp machine set {name} status=active"));
    }
    match store.machine_live(&name) {
        Some(l) if l.reachable => Ok(Some(m.name)),
        Some(l) if !l.error.is_empty() => Err(format!("`{name}` is not answering — {}", l.error)),
        Some(_) => Err(format!("`{name}` is not answering yet")),
        None => Err(format!(
            "`{name}` has no tunnel — is `wsp daemon` running? Nothing has reported on it"
        )),
    }
}

/// herdr's default when nobody says which agent. Every other kind it knows is
/// spelt the way its own CLI spells it and passed straight through — an
/// unknown one is refused by herdr with the whole catalogue in the message,
/// which is a better list than one kept here and left to go stale.
const DEFAULT_KIND: &str = "claude";

/// What a spawned Claude Code session is *not* given, and why each name is on
/// the list.
///
/// Every request re-reads the whole context, so a token present before the
/// agent has done anything is paid once per request — ~102 times in the session
/// t-260816-096 measured. The preamble is the largest single thing in that
/// context and almost none of it is wsp's: `wsp brief --session` is ~3,300
/// tokens of it, the rest is Claude Code's own.
///
/// **Measured against a live spawn on 2026-08-17, not estimated.** Two
/// `wsp spawn --agent` runs into a sandbox — its own herdr session, its own
/// store — on this task, one with `--full` and one without, read back off the
/// two transcripts:
///
/// | | first-request context |
/// |---|---|
/// | `--full` | 37,756 |
/// | trimmed | 25,306 |
///
/// **12,450 tokens off every request of the session — 33%.** At the ~102
/// requests that session ran, ~1.27M tokens.
///
/// Which is *worth saying plainly*: the parent task's arithmetic hoped for
/// ~28.6K, on the reading that the ~34K preamble was mostly sheddable. It is
/// not. What can be shed through a flag is the tool schemas and the MCP prose;
/// the system prompt underneath them is ~25K and there is no lever here for it.
/// This is 44% of the hoped saving and the rest is not available.
///
/// Attribution, one lever at a time, by running `claude -p` with the flag and
/// summing `input + cache_creation + cache_read` on the reply — the print-mode
/// baseline is 26,816 and each figure is what removing that one thing saved:
///
/// | dropped | tokens | why it is safe to drop |
/// |---|---|---|
/// | `Workflow` | 6,024 | the work order forbids it in as many words |
/// | `Agent` | 2,682 | same, and this task exists because sub-agents are what blew the budget |
/// | every MCP server, and its instruction prose | 831 | two spawned agents made zero MCP calls between them |
///
/// The 831 is the print-mode floor for it: an interactive session also carries
/// `claude-in-chrome`, Gmail, Calendar and Drive, whose instruction blocks are
/// prose rather than schema, which is most of the gap between the 9,537 those
/// three lines account for in print and the 12,450 measured live.
///
/// **Named removals rather than a named allowlist, and the difference is not
/// cosmetic.** `--tools` takes an allowlist of built-ins, and the first attempt
/// at this used one: `Bash,Read,Edit,Write,Glob,Grep,TodoWrite,Task`. Four of
/// those eight do not exist in this build, and unknown names are ignored in
/// silence — so it measured something other than what it said, and quietly
/// withheld several tools that do exist and were never considered. That is
/// precisely the failure this trim is supposed to avoid: an agent that silently
/// lacks a tool does not report it, it works around it, expensively and out of
/// sight. An allowlist withholds every tool nobody thought of, including every
/// one Claude Code gains after this line was written. A denylist withholds only
/// what it names, and what it names is here to read.
///
/// The same rule says what is deliberately *kept*, and it was priced rather
/// than assumed, so nobody has to re-measure to argue with it: `ScheduleWakeup`
/// 1,250, `ReportFindings` 609, `ListAgents` 306. Each is small, each is
/// reachable from a slash command a person might type into this pane, and 2,165
/// tokens is not worth a capability going missing without a sentence about it.
/// `Read`, `Edit`, `Write` and `Bash` are not negotiable: the same measurement
/// found those agents doing all their reading through `sed` and `head` at ~28K,
/// and a trim that pushes more work into Bash has made things worse while
/// appearing to make them better. Both probe agents above reached for `Read`
/// on their first tool call, which is the thing to watch if this list ever
/// grows.
///
/// What the trim leaves behind is legible, which was the other requirement: a
/// trimmed session asked what it has answers "No Workflow tool", "No mcp__
/// prefixed tools", and lists its fourteen skills. It does not silently
/// improvise around an absence it cannot see.
const TRIM: &[&str] = &["--strict-mcp-config", "--disallowedTools", "Agent", "Workflow"];

/// The arguments an agent of this kind is started with.
///
/// Keyed on the kind because [`TRIM`] is Claude Code's spelling and nothing
/// else's. `codex --strict-mcp-config` does not start, and a kind that does not
/// start leaves a workspace with a shell in it and no agent — a worse outcome
/// than the tokens are worth. Every other kind herdr knows is passed through
/// untrimmed, exactly as before.
///
/// `--full` is the way back. A trim is a capability change, so there has to be
/// one, and it has to be a flag rather than an edit: the agent that needs the
/// design MCP server to draw an artefact is a real spawn on this backlog, not a
/// hypothetical.
fn preamble(kind: &str, full: bool) -> Vec<String> {
    match (full, kind) {
        (false, "claude") => TRIM.iter().map(|s| (*s).to_string()).collect(),
        _ => Vec::new(),
    }
}

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
/// `args` is what herdr appends to the command line it types, and is omitted
/// from the call entirely when there is nothing to say — so an untrimmed spawn
/// puts exactly the bytes on the socket that it always did.
fn launch(pane: &str, kind: &str, name: &str, args: &[String]) -> Result<(), String> {
    let mut params = json!({ "pane_id": pane, "kind": kind, "name": name });
    if !args.is_empty() {
        params["args"] = json!(args);
    }
    let deadline = Instant::now() + Duration::from_millis(SHELL_MS);
    loop {
        match herdr::call("agent.start", params.clone()) {
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
fn start_agent(pane: &str, kind: &str, name: &str, args: &[String]) -> Result<(), String> {
    launch(pane, kind, name, args)?;
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
            launch(pane, kind, name, args)?;
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

    let on = match placement(store, args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    // Still this machine's paths, deliberately. A project root is a path in the
    // store and `~` expands here, which is right while the machines mirror each
    // other and is exactly what the Linux box breaks; host-qualified roots are
    // t-260815-060 and are not smuggled in here.
    let cwd = args
        .get("cwd")
        .or_else(|| work.project.as_deref().and_then(|p| index.root_of(p)));

    let (ws, pane) = match open_workspace(
        &work.label,
        cwd.as_deref(),
        work.project.as_deref(),
        work.task.as_deref(),
        !args.has("no-focus"),
        on.as_deref(),
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
        let trim = preamble(&kind, args.has("full"));
        match start_agent(&pane, &kind, &name, &trim) {
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
                        json!({ "target": pane, "text": claimed_text(t, Handover::Spawned) }),
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

    fn seat(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-place-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    /// Every way `--on` can be wrong, and the sentence each one earns.
    ///
    /// They are four different problems — a typo, a machine you retired, a
    /// machine that is down, and a daemon that is not running — and the fix for
    /// each is different, so one "cannot spawn on mb2" for all four would be
    /// the least useful thing this could say. The unreachable case carries what
    /// the daemon last saw, because that line is the whole answer to "why can I
    /// not spawn on mb2".
    #[test]
    fn saying_where_is_checked_before_anything_is_opened() {
        use crate::model::Machine;
        use crate::store::MachineLive;
        let store = seat("errs");
        let on = |v: &str| Args::synth("spawn", &["t-1"], &[("on", v)]);

        assert!(placement(&store, &Args::synth("spawn", &["t-1"], &[])).unwrap().is_none(),
            "no flag is this machine, which is what every existing caller passes");

        let err = placement(&store, &on("mb2")).unwrap_err();
        assert!(err.contains("this seat has none"), "{err}");

        store.save_machine(&Machine::new("mb2", "mb2")).unwrap();
        let err = placement(&store, &on("mb3")).unwrap_err();
        assert!(err.contains("there is mb2"), "a typo is told what there was: {err}");

        let err = placement(&store, &on("mb2")).unwrap_err();
        assert!(err.contains("is `wsp daemon` running"), "nothing has reported: {err}");

        store.set_machine_live("mb2", &MachineLive {
            reachable: false,
            error: "ssh: no route to host".into(),
            ..Default::default()
        });
        let err = placement(&store, &on("mb2")).unwrap_err();
        assert!(err.contains("no route to host"), "what the daemon saw: {err}");

        store.set_machine_live("mb2", &MachineLive { reachable: true, ..Default::default() });
        assert_eq!(placement(&store, &on("mb2")).unwrap().as_deref(), Some("mb2"));

        let mut retired = store.machine("mb2").unwrap();
        retired.status = "retired".into();
        store.save_machine(&retired).unwrap();
        let err = placement(&store, &on("mb2")).unwrap_err();
        assert!(err.contains("retired"), "{err}");

        let _ = std::fs::remove_dir_all(&store.root);
    }
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

    /// The sentence an agent is handed work with has one definition and two
    /// cases, and the case is decided by whether the hook has already run with
    /// this claim in place.
    ///
    /// A spawned agent's has: `spawn` claims before it starts the agent, so the
    /// payload is at the top of its context and asking for it again is a wasted
    /// round-trip — which at request 1 is a full context re-read, ~35K on the
    /// measurement that prompted this, against ~700 for the duplicated text.
    /// An agent the panel hands work to has been running since before the claim
    /// existed, so it genuinely has to fetch, and `--session` makes that one
    /// call rather than a dozen `wsp show`s.
    #[test]
    fn only_the_agent_whose_hook_missed_the_claim_is_asked_to_fetch() {
        let spawned = claimed_text("t-260815-033", Handover::Spawned);
        assert!(spawned.contains("t-260815-033"));
        assert!(!spawned.contains("wsp brief"), "the hook has already injected it: {spawned}");

        let running = claimed_text("t-260815-033", Handover::Running);
        assert!(running.contains("t-260815-033"));
        assert!(running.contains("wsp brief --session"), "the whole payload in one call: {running}");
    }

    /// The trim names what it takes away, and the names are the ones the work
    /// order already forbids.
    ///
    /// Asserted as names rather than as a count, because the point of a
    /// denylist is that it is legible: a test that only checked the length
    /// would pass on a list that had quietly become something else. `Read`,
    /// `Edit`, `Write` and `Bash` appearing here would be the bad change —
    /// the measurement that prompted this found agents doing all their reading
    /// through `sed` at ~28K, and a trim that pushes work into Bash costs more
    /// than it saves.
    #[test]
    fn a_spawned_claude_is_not_given_the_two_tools_it_is_told_not_to_use() {
        let trim = preamble("claude", false);
        assert!(trim.contains(&"--strict-mcp-config".to_string()), "{trim:?}");
        assert!(trim.contains(&"--disallowedTools".to_string()), "{trim:?}");
        assert!(trim.contains(&"Agent".to_string()), "sub-agents are what blew the budget: {trim:?}");
        assert!(trim.contains(&"Workflow".to_string()), "6,024 tokens of tool nobody may call: {trim:?}");
        for kept in ["Bash", "Read", "Edit", "Write"] {
            assert!(!trim.contains(&kept.to_string()), "{kept} is how the work gets done: {trim:?}");
        }
    }

    /// Two ways back to the whole preamble, and both are needed.
    ///
    /// `--full` is the person's: a trim is a capability change, and the agent
    /// that needs the design MCP server to draw an artefact is a spawn on this
    /// backlog rather than a hypothesis. The kind is the machine's: these are
    /// Claude Code's flag spellings, and handing them to `codex` or `gemini`
    /// buys a workspace with a shell in it and no agent.
    #[test]
    fn the_trim_is_claude_codes_alone_and_one_flag_undoes_it() {
        assert!(preamble("claude", true).is_empty(), "--full is the way back");
        assert!(preamble("codex", false).is_empty(), "not codex's spelling");
        assert!(preamble("gemini", false).is_empty());
        assert!(preamble("codex", true).is_empty());
    }
}
