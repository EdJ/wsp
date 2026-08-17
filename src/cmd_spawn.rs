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
//!
//! [`despawn`] is the other end of it, and its order is the reverse: the seat
//! goes first and the claim last, for the reason `place.rs` gives.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::cmd_agent;
use crate::place::{Agent, Order, Place, Refusal, Seat, State};
use crate::place_herdr::Herdr;
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
            "You have been claimed onto {task}. Your brief is already above: the task, \
             what binds it, and what to read. Begin work when you're ready."
        ),
        Handover::Running => format!(
            "You have been claimed onto {task}. Please run `wsp brief --session`, then begin \
             work on the task when you're ready."
        ),
    }
}

/// The order `spawn` places: what to call the seat, where the work lives, and
/// what whatever runs there should know without having to infer it.
///
/// `WSP_PROJECT` and `WSP_TASK` go into the environment, so every pane inside
/// the seat knows what it is for without anyone having to read it off a path.
/// herdr does not persist env across a restart, which is why the durable answer
/// is a claim rather than this — but for the life of the session it is exact,
/// and exactness is what the cwd heuristic lacks.
///
/// A pure function of what `spawn` resolved, so what an agent is handed can be
/// asserted without a backend to hand it to.
fn order(work: &Work, cwd: Option<&str>, on: Option<&str>, show: bool) -> Order {
    // The store first, then what this seat is for — the latter wins if someone
    // has both, which is right: it is more specific.
    let mut env: std::collections::BTreeMap<String, String> = util::store_env()
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_string())))
        .collect();
    if let Some(p) = &work.project {
        env.insert("WSP_PROJECT".into(), p.clone());
    }
    if let Some(t) = &work.task {
        env.insert("WSP_TASK".into(), t.clone());
    }
    Order {
        label: work.label.clone(),
        cwd: cwd.map(|c| c.to_string()),
        env,
        on: on.map(|m| m.to_string()),
        show,
    }
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

/// How long to give the agent to become ready for input, having started.
///
/// A cold Claude Code measured four seconds to readiness on this machine; herdr's
/// own default for the same wait is thirty. What "started" means, what has to be
/// retried to get there and how long any of it takes are the backend's, and live
/// in `place_herdr` — this is only how long the caller is prepared to wait.
const READY_MS: u64 = 30_000;
const POLL_MS: u64 = 150;

/// Wait until the agent in a seat will take a sentence.
///
/// **Not until it is idle**, which is the trap this walked into: herdr reports
/// `agent_status: idle` while an agent is still drawing its banner, and refuses
/// a prompt in that window. `will_take_a_prompt` is the port's single answer to
/// the question every `state == "idle"` caller is actually asking, so this is
/// the one reading and there is nothing left here to get wrong.
///
/// A refusal is not a verdict while there is time left — a backend that did not
/// answer this poll may answer the next — but a seat that has gone empty is: the
/// agent existed when `start` returned, so nothing in it now means it has
/// stopped, and waiting out the deadline would report the wrong failure thirty
/// seconds late.
fn wait_ready(place: &dyn Place, seat: &Seat, kind: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(READY_MS);
    loop {
        match place.state(seat) {
            Ok(s) if s.will_take_a_prompt() => return Ok(()),
            Ok(State::Empty) | Ok(State::Gone) => {
                return Err(format!("{kind} started and then stopped in {seat}"))
            }
            Err(Refusal::NoSeat(_)) => return Err(format!("{seat} is gone")),
            Ok(_) | Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(format!("{kind} started but never became ready for input"));
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
    // No pre-flight question about a socket. `herdr::available()` used to guard
    // this and printed a herdr sentence for a herdr fact; a backend that is not
    // answering now says so from the call that wanted it, which arrives at the
    // same moment and names what it stopped.
    place_work(&Herdr::new(), store, args)
}

fn place_work(place: &dyn Place, store: &Store, args: &Args) -> i32 {
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

    let order = order(&work, cwd.as_deref(), on.as_deref(), !args.has("no-focus"));
    let seat = match place.open(&order) {
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
            // The seat, under the name the claim still calls it. `claim --pane`
            // is `cmd_agent`'s vocabulary and migrating it is its own task; the
            // string is the same string either way.
            let mut flags: Vec<(&str, &str)> = vec![("pane", seat.as_str())];
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
        eprintln!("wsp: opened {seat} — but the claim was refused, so no agent was started");
        return 1;
    }

    let mut started: Option<String> = None;
    let mut told = false;
    if args.has("agent") {
        let kind = args.get("kind").unwrap_or_else(|| DEFAULT_KIND.to_string());
        let name = work.task.clone().or_else(|| work.project.clone()).unwrap_or_default();
        let agent = Agent { kind: kind.clone(), name, args: preamble(&kind, args.has("full")) };
        // Two waits, and they are different questions: `start` comes back when
        // the agent exists, `wait_ready` when it will listen. Whatever a backend
        // has to do to make the first one true — retry a shell that is not ready,
        // clear a half-typed line — happens on its side of the seam now.
        match place.start(&seat, &agent).map_err(|e| e.to_string())
            .and_then(|()| wait_ready(place, &seat, &kind))
        {
            Ok(()) => {
                started = Some(kind.clone());
                // Only a task gives an agent something to be told. A project
                // seat is a place to work, not an instruction, and `f` in the
                // panel is the key that turns one into the other.
                if let Some(t) = &work.task {
                    // `tell` rather than typing into the pane: readiness is
                    // established above, and a backend's own submit does not
                    // depend on a sleep being long enough for a TUI that takes a
                    // burst of keystrokes for a paste.
                    match place.tell(&seat, &claimed_text(t, Handover::Spawned)) {
                        Ok(()) => told = true,
                        Err(e) => eprintln!("wsp: agent started but not told: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("wsp: {kind} did not start in {seat}: {e}"),
        }
    }

    if args.json() {
        println!(
            "{}",
            json!({
                // One handle where there were two. A herdr seat is the pane id
                // this printed as `pane` before, so a caller doing
                // `wsp release --pane $(...)` reads the same string out of a
                // different key; what has gone is `workspace`, which was herdr's
                // second id and is not something the port hands back.
                "seat": seat.as_str(),
                "task": work.task,
                "project": work.project,
                "cwd": cwd,
                "agent": started,
                "told": told,
            })
        );
    } else {
        let what = match &started {
            Some(kind) => format!("{kind} in {seat}"),
            None => format!("a terminal in {seat}"),
        };
        println!("  {}", p.dim(&format!("opened {what}{}", match &cwd {
            Some(c) => format!(" · {}", util::contract(&util::expand(c))),
            None => String::new(),
        })));
        if told {
            println!("  {}", p.dim("told it what it is holding"));
        }
    }
    // An agent asked for and not started is a failure however well the seat
    // went: the caller wanted somebody working, and there is nobody.
    if args.has("agent") && started.is_none() {
        return 1;
    }
    0
}

/// `wsp despawn` — end the agent on a piece of work, and put the work down.
///
/// The other end of [`spawn`], and the reason the port has a `stop` at all: a
/// loop that starts agents one at a time and cannot end one makes despawning
/// part of the loop rather than an edge case. What it replaces is two commands,
/// one of which is not wsp:
///
///     wsp release --pane w26:p1     # drop the claim
///     herdr workspace close w26     # kill the workspace
///
/// **Stop first, release last** — the reverse of that, and the argument for it is
/// where the rule about the claim and the seat already lives, in `place.rs`. This
/// is the half of it that is code: the claim is released only once the seat is
/// gone, a seat that was *already* gone counts as gone, and a backend that did
/// not answer is neither.
///
/// No guard on an agent that is busy, and that is a decision rather than an
/// omission. `claim`'s live-holder guard protects you from a *third party* you
/// may not have known was there; this verb is aimed at a seat by somebody who
/// knows what is in it. What ending it costs is the session, not the work — the
/// files in the tree are untouched — and the state it would refuse on is the one
/// a wedged agent reads as, which is when you most want this.
pub fn despawn(store: &Store, args: &Args) -> i32 {
    end_work(&Herdr::new(), store, args, cmd_agent::my_pane().as_deref())
}

/// Which seat a despawn is about: the one named, or the one holding the task.
///
/// A task resolves through its *binding*, which is the only record that names a
/// seat — a claim names a workspace, and the port cannot turn one into a seat
/// (`place::Seated` does not say where a seat is). So a task whose binding has
/// been lost, which is what a herdr restart leaves behind, is refused with the
/// command that rebuilds it rather than a guess: `wsp reconcile` binds a claim
/// back to a pane by label, and despawn works again afterwards.
fn seat_of(store: &Store, args: &Args, index: &Index) -> Result<(Seat, Option<String>), String> {
    let task_of = |seat: &str| {
        store
            .bindings()
            .get(seat)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .map(String::from)
    };
    if let Some(given) = args.get("pane").or_else(|| args.get("seat")) {
        let task = task_of(&given);
        return Ok((Seat::new(given), task));
    }
    if args.rest.is_empty() {
        return Err("usage: wsp despawn <task> | wsp despawn --pane <seat>".into());
    }
    let work = resolve(store, args, index)?;
    let Some(task) = work.task else {
        return Err("despawn ends the agent on a task — a project is not a seat".into());
    };
    match store.panes_for_task(&task).first() {
        Some(seat) => Ok((Seat::new(seat.clone()), Some(task))),
        None => Err(match store.claims().contains_key(&task) {
            // The claim outlived the pane it was made in, which is what a herdr
            // restart does. Nothing here can say which seat holds it.
            true => format!(
                "{task} is claimed but no seat is bound to it — `wsp reconcile` to bind it again, \
                 or `wsp release --pane <seat>` if the agent is already gone"
            ),
            false => format!("nothing is working {task}"),
        }),
    }
}

/// `me` is which seat this process is standing in, passed in rather than read
/// here: the refusal below is the one behaviour of this verb that depends on the
/// environment, and a test that had to export `HERDR_PANE_ID` to reach it would
/// be changing a process-wide variable every other test can see — which is a
/// flake somebody else's test pays for. It has already happened once in this
/// tree, to `cmd_install`'s lock test, from the first draft of these tests.
fn end_work(place: &dyn Place, store: &Store, args: &Args, me: Option<&str>) -> i32 {
    let p = Paint::new();
    let index = Index::new(store.projects());
    let (seat, task) = match seat_of(store, args, &index) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    // Ending the seat you are standing in kills this process partway through,
    // which is the one case where stopping first cannot work: there would be
    // nobody left to release the claim. Refused rather than reordered, because
    // an agent that wants to put its work down has a verb for that already, and
    // a loop that has resolved its own seat by accident wants to be told.
    if me == Some(seat.as_str()) {
        eprintln!("wsp: {seat} is this pane — `wsp release`, then leave");
        return 2;
    }

    let closed = match place.stop(&seat) {
        Ok(()) => true,
        // Already gone. The first half of the verb is done, however it happened.
        Err(Refusal::NoSeat(_)) => false,
        Err(e) => {
            eprintln!("wsp: {seat} is still standing, so nothing was released: {e}");
            return 1;
        }
    };

    // The claim, through the one implementation of ending one. `release` writes
    // the `worked` record, the line in the task's log and the commit; a second
    // copy of that here would be a second contract.
    let (released, ended) = cmd_agent::release_pane(store, seat.as_str());
    // What the binding said, unless the release found something else there —
    // which it will not, and if it ever does, the release is the later reading.
    let task = ended.or(task);

    if args.json() {
        println!(
            "{}",
            json!({ "seat": seat.as_str(), "closed": closed, "task": task, "released": released })
        );
    } else {
        println!("  {}", p.dim(&match closed {
            true => format!("ended {seat}"),
            false => format!("{seat} was already gone"),
        }));
        match (&task, released) {
            (Some(t), true) => println!("  {}", p.dim(&format!("released {t}"))),
            (Some(t), false) => println!("  {}", p.dim(&format!("{t} was not bound to it"))),
            (None, _) => println!("  {}", p.dim("it was holding nothing")),
        }
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

    /// What a seat is opened with, which is the whole of what `spawn` says to a
    /// backend before anything is running in it.
    ///
    /// `WSP_PROJECT` and `WSP_TASK` are the part that matters and the part
    /// nothing else can supply: herdr does not persist an environment across a
    /// restart, so this is exact for the life of the session and the claim is
    /// what is durable. A seat opened without them leaves every pane inside it
    /// inferring what it is for from a path.
    #[test]
    fn a_seat_is_opened_knowing_what_it_is_for() {
        let work = Work {
            task: Some("t-260817-004".into()),
            project: Some("robustness".into()),
            label: "robustness/004 · a title".into(),
        };
        let o = order(&work, Some("~/claude/wsp"), Some("mb2"), false);
        assert_eq!(o.label, "robustness/004 · a title");
        assert_eq!(o.cwd.as_deref(), Some("~/claude/wsp"), "expanded by the backend, not here");
        assert_eq!(o.on.as_deref(), Some("mb2"));
        assert!(!o.show, "--no-focus is a statement about placing the work");
        assert_eq!(o.env.get("WSP_TASK").map(String::as_str), Some("t-260817-004"));
        assert_eq!(o.env.get("WSP_PROJECT").map(String::as_str), Some("robustness"));

        // A project spawn has no task, and says so by absence rather than by an
        // empty string somebody downstream has to test for.
        let proj = Work { task: None, project: Some("robustness".into()), label: "robustness".into() };
        let o = order(&proj, None, None, true);
        assert!(o.env.get("WSP_TASK").is_none());
        assert!(o.on.is_none());
        assert!(o.show);
    }

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

    /// The work order is ASCII, and it is not a style rule.
    ///
    /// t-260817-004: a spawned agent sat with its work order typed into the
    /// input box and never submitted. The pane had the text and the agent had
    /// nothing to do, so `wsp wip` showed a healthy spawn doing no work — it
    /// fails open, which is the worst way for the loop's start verb to fail.
    /// The one thing that sentence had which every working one before it did
    /// not was an em-dash.
    ///
    /// Asserted over both cases and over the whole string rather than the one
    /// character, because the next well-meant curly quote or ellipsis costs
    /// another unattended spawn to find. Ed's call, 2026-08-17: do not use them.
    #[test]
    fn the_work_order_is_ascii_because_a_spawn_once_hung_on_one_character() {
        for how in [Handover::Spawned, Handover::Running] {
            let text = claimed_text("t-260815-033", how);
            assert!(
                text.is_ascii(),
                "non-ASCII in a work order, which is what t-260817-004 was: {text}"
            );
        }
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

    /// A backend that answers `stop` however the test needs, and nothing else.
    ///
    /// In-process rather than the fake behind a socket, and for the reason the
    /// fake's own docs record: `stop` and the arrange port's close are the same
    /// herdr method, so a socket can see that a pane was taken away and cannot
    /// see which verb meant it. What is under test here is the *order* — which
    /// half ran, and what survived the other half failing — and that is a
    /// question about wsp, not about a wire.
    struct Ends {
        snub: Option<Refusal>,
        asked: std::cell::RefCell<Vec<Seat>>,
    }

    impl Ends {
        fn ok() -> Ends {
            Ends { snub: None, asked: std::cell::RefCell::new(Vec::new()) }
        }
        fn refusing(snub: Refusal) -> Ends {
            Ends { snub: Some(snub), asked: std::cell::RefCell::new(Vec::new()) }
        }
    }

    impl Place for Ends {
        fn stop(&self, seat: &Seat) -> crate::place::Result<()> {
            self.asked.borrow_mut().push(seat.clone());
            match &self.snub {
                Some(r) => Err(r.clone()),
                None => Ok(()),
            }
        }
        // Loudly rather than politely: a despawn that opened a seat or told an
        // agent something would be a defect these tests exist to notice.
        fn open(&self, _: &Order) -> crate::place::Result<Seat> {
            panic!("despawn does not open seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            panic!("despawn does not start agents")
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<()> {
            panic!("despawn does not talk to agents")
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            panic!("despawn does not ask how the work is going")
        }
        fn census(&self) -> crate::place::Result<Vec<crate::place::Seated>> {
            panic!("despawn is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("despawn does not wait for anything")
        }
    }

    /// A task somebody is working: the task, the binding that names the seat, and
    /// the claim the binding stands for.
    fn working(store: &Store, task: &str, seat: &str) {
        let mut t = crate::model::Task::new("stop, and the claim with it", task);
        t.project = Some("robustness".into());
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();
        store.set_binding(seat, json!({ "task_id": task, "pane_id": seat, "workspace_id": "w1" }));
        store.set_claim(
            task,
            json!({ "workspace_id": "w1", "workspace_label": "robustness/095", "cwd": "/tmp" }),
        );
    }

    /// Nothing in these tests may reach a herdr, and one of them would: `release`
    /// re-syncs on its way out, and this machine has a live herdr with real
    /// workspaces in it. A socket path that answers nothing makes every call fail
    /// at once rather than pushing a fixture's metadata onto somebody's pane.
    fn no_backend() -> std::sync::MutexGuard<'static, ()> {
        let lock = util::env_lock();
        std::env::set_var("HERDR_SOCKET_PATH", "/nonexistent/wsp-despawn-tests.sock");
        lock
    }

    /// **The decision this task was opened for.** A seat that will not close
    /// keeps its claim.
    ///
    /// The two failures are not comparable, which is why the order is not a
    /// matter of taste: work that looks unowned while an agent is still standing
    /// in it is handed to a second agent by the next `claim` — that guard reads
    /// bindings, so releasing first blinds it — and two agents in one tree is the
    /// failure this whole store is arranged against. A claim left over a closed
    /// seat is residue `reconcile --reap` already sweeps.
    #[test]
    fn a_seat_that_will_not_close_keeps_its_claim() {
        let _env = no_backend();
        let store = seat("stop-refused");
        working(&store, "t-260816-095", "w1:p1");

        let place = Ends::refusing(Refusal::Backend("pane is not going anywhere".into()));
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), None);

        assert_eq!(code, 1, "a despawn that ended nothing must not report success");
        assert_eq!(place.asked.borrow().len(), 1, "it did try");
        assert!(store.claims().contains_key("t-260816-095"), "the claim went with the agent still there");
        assert!(store.bindings().contains_key("w1:p1"), "and so did the binding");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// The other half of the direction's question: ending a seat ends the
    /// **claim**, not only the binding.
    ///
    /// A pane exiting is an accident of process lifetime and leaves the intent
    /// standing; this is a decision, so it ends the way `release` does and leaves
    /// the same record — a `worked` row saying who had it and for how long, and a
    /// line in the task's log. The status stays `doing`, because work with nobody
    /// on it is a true and useful state.
    #[test]
    fn ending_a_seat_ends_the_claim_and_leaves_the_record_a_release_leaves() {
        let _env = no_backend();
        let store = seat("stop-ok");
        working(&store, "t-260816-095", "w1:p1");

        let place = Ends::ok();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), None);

        assert_eq!(code, 0);
        assert_eq!(place.asked.borrow().as_slice(), &[Seat::new("w1:p1")], "the bound seat");
        assert!(!store.claims().contains_key("t-260816-095"), "the claim outlived the seat");
        assert!(!store.bindings().contains_key("w1:p1"));
        assert!(store.worked().contains_key("t-260816-095"), "no trace of who had it");
        let t = store.task("t-260816-095").expect("the task");
        assert_eq!(t.status_raw, "doing", "work nobody is on is still work");
        assert!(t.body.contains("released"), "the log should say it was put down:\n{}", t.body);

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// A seat that had already gone still ends the claim, and this is the case
    /// the verb is most needed for.
    ///
    /// An agent whose backend crashed under it leaves exactly this: no seat, and
    /// a claim. Treating `NoSeat` as a failure would leave the one command that
    /// ends a claim unable to end the claims that most need ending — and the
    /// residue would be swept by a reaper instead, which is a person noticing
    /// later rather than a verb doing what it was asked.
    #[test]
    fn a_seat_that_was_already_gone_still_ends_the_claim() {
        let _env = no_backend();
        let store = seat("stop-gone");
        working(&store, "t-260816-095", "w1:p1");

        let place = Ends::refusing(Refusal::NoSeat(Seat::new("w1:p1")));
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), None);

        assert_eq!(code, 0);
        assert!(!store.claims().contains_key("t-260816-095"));

        // A backend that did not answer is not the same sentence, and must not
        // release: silence is not evidence that the seat is gone.
        working(&store, "t-260816-094", "w1:p2");
        let quiet = Ends::refusing(Refusal::Unreachable("no socket".into()));
        assert_eq!(end_work(&quiet, &store, &Args::synth("despawn", &["094"], &[]), None), 1);
        assert!(store.claims().contains_key("t-260816-094"), "released on a backend's silence");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// Which seat a despawn is about, and the two ways there is not one.
    ///
    /// A claim names a workspace and a binding names a seat, so a task whose
    /// binding was lost — a herdr restart, and the claim is the half that
    /// survives — cannot be resolved to a seat by anything in the port. Said
    /// with the command that rebuilds the binding, rather than guessed at.
    #[test]
    fn a_task_whose_seat_is_unknown_is_told_what_would_find_it() {
        let _env = no_backend();
        let store = seat("stop-unbound");
        let index = Index::new(store.projects());
        working(&store, "t-260816-095", "w1:p1");

        let of = |a: &Args| seat_of(&store, a, &index);
        assert_eq!(
            of(&Args::synth("despawn", &["095"], &[])).unwrap(),
            (Seat::new("w1:p1"), Some("t-260816-095".into()))
        );
        // Named directly, the seat is whatever was said — including one no
        // binding knows about, which is how a stray agent is ended.
        assert_eq!(
            of(&Args::synth("despawn", &[], &[("pane", "w9:p9")])).unwrap(),
            (Seat::new("w9:p9"), None)
        );

        store.clear_binding("w1:p1");
        let err = of(&Args::synth("despawn", &["095"], &[])).unwrap_err();
        assert!(err.contains("wsp reconcile"), "the way back is not named: {err}");

        store.clear_claim("t-260816-095");
        let err = of(&Args::synth("despawn", &["095"], &[])).unwrap_err();
        assert!(err.contains("nothing is working"), "{err}");

        let err = of(&Args::synth("despawn", &[], &[])).unwrap_err();
        assert!(err.contains("usage"), "{err}");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// An agent cannot despawn itself, and the refusal is the ordering decision
    /// showing its edge.
    ///
    /// Stopping first means the process dies partway through, before the claim is
    /// released — so the one seat this verb must not touch is the one it is
    /// running in. `wsp release` is the verb for putting your own work down, and
    /// saying so is more use than saying no.
    ///
    /// Which seat this is arrives as an argument, so this asserts the behaviour
    /// without exporting `HERDR_PANE_ID` for every other test in the process to
    /// trip over; see [`end_work`].
    #[test]
    fn a_despawn_will_not_end_the_seat_it_is_running_in() {
        let _env = no_backend();
        let store = seat("stop-self");
        working(&store, "t-260816-095", "w1:p1");

        let place = Ends::ok();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), Some("w1:p1"));

        assert_eq!(code, 2);
        assert!(place.asked.borrow().is_empty(), "it asked the backend to end this pane");
        assert!(store.claims().contains_key("t-260816-095"), "and it dropped its own claim");

        let _ = std::fs::remove_dir_all(&store.root);
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
