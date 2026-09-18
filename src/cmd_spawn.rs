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
//! It is also what lets `spawn` compose a brief for a kind that has no hook to
//! run one: by the time the sentence is built, the claim is in the store and
//! the seat is open, so the brief a session would have read can be built and
//! handed over instead. That is [`Route`], and it is why the sentence never
//! asks for a brief. *What* the agent is started with, and how the sentence
//! reaches it, are facts about the agent rather than about placing work, and
//! live in [`crate::agent_commands`].
//!
//! [`despawn`] is the other end of it, and its order is the reverse: the seat
//! goes first and the claim last, for the reason `place.rs` gives.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::agent_commands;
use crate::cmd_agent;
use crate::cmd_checkout::{self, Tree};
use crate::cmd_govern;
use crate::place::{Agent, Order, Place, Refusal, Seat, State};
use crate::place_herdr::Herdr;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Clock, Paint};
use crate::Args;

/// How an agent came by the work it is being told about — and it is the one
/// thing that changes what the work order should say.
///
/// [`Handover::Spawned`] is `spawn`'s case, and it says *your brief is already
/// above* — which is a claim about what is in the agent's context and is made
/// true two different ways. For a kind with a session hook, by the order
/// workspace, claim, agent, sentence: the hook has run `wsp brief --session`
/// *with the claim in place* before this is said. For a kind without one, by
/// [`Route::Inline`], which puts that same brief in front of this sentence. A
/// sentence asking the agent to fetch it instead costs a round-trip, and a
/// round-trip at request 1 is a full context re-read — measured at ~35K on
/// robustness-031, against ~700 for the duplicated text itself.
///
/// [`Handover::Running`] is the panel's. That agent's session began before the
/// claim existed, so its brief is a brief about holding nothing. It has to
/// fetch, and one `--session` call is the whole payload in one round-trip
/// rather than the dozen `wsp show` calls it would otherwise make. It is also
/// [`Route::Fetch`]'s wording, for the one kind of spawn wsp still cannot put a
/// brief in front of.
///
/// The duplication is therefore disposed of by construction rather than by
/// remembering — the caller that knows the brief has landed is the caller that
/// stops asking.
///
/// [`Handover::Custodian`] is the third, and it is a different *job* rather
/// than a different route to the same one. By the decision of 2026-08-17 on
/// robustness-048 an agent can be assigned to a **project**, which is an edge the
/// model did not have — every other assignment in wsp is agent-to-task — and
/// what arrives in that slot is not a claimant with a piece of work to finish.
/// It sequences, directs, reviews and holds the record for everything beneath
/// it, and stands down when a person says so rather than when something is
/// done. This reverses the line that used to sit at the one call site below:
/// *"only a task gives an agent something to be told; a project workspace is a
/// place to work, not an instruction"*. Under a slot, a project workspace **is**
/// an instruction, and this is it.
///
/// It carries no fetch/no-fetch pair, because a custodian is spawned into its
/// slot before its agent starts — [`crate::cmd_brief`] draws the custodial
/// brief off the same record, whether a hook reads it or `spawn` composes it.
/// A *running* agent handed a slot would be the fourth case and would have to
/// fetch; nothing in wsp does that yet, and inventing the sentence for it here
/// would be inventing the flow.
#[derive(Clone, Copy)]
pub enum Handover {
    Spawned,
    Running,
    Custodian,
}

/// How an agent comes to know what it is holding — which is a fact about the
/// kind, not about the work, and is what decides whether the sentence below is
/// true when it is said.
///
/// [`Route::Hook`] is Claude Code. `spawn`'s order is workspace, claim, agent,
/// sentence, so the `SessionStart` hook has already run `wsp brief --session`
/// with the claim in place and the brief is at the top of the agent's context
/// before it reads a word of the order. Nothing to write and nothing to ask.
///
/// [`Route::Inline`] is `core-032`, and it is what a kind with no session hook
/// gets instead: wsp writes the brief to a file and the kind's own
/// configuration names it, so the runtime loads it as system context before the
/// first message. Nothing is fetched, because the agent never goes and gets it;
/// nothing is asked, because there is no `bash` call and so `core-020` d2's
/// brake is never met. See [`crate::agent_commands::Kind::brief_file`] for the
/// driving, and [`crate::cmd_brief::At`] for why the brief cannot simply be the
/// live one.
///
/// **It is not [`crate::agent_commands::Kind::order_in_args`], and the day
/// those two were the same predicate cost a revert.** The brief went into a
/// `--prompt` argv element; herdr refuses `agent.start` args holding any
/// control character, a brief is multi-line, and three spawns of three were
/// refused with `invalid_agent_argument` and no agent started. Env is not argv.
///
/// [`Route::Fetch`] is what is left: a kind with neither a hook nor a config
/// file wsp can write into. `codex` and `gemini` are told to run `wsp brief
/// --session`, which is a round-trip and — under a brake — a stall, and it is
/// the honest answer until one of them is driven the way `core-026` drove
/// opencode. It is also where a brief that could not be *written* lands, which
/// is the one failure this must not paper over: opencode ignores an
/// `instructions` file that is not there, in silence, so an unwritten brief
/// with the confident sentence on top of it would be exactly the `core-027`
/// failure this row exists to remove, arriving by a new door.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    Hook,
    Inline,
    Fetch,
}

/// Whether the brief this seat needed is where the agent will find it.
///
/// [`Laid::Elsewhere`] is not a failure: it is every kind that does not read a
/// brief from a file, and every spawn with nothing to be briefed about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Laid {
    Written,
    Failed,
    Elsewhere,
}

/// The kind's answer and the brief's, read as one question.
fn route(how: &dyn agent_commands::Kind, laid: Laid) -> Route {
    match (how.briefed(), how.brief_file(), laid) {
        (true, _, _) => Route::Hook,
        // Written, so the sentence is true.
        (false, true, Laid::Written) => Route::Inline,
        // A kind that reads a brief from a file, and no file. Say the true
        // thing, which is the old thing.
        (false, true, _) => Route::Fetch,
        (false, false, _) => Route::Fetch,
    }
}

/// Where a seat's brief is written.
///
/// Beside the machine state rather than in the working tree, which is
/// `core-020` d2's correction applied to a second kind of file: `cmd_checkout::dirty`
/// asks git with `--untracked-files=all`, so anything wsp leaves in a checkout
/// makes that tree permanently dirty — refused by `wsp checkout --rm`, skipped
/// by `--sweep`, kept by `despawn` as holding uncommitted work. It is also why
/// this is not simply `AGENTS.md`, which is the other way opencode would read
/// it.
///
/// Named for the subject, so a spawn onto the same task twice overwrites its
/// own file rather than accumulating, and so [`despawn`] can find it again with
/// nothing recorded anywhere.
fn brief_path(store: &Store, subject: &str) -> std::path::PathBuf {
    store.state.join("briefs").join(format!("{subject}.md"))
}

/// Where this seat may reach outside the tree it stands in, from what wsp
/// already holds.
///
/// **`core-042`, and the row it answers is *a permission prompt nobody can
/// answer is an agent that is lost*.** A spawned opencode stands in
/// `.worktrees/<task>` and opencode's `external_directory` boundary is anything
/// outside that directory — so the moment a row's work is somewhere else, the
/// agent stops on a question and holds its seat until a person walks past. The
/// answer is not to widen the boundary; it is to name the few places wsp
/// already knows the work is, and leave everything else asking.
///
/// **Two sources, and neither is guessed.**
///
/// The **store** is where task files live, and reading one is the most ordinary
/// thing an agent does: `wsp show` prints the id, the agent reaches for
/// `~/wsp/tasks/<id>.md`, and that is the single prompt `core-041`'s replay of
/// `ui-007` had left. `render-033` asked for it too. It is granted whole rather
/// than as `tasks/` alone, on `core-027` d2's rule about the brake that stops
/// the brief: allowing one directory of it moves the same stall one command
/// later, to a project handbook or a worklist. What the allow costs is bounded
/// by what a `bash` that is already `allow` can do to the same files anyway —
/// this widens the file tools to match the shell, and the four verbs that
/// destroy records stay denied above both.
///
/// The task's **`refs`** is the frontmatter list of paths, and it is exactly
/// the mechanism for *this row names a file outside the tree* — a spec, a
/// design note, a sibling repository. A ref that resolves inside the agent's
/// own tree needs nothing and is dropped; one that does not names the directory
/// it sits in. So a row that has to reach somewhere says where, once, on the
/// row, and every agent that runs it reaches there without asking. That is a
/// thing a person writes down rather than a thing wsp infers, which is the
/// point: the list of outside paths is per-row and finite, and the alternative
/// is a standing grant that covers every row for ever.
///
/// **What is deliberately absent is the project's `roots`**, and the argument
/// is on [`crate::agent_commands::OpenCode`]'s config: a worktree already holds
/// every tracked file, the three agents that asked for the parent were each
/// reaching for something their own tree had, and an `external_directory` allow
/// is a path rule rather than an operation rule — so granting it would hand a
/// spawned agent write access to the checkout somebody else is standing in.
///
/// The directory rather than the file, because that is opencode's own unit: it
/// asks about `<dir>/*` and never about a single path. Deduplicated, and a
/// directory under one already named is dropped, since opencode's `*` spans
/// separators and the parent already answers for it.
///
/// Nothing above the tree survives, whatever named it — see the filter below,
/// where the reason is written: a rule on any ancestor of `.worktrees/<task>`
/// covers every sibling tree, which is every other agent's uncommitted work.
pub(crate) fn reach(store: &Store, task: Option<&str>, tree: Option<&str>) -> Vec<std::path::PathBuf> {
    let tree = tree.map(util::expand);
    let mut dirs: Vec<std::path::PathBuf> = vec![store.root.clone()];
    // A ref is written the way a person types a path — `~/claude/strata-spec.md`,
    // or one relative to the tree the work is done in. Both are resolved here,
    // because the rule opencode matches is an absolute one and a `~` in it
    // would match nothing at all.
    let refs = task.and_then(|t| store.task(t)).map(|t| t.refs).unwrap_or_default();
    for r in refs {
        let at = match r.starts_with('~') || r.starts_with('/') {
            true => util::expand(&r),
            false => match &tree {
                Some(t) => t.join(&r),
                // Nowhere to resolve it against is not a licence to guess a
                // root: a relative ref with no tree names nothing wsp can
                // prove, and a wrong absolute path here is a standing allow on
                // somebody else's directory.
                None => continue,
            },
        };
        // The directory it sits in, and the ref itself when it is one already.
        let dir = match at.is_dir() {
            true => at,
            false => match at.parent() {
                Some(d) => d.to_path_buf(),
                None => continue,
            },
        };
        dirs.push(dir);
    }
    // The tree and a reach must be **disjoint**, and both halves of that matter.
    //
    // Inside the tree is not outside it: a rule about the agent's own directory
    // is noise in a policy whose entire subject is leaving it. And an *ancestor*
    // of the tree is the one thing that may never be granted however it was
    // asked for — a worktree lives at `<checkout>/.worktrees/<task>`, so a rule
    // on any directory above it covers every sibling tree, which is to say
    // every other agent's uncommitted work, and `<checkout>` itself is the
    // shared tree somebody is usually standing in. That is refused here rather
    // than trusted to nobody writing it down, because a ref is prose and this
    // is the one mistake in it that cannot be taken back.
    if let Some(t) = &tree {
        dirs.retain(|d| !d.starts_with(t) && !t.starts_with(d));
    }
    dirs.sort();
    dirs.dedup();
    // Shortest first, so the parent is always seen before anything under it.
    let mut kept: Vec<std::path::PathBuf> = Vec::new();
    for d in dirs {
        if !kept.iter().any(|k| d.starts_with(k)) {
            kept.push(d);
        }
    }
    kept
}

/// Compose this seat's brief and write it where the agent will read it.
///
/// The claim has landed by the time this is called, which is the whole reason
/// it is called here and not before the seat opened: a brief composed a moment
/// earlier is a brief about an empty seat.
///
/// A write that fails is reported and answered with [`Laid::Failed`], never
/// swallowed. The agent would start perfectly well — opencode ignores a missing
/// `instructions` file without a word — and would be told its brief was above
/// it when nothing was, which is the failure `core-027` found by driving and
/// this row was filed to remove. Rotation composes its successor's brief
/// through this same one writer: a second copy of the write would be a second
/// answer to where a seat's brief lives, and [`despawn`] looks in exactly one
/// place to take it away again.
fn lay_brief(
    store: &Store,
    work: &Work,
    seat: &Seat,
    cwd: Option<&str>,
    path: &std::path::Path,
) -> Laid {
    let ws = workspace_of(seat);
    let text = crate::cmd_brief::session_text(&crate::cmd_brief::Briefing::at(
        store,
        crate::cmd_brief::At {
            project: work.project.as_deref(),
            pane: Some(seat.as_str()),
            workspace: ws.as_deref(),
            cwd,
        },
    ));
    let wrote = path
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|()| std::fs::write(path, format!("{text}\n")));
    match wrote {
        Ok(()) => Laid::Written,
        Err(e) => {
            eprintln!("wsp: could not write the brief to {}: {e}", util::contract(path));
            eprintln!("wsp: the work order will ask for it instead");
            Laid::Failed
        }
    }
}

/// What an agent is told about work it has just been handed.
///
/// One definition, three cases: the panel says this to an agent it claims a
/// task onto, `spawn` says it to the agent it just started, and `spawn
/// --govern` says the custodial one to an agent it has just put in a project's
/// slot. Wordings written in three places would be three contracts.
///
/// `subject` is a task for the first two and a **project** for the third, which
/// is the whole of what the new edge comes to.
pub fn work_order(subject: &str, how: Handover) -> String {
    match how {
        Handover::Spawned => format!(
            "You have been claimed onto {subject}. Your brief is already above: the task, \
             what binds it, and what to read. Begin work when you're ready."
        ),
        Handover::Running => format!(
            "You have been claimed onto {subject}. Please run `wsp brief --session`, then begin \
             work on the task when you're ready."
        ),
        // The four things the seat did on the night this was written from, in
        // the order they were done, and the one thing it must not become. No
        // task is named because there is not one: what is above this sentence
        // is the project's brief — what is open beneath it and who is standing
        // in it — and picking one of those up itself is the failure mode, not
        // the job.
        //
        // The last sentence is `core-049`, and it is a change of policy rather
        // than of wording: the seat used to be the one thread that ran all
        // night, and its context grew with every verdict, flag receipt and
        // `wip` poll it was handed — re-billed on every request it made, since
        // a token in context is paid for by every later request. The store
        // holds everything a successor needs — the run's position behind
        // `worklist next`, raised hands behind `flag --seat`, decisions behind
        // `project show` — so the thread does not have to hold anything at all.
        // Rotation per barrier keeps each custodian's window small by
        // construction instead of by discipline.
        //
        // Since `core-050` rotation is one verb, and the order names it rather
        // than composing it out of three instructions — the third of which,
        // ending your own session, was the one that could be skipped. The verb
        // carries its own failure mode in the sentence: nothing ends on a
        // promise, so a handover that cannot confirm the successor says so and
        // leaves the caller holding the seat.
        Handover::Custodian => format!(
            "You are the custodian of the {subject} project. You have not been claimed onto a \
             task and you should not claim one. Your brief is already above: what {subject} is \
             for, what is open beneath it, and who else is standing in it. The job is to \
             sequence what runs next and what waits, to write the direction an arriving agent \
             needs and no more, to review finished work against the code rather than against \
             the agent's report, and to hold the record: decisions, corrections, and what must \
             not close with the task that found it. `wsp flag --seat` is your inbox and `wsp \
             spawn` puts agents under you. You coordinate rather than authorise, so nothing \
             waits on your permission. Say what you are doing with `wsp say`, and begin by \
             reading what is open. Keep direction in task logs and decisions rather than in \
             this conversation, so nothing lives only here; and when you pass a worklist \
             barrier, rotate: write the verdict with `wsp worklist go`, then make `wsp govern \
             {subject} --rotate` your last act. It seats your successor, waits until its first \
             turn starts, moves the seat, and arranges your ending; if any of that fails it \
             says so, exits non-zero, and you are still the seat."
        ),
    }
}

/// The sentence an agent is handed, and it is one sentence per job rather than
/// one per kind.
///
/// Pure, and it takes the [`Route`] rather than the kind, for the reason
/// [`order`] is pure about backends: what an agent is told is the thing worth
/// asserting, and asserting it should not need a store, a seat or a file on
/// disk.
///
/// [`Route::Hook`] and [`Route::Inline`] say the same thing, which is the point
/// of the carrier rather than a coincidence: both mean *the brief is already in
/// your context*, and the only difference is who put it there.
fn handover(subject: &str, how: Handover, route: Route) -> String {
    match (route, how) {
        (Route::Hook | Route::Inline, _) => work_order(subject, how),
        // The custodial order says what the seat is for and assumes the brief
        // beneath it, so a custodian that has to fetch needs the same
        // instruction a claimant does. Prepended rather than woven in: the
        // custodial sentence is a job description and this is one instruction
        // before it, which is the fourth case `Handover`'s docs said would have
        // to fetch.
        (Route::Fetch, Handover::Custodian) => format!(
            "Please run `wsp brief --session` first — your context is empty and \
             nothing has been read to you. {}",
            work_order(subject, Handover::Custodian)
        ),
        // A claimant with no brief in its context is exactly what
        // `Handover::Running` is the wording for. An existing sentence rather
        // than a new one.
        (Route::Fetch, _) => work_order(subject, Handover::Running),
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
/// It also *strips*, which is the same job and was the missing half of it: a
/// spawned agent is a new session and must inherit none of the spawning
/// session's identity. [`crate::place::shed_env`] says which names and why, and why an
/// empty value is the only strip there is to make. Nothing else in wsp is
/// positioned to do it — this is the one function that decides what a seat's
/// occupant finds — and it fails silently when it is not done: the agent runs
/// fine and saves no transcript.
///
/// A pure function of what `spawn` resolved, so what an agent is handed can be
/// asserted without a backend to hand it to.
fn order(
    work: &Work,
    cwd: Option<&str>,
    on: Option<&str>,
    show: bool,
    agent: Option<Occupant<'_>>,
    custodian: bool,
) -> Order {
    Order {
        label: work.label.clone(),
        cwd: cwd.map(|c| c.to_string()),
        env: seat_env(
            agent,
            work.project.as_deref(),
            work.task.as_deref(),
            custodian,
        ),
        on: on.map(|m| m.to_string()),
        show,
    }
}

/// What is about to run in a seat, when what is about to run in it is an agent.
///
/// It is a parameter rather than something [`seat_env`] works out for itself
/// because `spawn` opens seats with no agent in them at all — `wsp spawn` on a
/// bare project is a terminal in the right tree — and configuring a runtime
/// nobody is launching would be a variable in a shell somebody else is using.
///
/// `brief` is where this seat's brief will be written, for a kind that reads
/// one out of a file: see [`crate::agent_commands::Kind::brief_file`] and
/// [`brief_path`]. `None` for every other kind, for a seat with nothing to be
/// briefed about, and for a resume — [`crate::cmd_resume`] carries the argument
/// for the last of those, which was driven rather than assumed.
pub(crate) struct Occupant<'a> {
    pub kind: &'a str,
    pub brief: Option<&'a std::path::Path>,
    /// Where this agent may reach outside its own tree, from [`reach`].
    ///
    /// A field the caller must fill in rather than something worked out here,
    /// for `core-038`'s reason applied a second time: it is a function of the
    /// store, the task and the tree, and `resume` opens seats knowing all
    /// three. A default computed in this file would be the same two builders
    /// that have to agree, one of which would quietly hand a resumed agent a
    /// narrower world than the spawn it is continuing.
    pub reach: &'a [std::path::PathBuf],
}

/// Everything the occupant of a seat wsp opens finds in its environment: what
/// every seat gets, and what this kind's runtime needs on top.
///
/// Here rather than inlined above because `wsp resume` opens seats too, and a
/// resumed agent that inherited the caller's session identity — or missed the
/// store it is supposed to be reading — would be a second, quieter copy of
/// every failure [`crate::place::shed`] was written for.
///
/// **The kind's half is inside this function and not composed onto it at the
/// call site, and that is `core-038`.** It was composed at the call site for a
/// month, in `spawn` and nowhere else, so `resume` opened seats with the
/// `WSP_*` half and none of the runtime's — and for opencode the runtime's half
/// is `core-020` d1's permission policy. Driven 2026-08-22 in a sandbox herdr: a
/// spawned opencode stopped on `git status` and asked; the same agent, resumed
/// into the same session, ran it without asking. `core-040` has since put
/// `git status` on the read list, so re-drive that with `cargo build` — the
/// divergence is the finding, not the verb. The agent was doing the same
/// work under a different policy and nothing said so. Two builders that have to
/// agree is the defect; one builder that cannot be called without answering
/// this question is the fix, which is why `agent` is a parameter a caller must
/// say `None` to rather than a field it can leave off.
///
/// It goes on the **seat** rather than on the agent's command line, and why is
/// on [`crate::agent_commands::Kind::env`], which is also where the names in it
/// live: a module about placing work should not learn one runtime's spelling.
///
/// `custodian` is stated by the caller rather than inferred from `task` being
/// `None`, for the same reason [`Occupant`] is: a bare project workspace is a
/// seat with no agent in it and no job at all, and only the caller knows which
/// kind of nothing it opened. What it buys is `WSP_TERSE=1` — a coordinating
/// agent re-reads `brief` and `wip` several times an hour for a whole run, so
/// the two blocks `--terse` drops are paid for by every one of those readings
/// (`core-049`). The session payload is unaffected: `Depth` puts `--session`
/// above `--terse` precisely so this variable cannot strip it.
pub(crate) fn seat_env(
    agent: Option<Occupant<'_>>,
    project: Option<&str>,
    task: Option<&str>,
    custodian: bool,
) -> BTreeMap<String, String> {
    // Shed first: everything below is something this seat is *for*, and none of
    // it collides with a name the caller's Claude Code set.
    let mut env = crate::place::shed_env();
    // The store next, then what this seat is for — the latter wins if someone
    // has both, which is right: it is more specific.
    env.extend(
        util::store_env().into_iter().filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_string()))),
    );
    if let Some(p) = project {
        env.insert("WSP_PROJECT".into(), p.to_string());
    }
    if let Some(t) = task {
        env.insert("WSP_TASK".into(), t.to_string());
    }
    if custodian {
        env.insert("WSP_TERSE".into(), "1".into());
    }
    // The kind's own, last, so a runtime that needs configuring gets it and
    // every seat that does not is byte-for-byte what it was.
    if let Some(a) = agent {
        env.extend(crate::agent_commands::of(a.kind).env(a.brief, a.reach));
    }
    env
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

/// Which tier the agent is to be started at, checked before anything is opened.
///
/// **Stated when somebody states it, and otherwise inferred from where the work
/// sits — out loud.** `--model` and `--effort` are the whole vocabulary, and one
/// word of it means the spawner is stating the tier; a spawn that says neither,
/// onto work with a filled seat above it, starts at [`GOVERNED_MODEL`] and
/// [`GOVERNED_EFFORT`] and prints that it did. Ed, 2026-08-25, `wsp-058` d6.
///
/// That is inside d1's line rather than across it, and the line is not
/// [`placement`]'s: **placement stays asked-never-inferred, tier is
/// inferred-but-printed-and-overridable**, which is what that decision put on
/// each of them. Nor does it unpark the router d4 stood down — nothing per-task
/// is estimated here, no field was added and the task's prose is not read. The
/// one thing inferred is a structural fact the store already holds, and
/// [`governed`] carries the rest of the argument for it.
///
/// [`agent_commands::Kind::tier`] holds why the words are checked at all, and
/// [`agent_commands::EFFORTS`] why `--effort` is the one to reach for first.
///
/// What is decided *here* is only where and whether:
///
/// - **Before `place.open`.** A tier caught after the workspace exists has
///   already cost a workspace, a claim and a worktree, and now needs a
///   `wsp despawn` before it can be retyped. Nothing above this line has
///   written anything down.
/// - **Refused without `--agent`, not dropped.** `--model haiku` on a bare
///   workspace is a sentence about an agent that is never started, and a flag
///   that does nothing in silence is the failure the checking is for.
/// - **Refused when nobody will be there to unblock it.** A tier
///   [`agent_commands::Kind::unattended`] names cannot be walked away from, and
///   the default spawn is a background one — so the flag is refused unless
///   `--focus` says you are going to the pane. That check is second because it
///   is about this *spawn* and the one above it is about the words; a typo
///   should learn it is a typo before it learns anything else.
///
/// `--on <machine>` is orthogonal and stays that way: the flag states the tier,
/// and that machine's `claude` has its own version and its own settings.
fn tier(
    args: &Args,
    kind: &str,
    agent: bool,
    scope: &Scope,
) -> Result<(Option<String>, Option<String>), String> {
    let model = args.get("model");
    let effort = args.get("effort");
    // The trigger for the default is this same expression and not a second one
    // beside it: neither word stated is exactly the case that had nothing to
    // check, and it is now the case that has something to decide.
    if model.is_none() && effort.is_none() {
        return Ok(governed(args, kind, agent, scope));
    }
    if !agent {
        return Err("--model and --effort say how to start an agent — add --agent".into());
    }
    let kind = agent_commands::of(kind);
    kind.tier(model.as_deref(), effort.as_deref())?;
    if !args.has("focus") {
        if let Some(why) = kind.unattended(model.as_deref()) {
            return Err(why);
        }
    }
    Ok((model, effort))
}

/// The tier a spawn onto governed work starts at, when nobody said otherwise.
///
/// Sonnet because d5 *measured* it: `wsp-061` ran an unattended sonnet agent
/// through the whole harness on render-076 — claimed, stayed in its own tree,
/// read commit-help and took the right branch of it, verified, installed, swept
/// nothing of anybody else's — so the capability question was answered before
/// this default was written. It is also the tier
/// [`agent_commands::Kind::unattended`] has nothing to say about, which is the
/// same measurement from the other end: haiku panes open in manual mode and a
/// default that opened one would be a fleet of agents stopped at their first
/// permission prompt.
const GOVERNED_MODEL: &str = "sonnet";

/// Medium because effort is the cheaper knob — the same capability class for
/// less spend, and no failure mode a model change would not add worse. d1's
/// second consequence, applied to the flat default rather than to a router.
const GOVERNED_EFFORT: &str = "medium";

/// Where a piece of work sits, for the one question [`governed`] asks of it.
///
/// The four arguments [`cmd_govern::seat_for`] takes, carried in rather than
/// looked up. `place_work` has a store and has read all of this by the time it
/// calls [`tier`]; reaching for one *here* would cost the property that makes
/// every refusal above checkable — a function you can put four literals into
/// and read an answer out of, with no store, no seat and no herdr.
struct Scope<'a> {
    /// `governors/`, as [`crate::store::Store::governors`] reads it.
    governors: &'a BTreeMap<String, Value>,
    /// The project tree, for the ancestor half of the walk.
    index: &'a Index,
    /// The **running** worklist this work is a member of, which is the front of
    /// the walk and `None` in the ordinary state where nothing is running. See
    /// [`cmd_govern::seat_for`], which is where that ordering is argued.
    list: Option<&'a str>,
    /// The work's project, or `None` for work that has none.
    project: Option<&'a str>,
}

/// The default: work with a seat above it starts cheap, and is told so.
///
/// **A flat default at the spawn, not the per-task router `wsp-058` d4
/// parked.** No complexity estimate, no new field on a task, no reading of the
/// prose. The sole inference is whether the work has a filled seat above it,
/// which is [`cmd_govern::seat_for`]'s existing walk over the running list, the
/// project and its ancestors — the same walk a raised hand takes, so a spawn
/// routes down exactly where a hand would route up.
///
/// **The printing is the load-bearing half.** A default that swapped the tier
/// in silence would be the `--effort` warning failure that
/// [`agent_commands::Kind::tier`] was written against — a session that ran at
/// one tier and was recorded at another — and the whole licence d1 gave was for
/// a tier that is *inferred, printed and overridable*. So the sentence is
/// printed from inside this function rather than by the caller: there is no way
/// to take the inference without it.
///
/// Four things it does not fire on, and each is a decision rather than a guard.
///
/// - **Nothing to start.** A bare `wsp spawn <task>` opens a terminal and
///   claims the work; no agent runs, so there is no tier, and a tier recorded
///   against a claim nobody started would be a record of something that did not
///   happen.
/// - **`--govern`.** A seat sequences, reviews and writes the notes, and d5's
///   measured residue of a cheap tier was precisely that work — no review note,
///   and the change description left in an overview that is then injected into
///   every later spawn on the task. The seat is the worst place in the fleet to
///   route down: workers cheap, seats on the settings tier.
/// - **A kind with no vocabulary for these words.** `sonnet` is Claude Code's
///   spelling and [`agent_commands::Plain`] passes no tier on at all, so a
///   default sent to one of those kinds would be wsp saying it started `codex`
///   on sonnet and starting it on whatever codex defaults to. It declines
///   rather than refuses — a tier nobody typed may never be the reason a spawn
///   fails, and the way to say sonnet to a kind that spells it differently is
///   still to type it.
/// - **Ungoverned work.** No seat anywhere above it is the ordinary state and
///   reads as today's behaviour exactly: nothing added to the command line,
///   nothing written to the claim, nothing printed.
///
/// The trigger is **where the work sits and not who typed the spawn**, which is
/// the wider of the two readings and was chosen knowing what it widens: a spawn
/// Ed types at the panel onto `wsp` or `compound` defaults to sonnet too,
/// because governed territory is fleet work whoever opened it and `--model` is
/// there for the exception.
fn governed(args: &Args, kind: &str, agent: bool, scope: &Scope) -> (Option<String>, Option<String>) {
    let unstated = (None, None);
    if !agent || args.has("govern") {
        return unstated;
    }
    if agent_commands::of(kind).tier(Some(GOVERNED_MODEL), Some(GOVERNED_EFFORT)).is_err() {
        return unstated;
    }
    let Some(seat) = cmd_govern::seat_for(scope.governors, scope.index, scope.list, scope.project)
    else {
        return unstated;
    };
    // On stderr, and not through `Paint`: the `--json` form of this command
    // prints an object on stdout that a caller parses, and a dim line in front
    // of it would break the one reader that cannot skip it. This is the same
    // channel `spawn` already says "no tree of its own for …" on, and for the
    // same reason — it is news about what was done, not a failure.
    eprintln!(
        "wsp: starting at {GOVERNED_MODEL}, {GOVERNED_EFFORT} effort — \
         {} has a seat above this work. --model or --effort states your own",
        seat.scope
    );
    (Some(GOVERNED_MODEL.to_string()), Some(GOVERNED_EFFORT.to_string()))
}

/// herdr's default when nobody says which agent. Every other kind it knows is
/// spelt the way its own CLI spells it and passed straight through — an
/// unknown one is refused by herdr with the whole catalogue in the message,
/// which is a better list than one kept here and left to go stale.
pub(crate) const DEFAULT_KIND: &str = "claude";

/// The kind a record names, or [`DEFAULT_KIND`] for one that names none.
///
/// **`core-031`'s second decision, and it is a decision rather than a
/// convenience.** A record with no kind can be read two ways — as `claude`,
/// which is compatible and wrong for exactly the agents `core-020` is adding,
/// or as *unknown*, which is honest and costs more than it sounds.
///
/// Unknown resolves to [`crate::agent_commands::Plain`], and `Plain` has no
/// resume flag: `wsp resume` on every record written before this existed would
/// stop working, and the ones written before this existed are all `claude`
/// because `--kind` was read and thrown away. So honesty about the absence buys
/// nothing true and loses the whole back catalogue.
///
/// What makes the default safe rather than merely convenient is that an absence
/// no longer lasts: [`crate::cmd_agent::learn_sessions`] and
/// [`crate::cmd_govern::learn_seats`] take the kind off herdr's own reading on
/// every `sync` tick, so a live record is corrected within a tick of anything
/// looking at it and only a record whose agent is already gone can still be
/// reading its default. For those, the default is what they were.
///
/// One function because there are three call sites and `cmd_resume`'s census
/// path already had this line written out; the store path now reads the same
/// fact the same way.
pub(crate) fn kind_or_default(recorded: &str) -> String {
    match recorded.trim() {
        "" => DEFAULT_KIND.to_string(),
        k => k.to_string(),
    }
}

/// How long the caller is prepared to wait, in three numbers and a clock.
///
/// A struct rather than three constants for the reason `place_herdr::Herdr`
/// gives about its own: every one of these was set by a failure, and a test that
/// has to sit through two real seconds to check what happens after two seconds
/// is a test nobody runs. [`Patience::default`] is what the CLI uses.
struct Patience<'a> {
    /// How long to give the agent to become ready for input, having started.
    ///
    /// A cold Claude Code measured four seconds to readiness on this machine;
    /// herdr's own default for the same wait is thirty. What "started" means,
    /// what has to be retried to get there and how long any of it takes are the
    /// backend's, and live in `place_herdr` — this is only how long the caller
    /// is prepared to wait.
    ready: Duration,
    /// How long a turn has to start, once the work order has been sent.
    ///
    /// **Measured against a live herdr 0.8.0 and a live Claude Code on
    /// 2026-08-18**, three runs of the loop below in a workspace of its own: the
    /// prompt was accepted and left sitting in the composer every time, and a
    /// return pressed at 5.1s started the turn in **0.15s**. So the number this
    /// has to be bigger than is not how long an agent takes to answer, it is how
    /// long Claude Code takes to *draw the composer that can be submitted into*
    /// — pressed at 2.0s, before the TUI had replaced the shell on screen, the
    /// return was swallowed exactly as the first one had been, and the seat sat
    /// idle for the twenty seconds that followed.
    ///
    /// Five seconds is above the widest reading of that window and below the
    /// point where a spawn feels stuck.
    ///
    /// It used to be paid on every spawn, which on that evening was the whole
    /// cost of the defect. It is now paid by a spawn that has already been told
    /// the turn did not start and is confirming a submit, because a backend that
    /// reports [`Refusal::NotTaken`] has done this wait already and
    /// [`hand_over`] does not repeat it. A healthy handover through herdr pays
    /// one `agent.get` and no sleep at all.
    taken: Duration,
    /// How many times to press submit again on a work order that arrived and
    /// was not taken.
    ///
    /// Bounded because pressing it is not free — a submit into an agent that
    /// *is* working goes in as an empty line — and because a second failure is
    /// evidence about the seat rather than about the keystroke. Two because a
    /// press can be lost the same way the first submit was: one is what the
    /// measurement above needed, and the spare is for the case where the
    /// composer was still a tenth of a second away.
    nudges: u32,
    /// How often to ask.
    poll: Duration,
    /// How long a seat has to go on looking empty before that is a death rather
    /// than a gap in what the backend can see.
    ///
    /// Two seconds is three times the longest gap measured — 620ms from
    /// `agent.start` to a named agent, recorded on 2026-08-17 — and it is only
    /// ever paid by a spawn that has genuinely failed, because one reading of a
    /// live agent clears it.
    gone: Duration,
    /// How many further starts a seat gets when the first one does not survive.
    ///
    /// **What this retries is a start, and never a work order.** The three
    /// failures [`hand_over`] can end in all leave the same ambiguity — no turn
    /// running, and possibly a work order sitting in a composer — and the only
    /// safe answer to that is a fresh agent, which herdr has no verb for: its
    /// one way to end an agent is `pane.close`, which takes the seat with it.
    /// So a handover that failed is reported and not retried, and what is
    /// retried is the case where the seat is already empty because the agent
    /// died on its way up. That is the case Ed measured on 2026-08-18 and the
    /// one an overloaded API keeps producing.
    ///
    /// Two, and the number is a judgement rather than a measurement, which is
    /// worth saying plainly because the numbers above are measurements. The rate
    /// this used to be calibrated against cannot be read any more: the
    /// 2026-08-18 sightings are a wsp fault (robustness-083's `done` agent,
    /// which could not be told anything at all) and an API fault added together,
    /// in a proportion nobody can now recover. With 083 fixed this is the rare
    /// residue rather than the common case, so it is set to cost little and
    /// catch the easy half.
    attempts: u32,
    /// How long to wait before starting again, multiplied by [`Patience::steeper`]
    /// each time.
    ///
    /// Backoff rather than an immediate retry because the fault it answers is an
    /// overloaded API — six sessions opening inside thirty seconds failed and the
    /// same six spaced out succeeded — and an immediate retry is another
    /// connection into the same overload. Three seconds and then nine bounds the
    /// added wait on a spawn that is going to fail anyway at twelve, which is
    /// under the thirty [`Patience::ready`] already spends.
    backoff: Duration,
    /// What each backoff is multiplied by. Three, so two attempts span an order
    /// of magnitude rather than doubling into the same overload.
    steeper: u32,
    /// How long to wait for a seat to be able to hold an agent again.
    ///
    /// **This wait is the retry, and it is a wait rather than a call.** herdr
    /// refuses `agent.start` into a pane while `is_agent_terminal()` holds, and
    /// the agent *name* outlives the process, so a seat whose agent has died is
    /// briefly one that will answer `TargetBusy`. There is no verb to clear it:
    /// `pane.release_agent` is a no-op for exactly this case
    /// (`is_official_agent_source("herdr:claude", "claude")` is true and the
    /// handler returns early), and `pane.clear_agent_authority` drops the hook
    /// authority without touching the name.
    ///
    /// What does clear it is `agent.get`, which calls
    /// `reconcile_managed_agent_target` before it answers — so the polling
    /// [`Place::state`] already does is itself what re-arms the seat. Waiting
    /// for [`State::Empty`] is therefore both the test and the mechanism, and it
    /// is the same reading that tells `available_shell_name` the pane's
    /// foreground is a shell again.
    ///
    /// A seat that has not emptied inside this is one holding an agent that is
    /// alive and not working, which is the case this must not retry into: it
    /// stops and reports. Five seconds against the 620ms herdr was measured
    /// taking to notice an agent at all.
    rearm: Duration,
    /// What time it is, and how to wait for the next look.
    ///
    /// Handed in rather than read, and this line was written by a flaky test
    /// rather than by taste. The throttle below is *per elapsed second* — ask
    /// the runtime once per [`Patience::gone`], because asking it is a
    /// subprocess — and a test that counts the asks in a fixed number of polls
    /// is measuring the machine's load, not the rule. It passed alone and failed
    /// under the full suite, which is the worst way for a test to be wrong:
    /// three agents building beside it stretched two hundred polls over enough
    /// wall-clock for a ninth ask.
    ///
    /// So the clock is a parameter and the test drives it, which makes the bound
    /// exact arithmetic instead of a guess about scheduling. The same seam the
    /// handbook asks for everywhere else: what value would have to be passed in
    /// for this to be testable?
    ///
    /// It carries the sleep as well as the reading, which is robustness-054's
    /// correction: a test that drove the clock and left the sleep behind still
    /// had to set `poll` to zero to be fast, so the loop under test was not the
    /// loop that runs. [`util::Clock`] holds the whole argument, and
    /// `place_herdr::Herdr` waits through the same one.
    clock: &'a dyn Clock,
}

impl Default for Patience<'static> {
    fn default() -> Patience<'static> {
        Patience {
            ready: Duration::from_millis(30_000),
            taken: Duration::from_millis(5_000),
            nudges: 2,
            poll: Duration::from_millis(150),
            gone: Duration::from_millis(2_000),
            attempts: 2,
            backoff: Duration::from_millis(3_000),
            steeper: 3,
            rearm: Duration::from_millis(5_000),
            clock: &util::Wall,
        }
    }
}

/// Wait until the agent in a seat will take a sentence.
///
/// **Not until it is idle**, which is the first trap this walked into: herdr
/// reports `agent_status: idle` while an agent is still drawing its banner, and
/// refuses a prompt in that window. `will_take_a_prompt` is the port's single
/// answer to the question every `state == "idle"` caller is actually asking, so
/// this is the one reading and there is nothing left here to get wrong.
///
/// **An empty seat is not a death, which is the second, and it cost a night.**
/// This used to return on the first empty reading, arguing that the agent
/// existed when `start` returned so nothing in it now meant it had stopped. Both
/// halves of that were wrong at once: `start` was coming back inside the launch
/// window (see `place_herdr::start`), and even with that fixed, one blind read
/// from one backend is a thin thing to end somebody's spawn on. It failed open
/// every time — a claimed task, a live agent, and no work order — and it was
/// only ever caught by a person watching the pane.
///
/// So an absence has to persist for [`Patience::gone`], and the kind gets a
/// veto. The
/// asymmetry in [`agent_commands::Kind::running`] is the point: it can say *this
/// agent is alive, keep waiting* and it cannot say *it is dead, stop now*,
/// because its registry is written a moment after the agent starts and "not yet"
/// and "never" look identical there. What that buys is a spawn that survives a
/// backend which cannot see, and still fails in two seconds rather than thirty
/// when the start really did fail.
fn wait_ready(
    place: &dyn Place,
    how: &dyn agent_commands::Kind,
    spawn: &agent_commands::Spawn,
    kind: &str,
    wait: &Patience,
) -> Result<(), String> {
    let seat = spawn.seat;
    let now = || wait.clock.now();
    let deadline = now() + wait.ready;
    let mut empty_since: Option<Instant> = None;
    loop {
        match place.state(seat) {
            Ok(s) if s.will_take_a_prompt() => return Ok(()),
            Ok(State::Empty) | Ok(State::Gone) => {
                let since = *empty_since.get_or_insert_with(now);
                if now().saturating_duration_since(since) >= wait.gone {
                    match how.running(spawn) {
                        // Alive, and the backend simply cannot see it. Start the
                        // clock again rather than clearing it, so the runtime is
                        // asked once per `gone` and not once per poll.
                        Some(true) => empty_since = Some(now()),
                        _ => return Err(format!("{kind} started and then stopped in {seat}")),
                    }
                }
            }
            // The seat itself, rather than what is in it. Nothing is coming back
            // from a pane that has been closed.
            Err(Refusal::NoSeat(_)) => return Err(format!("{seat} is gone")),
            // A refusal is not a verdict while there is time left: a backend that
            // did not answer this poll may answer the next.
            Ok(_) | Err(_) => empty_since = None,
        }
        if now() >= deadline {
            return Err(format!("{kind} started but never became ready for input"));
        }
        wait.clock.rest(wait.poll);
    }
}

/// Start an agent in a seat, and try again if it does not survive the attempt.
///
/// **The retry this task asked for, and the whole of the argument for why it is
/// here and not one step later.** `spawn` reports success on its own records
/// rather than on the far side's state — that is the house failure and this file
/// is where it lives — and `fork-015` closed the detection half: herdr now waits
/// for the turn and wsp knows when a work order did not land. What was left was
/// the recovery, and the recovery is only safe for one of the two failures.
///
/// **A failed handover is not retried, deliberately.** All three ways
/// [`hand_over`] can end leave the same ambiguity — no turn running, and a work
/// order that may be sitting in a composer — so the only retry that could not
/// duplicate it is a fresh agent. herdr has no verb for that: `pane.close` is
/// the one thing that ends an agent and it takes the seat with it, and re-opening
/// a seat means rebinding a claim, which is `cmd_agent`'s and is not paid for by
/// a transient fault. So that case reports, names the agent it could not reach,
/// and exits non-zero exactly as it did.
///
/// **A start that did not survive is retried, and that is the measured case.**
/// The agent died on its way up — six of six on 2026-08-18, an overloaded API
/// opening six model connections inside thirty seconds — leaving a seat with a
/// shell in it and nothing to duplicate into. Backoff is the answer to that
/// fault by construction rather than by hope.
///
/// The order of the two waits below is what makes it safe. [`re_arm`] runs
/// *before* the next start and refuses to proceed while anything is still in the
/// seat, so this can never start a second agent beside a first one that was
/// merely slow — and that same wait is what clears herdr's stale agent name; see
/// [`Patience::rearm`].
///
/// The name is reused across attempts rather than freshened, which is a decision
/// and not an oversight. `agent_commands::mint` is deterministic on purpose — the
/// same string at `agent.start`, in a failed spawn's recovery sentence, and to
/// anything later addressing the session channel — and a per-attempt suffix would
/// cost all three to dodge a `DuplicateName` that only fires while the seat has
/// not cleared, which is the condition [`re_arm`] already refuses to start under.
fn start_agent(
    place: &dyn Place,
    how: &dyn agent_commands::Kind,
    spawn: &agent_commands::Spawn,
    agent: &Agent,
    kind: &str,
    wait: &Patience,
) -> Result<(), String> {
    let seat = spawn.seat;
    let mut backoff = wait.backoff;
    for left in (0..=wait.attempts).rev() {
        let why = match place
            .start(seat, agent)
            .map_err(|e| e.to_string())
            .and_then(|()| wait_ready(place, how, spawn, kind, wait))
        {
            Ok(()) => return Ok(()),
            Err(why) => why,
        };
        if left == 0 {
            return Err(why);
        }
        // Said before the wait rather than after it, so a person watching a
        // spawn that has gone quiet knows what it is waiting for. The seat is
        // named because a spawn that retries is a spawn somebody will go and
        // look at.
        eprintln!("wsp: {kind} did not start in {seat}: {why}");
        if let Err(stuck) = re_arm(place, seat, wait) {
            return Err(format!("{why}, and {stuck}"));
        }
        eprintln!("wsp: trying {seat} again in {}s", backoff.as_secs());
        wait.clock.rest(backoff);
        backoff *= wait.steeper;
    }
    // Unreachable: the loop runs at least once and every path out of it returns.
    Err(format!("{kind} was never started in {seat}"))
}

/// Wait until a seat will take an agent again, or say why it will not.
///
/// [`State::Empty`] is a seat that exists with nothing in it, which is both the
/// condition `agent.start` needs and — because [`Place::state`] is `agent.get`,
/// and `agent.get` reconciles the managed agent before it answers — the thing
/// that brings it about. [`Patience::rearm`] carries that mechanism in full.
///
/// The failure is the interesting return. A seat that will not empty is holding
/// an agent that is alive and is not working, and starting a second agent beside
/// it is the one thing a retry must never do — so this refuses, and its caller
/// stops at reporting.
fn re_arm(place: &dyn Place, seat: &Seat, wait: &Patience) -> Result<(), String> {
    let now = || wait.clock.now();
    let deadline = now() + wait.rearm;
    loop {
        match place.state(seat) {
            Ok(State::Empty) => return Ok(()),
            // Nothing is coming back from a seat that has been closed, and a
            // retry into it would open nothing. The same reading `wait_ready`
            // takes, for the same reason.
            Err(Refusal::NoSeat(_)) => return Err(format!("{seat} is gone")),
            Ok(_) | Err(_) => {}
        }
        if now() >= deadline {
            return Err("it is still holding what failed, so it was not tried again".to_string());
        }
        wait.clock.rest(wait.poll);
    }
}

/// Hand over the work order, and come back only when a turn has started on it.
///
/// **The defect this is the whole of, and it fails open, which is why it was
/// expensive.** `tell` returns `Ok` when the backend accepted the call, and
/// `spawn` reported success on the send. What was actually happening on a
/// terminal backend is that the sentence arrived in Claude Code's composer and
/// the Enter after it was swallowed: an agent sitting idle in front of a work
/// order it has never been shown, holding a claim, and reported as a healthy
/// spawn. Ed found it by watching a pane; nothing else could have. Four of six
/// agents in one burst on 2026-08-17, three of three in a quiet moment that
/// night with the machine idle and nobody at the keyboard, after herdr 0.8.0
/// had already put a wait between its own text and its own Enter.
///
/// **It is not wsp's, and that is why nothing wsp could do to its own call
/// fixed it.** Reproduced on 2026-08-18 with no wsp in the picture at all — a
/// workspace, `agent.start`, `agent.prompt` on the raw socket — three runs,
/// three sentences left sitting in the composer with the agent idle, read back
/// off the screen. What wsp owns is the half after that: noticing, and saying
/// so.
///
/// So the readiness wait is not the fix and was never missing: [`wait_ready`]
/// runs first and asks `will_take_a_prompt`, which is herdr's optimistic
/// signal and goes true before the composer can take a submission. Waiting
/// longer only widens the window it is wrong in. **The only thing that settles
/// it is the agent leaving idle**, which is a fact about the agent rather than
/// a promise about the call.
///
/// **Half of that reading has since moved to the server, and the half that
/// stayed is the interesting one.** `agent.prompt` will now hold its reply until
/// the agent's status moves and refuse when it does not
/// (`place_herdr::Herdr::tell`), so on that backend the looking below is no
/// longer how the defect is caught — the answer arrives already knowing. What it
/// cannot answer is *why* nothing moved. herdr sees a prompt delivered to an
/// agent that is genuinely idle, and that is the same picture whether the
/// composer was a moment from ready or a folder-trust modal had the keyboard
/// when the text arrived. Only the second is a thing a keystroke fixes and only
/// wsp is in a position to try, so the submit stays and this function still
/// exists. What it no longer does is *discover* the failure by waiting five
/// seconds for it.
///
/// Then the loop is what was run by hand: press submit again, look again,
/// bounded, and fail loudly rather than print a success. `Unsupported` from
/// [`Place::nudge`] ends it at once — a backend with nothing to press has
/// nothing to try — and is still a failure, because the turn did not start
/// either way. That case is `wsp spawn --on <machine>` into a session nobody
/// had logged in, which reported three lines of success on 2026-08-17 for an
/// agent that could never take a turn.
///
/// [`State::turn_in_flight`] and nothing softer is what counts as taken, and
/// robustness-083 gave that reading a name without widening it. The one that
/// asks to be let in is [`State::Blocked`] — an agent that answers a work order
/// by asking for a permission has plainly taken it — and it must stay out,
/// because the *other* thing that reads `blocked` is a folder-trust modal
/// holding the keyboard with the order still unsent behind it. Those two are
/// the same status and opposite outcomes, and the second Enter below is right
/// for one of them, so this stays strict and lets the press decide.
///
/// The cost of that strictness is the one false alarm available here: an agent
/// that takes the order, finishes the whole turn inside [`Patience::taken`] and
/// is idle again by the first look. A work order whose first act is reading a
/// brief does not, and a spawn wrongly called failed is recoverable in a way
/// that a spawn wrongly called succeeded is not.
fn hand_over(
    place: &dyn Place,
    how: &dyn agent_commands::Kind,
    spawn: &agent_commands::Spawn,
    text: &str,
    wait: &Patience,
) -> Result<(), String> {
    let seat = spawn.seat;
    // What was removed here, and it is a reading rather than a line: a backend
    // that answers [`Refusal::NotTaken`] has just spent its own timeout watching
    // for the turn this used to go and look for, so the first look is skipped and
    // the submit is pressed at once. herdr does that watching now
    // (`place_herdr::tell`), which is why the five seconds below are no longer
    // the thing that catches the failure — they are what confirms the rescue.
    //
    // The refusal is not an error. A caller that propagated it would report the
    // one failure this function exists to recover from, and `?` here was the
    // whole of that mistake.
    let mut look = match how.tell(place, seat, text) {
        // Both deliveries look the same from here, and deliberately.
        // `Delivery::Started` is a status change of *some* kind and this
        // function is strict about which — see the paragraph above on
        // `Blocked` — so the confirmation stays wsp's own.
        Ok(_) => true,
        Err(Refusal::NotTaken) => false,
        Err(e) => return Err(e.to_string()),
    };
    for pressed in 0..=wait.nudges {
        if look && took_it(place, seat, wait) {
            return Ok(());
        }
        // Only the first look can be skipped. After a submit nobody has watched
        // anything, and the confirmation is wsp's again.
        look = true;
        if pressed == wait.nudges {
            break;
        }
        match place.nudge(seat) {
            Ok(()) => {}
            Err(Refusal::Unsupported(_)) => {
                return Err(format!("{seat} took the work order and started nothing"))
            }
            Err(e) => return Err(format!("{seat} did not take the work order: {e}")),
        }
    }
    // Where the work order now sits, said as the unknown it is. herdr wrote
    // the keystrokes either way — both of this function's endings are reached
    // only after a delivery — but whether the TUI took them into a composer or
    // dropped them on its floor is inside the application, and wsp has no
    // reading of that. The first spawn to report this asserted "sitting in
    // {seat} unsent", and the composer was empty when somebody looked three
    // seconds later; twice more that week read the same way, and all three
    // were recovered not by pressing return but by sending the order again,
    // which is the one repair that works on either half of the uncertainty.
    // Naming it is left to the caller, because the command is spelled with
    // the task id and only the caller has one.
    Err(format!(
        "no turn started in {seat}: the work order went to the pane, and \
         is either sitting unsent or was dropped — wsp cannot see which"
    ))
}

/// Whether a turn started inside [`Patience::taken`].
///
/// The same shape as [`wait_ready`]'s loop and deliberately not folded into it:
/// that one waits for an agent to become *able* to be told something, this one
/// waits for evidence that it *was*. They read the same call and mean opposite
/// things by an idle agent.
fn took_it(place: &dyn Place, seat: &Seat, wait: &Patience) -> bool {
    let now = || wait.clock.now();
    let deadline = now() + wait.taken;
    loop {
        if place.state(seat).is_ok_and(|s| s.turn_in_flight()) {
            return true;
        }
        if now() >= deadline {
            return false;
        }
        wait.clock.rest(wait.poll);
    }
}

/// Hand over the work order, and resend it once if no turn started.
///
/// The detection is [`hand_over`]'s and predates rotation; what rotation adds
/// is the retry, because the reader between detection and repair used to be a
/// person reading stderr, and the custodian's pane is exactly what rotation is
/// taking out of the loop. One resend of the *same* text, then stop — bounded,
/// because a send is not free either: worklist-010 was three copies of one
/// paragraph delivered to a seat that had queued the first. Here nothing is
/// resent unless no turn started at all, which is the evidence the order was
/// never taken; and the resend is the repair that worked on every recorded
/// failure of this shape, whether the text sat unsent in a composer or was
/// dropped outright.
fn confirm_turn(
    place: &dyn Place,
    how: &dyn agent_commands::Kind,
    spawn: &agent_commands::Spawn,
    text: &str,
    wait: &Patience,
) -> Result<(), String> {
    match hand_over(place, how, spawn, text, wait) {
        Ok(()) => Ok(()),
        Err(first) => {
            eprintln!("wsp: no turn started - sending the work order once more");
            hand_over(place, how, spawn, text, wait)
                .map_err(|second| format!("{first}; resent once and {second}"))
        }
    }
}

/// Say how to reach an agent a spawn could not, where the kind knows a way.
///
/// The failure this softens is measured rather than imagined: `spawn`'s work
/// order failed to arrive seven times in one night, and every one of those
/// agents was started, alive, and recovered by hand over its own session
/// channel. What that recovery cost each time was somebody listing every agent
/// on the machine and working out which name belonged to the pane that had just
/// failed. wsp is the only thing that already knows.
///
/// Best effort by construction, and never wrong about *whose* agent it names —
/// a work order sent to the handle here would go to whoever it names, so
/// `agent_commands::pick` refuses an ambiguous one and always will. What it is no
/// longer allowed to be is **silent**, which is what it was on all three of the
/// failures robustness-041 reproduced; it is asked the spawn rather than the seat
/// for the reason `agent_commands` measures, and it may hedge.
///
/// The order of the two lines at the call sites is load-bearing: the caller
/// prints *why* the spawn failed and then calls this, so the wait inside delays
/// the advice and never the diagnosis.
pub(crate) fn unreached(how: &dyn agent_commands::Kind, place: &dyn Place, spawn: &agent_commands::Spawn) {
    if let Some(line) = agent_commands::recovery(how.address(place, spawn)) {
        eprintln!("wsp: {line}");
    }
}

/// Wait for a just-started agent to be ready for input, on the CLI's patience.
///
/// The one thing a second caller of `place.start` needs and cannot have: the
/// two-question sequence — *does it exist*, *will it listen* — is this file's,
/// and [`Patience`] is deliberately private because its numbers were each set
/// by a failure and are not a knob. `wsp resume` starts an agent exactly the
/// way `spawn` does and waits exactly as long.
pub(crate) fn ready(
    place: &dyn Place,
    how: &dyn agent_commands::Kind,
    spawn: &agent_commands::Spawn,
    kind: &str,
) -> Result<(), String> {
    wait_ready(place, how, spawn, kind, &Patience::default())
}

/// What `spawn` resolved its argument to.
struct Work {
    task: Option<String>,
    project: Option<String>,
    /// The **worklist** this spawn is being seated on, when `--govern` named
    /// one rather than a project.
    ///
    /// A separate field rather than a scope in `project`, and that is the whole
    /// reason it exists: `project` is what the pane is *standing in* — it fills
    /// `WSP_PROJECT`, it resolves the cwd, and every reading of "where am I"
    /// walks it — and a worklist is none of those things. A slug written into
    /// it would give the custodian a brief about a project that does not exist,
    /// which is precisely the near miss the `--govern` guard below refuses to
    /// make.
    list: Option<String>,
    /// The workspace's opening name. A claim renames it after the task a
    /// moment later — to the same thing, for a task, so the window does not
    /// change its name under whoever was already looking at it. A project
    /// keeps this one.
    label: String,
}

/// A task, or a project, or nothing that resolves.
///
/// `-p` forces the project reading. Without it a task is tried first and a
/// project second, which is the order the ids themselves suggest: `wsp-014`
/// can only be a task, and a project slug can only be a project, so the two
/// collide solely on a title substring — where the task is what was meant, that
/// being the thing you were just reading.
fn resolve(store: &Store, args: &Args, index: &Index) -> Result<Work, String> {
    if let Some(p) = args.get("project") {
        // A worklist, but only for `--govern`. A list is a thing to *run*, not
        // a place to work: it has no root to stand in and no backlog to claim
        // out of, so `-p <slug>` on its own would open a workspace that could
        // not answer the first question asked of it. Under `--govern` it is the
        // obvious thing and needs no flag of its own — the seat's key is a
        // scope, so this is the same command it always was pointed at the other
        // half of one key space.
        if args.has("govern") {
            if let Some(w) = store.worklist(&p) {
                return Ok(Work {
                    task: None,
                    project: None,
                    label: crate::cmd_govern::governor_of(&w.id),
                    list: Some(w.id),
                });
            }
        }
        let proj = index.find(&p).ok_or_else(|| format!("no project matching `{p}`"))?;
        return Ok(Work {
            task: None,
            project: Some(proj.id.clone()),
            label: proj.name.clone(),
            list: None,
        });
    }
    let needle = args
        .rest
        .first()
        .cloned()
        .ok_or_else(|| "usage: wsp spawn <task|-p project> [--agent]".to_string())?;
    if args.has("govern") {
        if let Some(w) = store.worklist(&needle) {
            return Ok(Work {
                task: None,
                project: None,
                label: crate::cmd_govern::governor_of(&w.id),
                list: Some(w.id),
            });
        }
    }
    if let Some(t) = store.find_task(&needle) {
        return Ok(Work {
            project: t.project.clone(),
            label: cmd_agent::task_label(&t).unwrap_or_else(|| t.title.clone()),
            task: Some(t.id),
            list: None,
        });
    }
    match index.find(&needle) {
        Some(proj) => Ok(Work {
            task: None,
            project: Some(proj.id.clone()),
            label: proj.name.clone(),
            list: None,
        }),
        None => Err(format!("no task or project matching `{needle}`")),
    }
}

pub fn spawn(store: &Store, args: &Args) -> i32 {
    // No pre-flight question about a socket. `herdr::available()` used to guard
    // this and printed a herdr sentence for a herdr fact; a backend that is not
    // answering now says so from the call that wanted it, which arrives at the
    // same moment and names what it stopped.
    place_work(backend(args).as_ref(), store, args)
}

/// Which backend places this work: a terminal, a supervisor with none, or a
/// compound session — a terminal wsp itself owns rather than herdr's.
///
/// The flag is the whole of the choice and there is deliberately no inference
/// behind it. What `--headless` buys is an agent that can be started, told,
/// observed and stopped with no terminal anywhere; what it costs is the one
/// thing a supervisor cannot give you, which is an agent you can **sit down in
/// front of** — no permission prompts, no input box, no attaching. `--compound`
/// buys that back through a different multiplexer: a real pty, hosted by
/// `compound-sup` rather than herdr (`compound-064`). That is a decision about
/// how you mean to work with this agent rather than a detail of where it runs,
/// so it is asked rather than guessed, and the two flags are mutually
/// exclusive for the same reason a seat cannot be in two backends at once.
///
/// Everything below this line is backend-agnostic already, which is the port
/// earning itself: `place_work` was written against `&dyn Place` and needed no
/// change to grow a second implementor, or now a fourth.
///
/// `pub(crate)` because `wsp stamp` asks the same question — which backend is
/// running the agents — and a second copy of this match is a second place to
/// forget when the next implementor lands. That is this repository's oldest
/// lesson about hand-kept lists, and the port is the thing that makes one copy
/// enough.
pub(crate) fn backend(args: &Args) -> Box<dyn Place> {
    match (args.has("headless"), args.has("compound")) {
        (true, _) => Box::new(crate::place_super::Supervisor::new()),
        (false, true) => Box::new(crate::place_compound::Compound::new()),
        (false, false) => Box::new(Herdr::new()),
    }
}

/// Every backend wsp can spawn onto, for the readers that do not get to ask —
/// `wsp wip` folds all of them into one census (`Wip::live`, `wsp-100`+
/// `compound-078`) and `wsp tell` (`compound-077`) has to find the ONE that
/// answers for a seat before it knows how to reach it. `headless` is left
/// out: `place_super` has no terminal to fold into a listing and no prose to
/// receive that is not already reachable through its own `agent.prompt` —
/// see `robustness-022`'s three fates.
///
/// The same "one copy" argument [`backend`] makes: a reader that built its
/// own array would be a second list to update when a fifth implementor
/// lands.
pub(crate) fn local_backends() -> [Box<dyn Place>; 2] {
    [Box::new(Herdr::new()), Box::new(crate::place_compound::Compound::new())]
}

/// The name a whole backend's own silence is filed under — `wsp stamp`
/// (`compound-064` item 2), paired with [`local_backends`] by position, so
/// this file stays the one place that ordering is named rather than
/// re-assumed at a call site. `""` is herdr's own local machine
/// ([`Census::heard`]'s spelling, unchanged); `"compound"` distinguishes
/// the second backend's total refusal from a machine of that name, since a
/// backend is not a machine and the two silences must not collide in a
/// listing.
pub(crate) const LOCAL_BACKEND_NAMES: [&str; 2] = ["", "compound"];

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

    // Where this work sits, which is the one thing the tier default below reads
    // — and it is read here, off the store this function already holds, so that
    // `tier` itself stays a function four literals can check. `Running::read`
    // is the same directory read `wsp message` and `wsp task` do to answer the
    // same question, and it is paid on every spawn rather than behind the
    // trigger, because the trigger is one expression inside `tier` and copying
    // it out here to save a directory listing would be two places to change it.
    let governors = store.governors();
    let running = crate::worklist::Running::read(store);
    let scope = Scope {
        governors: &governors,
        index: &index,
        // A `--govern` spawn names its own list; a task spawn is a member of
        // whatever is running over it. Either way this is the front of the
        // walk — see `cmd_govern::seat_for`.
        list: work
            .list
            .as_deref()
            .or_else(|| work.task.as_deref().and_then(|t| running.list_of(t))),
        project: work.project.as_deref(),
    };

    // Both read before anything is opened, so a mistyped alias costs a line of
    // output instead of a workspace to tear down again — and so a tier this
    // infers is printed before the workspace it applies to exists.
    let kind = args.get("kind").unwrap_or_else(|| DEFAULT_KIND.to_string());
    let (model, effort) = match tier(args, &kind, args.has("agent") || args.has("govern"), &scope) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    // A slot is on a project, so `--govern` on a task is a sentence with no
    // meaning rather than a near miss — and the near miss it would otherwise
    // become is the expensive one: an agent claimed onto a task and told it is
    // the custodian of everything above it.
    if args.has("govern") && work.task.is_some() {
        eprintln!("wsp: --govern seats an agent on a project or a worklist — name one, or use -p");
        return 2;
    }

    // Still this machine's paths, deliberately. A project root is a path in the
    // store and `~` expands here, which is right while the machines mirror each
    // other and is exactly what the Linux box breaks; host-qualified roots are
    // wsp-025 and are not smuggled in here.
    let cwd = args
        .get("cwd")
        .or_else(|| work.project.as_deref().and_then(|p| index.root_of(p)));

    // One tree per agent, which is the whole of robustness-010 and is done here
    // rather than asked of the agent. Every softer version of it has been tried
    // in this repository and recorded on that task — naming paths, staging by
    // hunk, announcing first — and each failed the same way: an agent that has
    // to remember a rule is an agent that will be halfway through something
    // more interesting when it matters. An agent that is simply standing in a
    // checkout of its own cannot take anybody's work, because the work is not
    // there to take.
    //
    // Only a task gets one. A project seat is a place to work rather than a
    // piece of work, it has nothing to branch for and nothing to land, and it
    // is where a person reviewing the whole tree wants to be standing.
    // `--no-tree` for the case where you deliberately want the trunk, and a
    // spoken fallback when the tree cannot be made, because a spawn that fails
    // outright over this is worse than a spawn that says where it put you.
    //
    // Asked here rather than inside the arm: a flag nothing read and the help
    // does not list is refused now (`main::unknown_flags`), and a project seat
    // never reaches this arm, so asking down one branch only would refuse the
    // word on the other.
    let no_tree = args.has("no-tree");
    let cwd = match (&work.task, &cwd) {
        (Some(task), Some(root)) if !no_tree => {
            Some(crate::cmd_checkout::tree_for(root, task).unwrap_or_else(|| {
                eprintln!("wsp: no tree of its own for {task} — opening in {root}");
                root.clone()
            }))
        }
        _ => cwd,
    };

    // Focus is asked for, never assumed. This line used to read
    // `!args.has("no-focus")`, so every spawn dragged the screen onto the new
    // seat unless the caller thought to say otherwise — and `spawn` is run by
    // the queue at least as often as by hand, which meant a batch starting
    // overnight took the screen off whatever a person was reading. herdr's
    // `workspace.create` defaults `focus` to false and wsp was opting in; the
    // doc on [`Order::show`] already said `false` by default, so the port
    // stated the rule and this line contradicted it.
    //
    // `--no-focus` is still parsed (`main::BOOL_FLAGS`) and now asks for what
    // already happens: dropping it outright would make an old invocation swallow
    // the id after it rather than fail, which is a worse way to learn.
    //
    // The ask below is honoured and the screen still moves, which is not this
    // line's doing: herdr's `workspace.created` runs wsp's own plugin hook, the
    // hook installs the panel, and the `pane.swap` it needs focuses with no way
    // to decline. See `panel::install::install_one` and `fork-002`.
    // Only where an agent is actually going to be started. `--kind` on a bare
    // workspace names nothing that will run, and configuring a runtime nobody
    // is launching would be a variable in a shell somebody else is using.
    // `--govern` reads the same way it does where the slot is taken below: a
    // custodial spawn starts an agent too, and it is the one case where there
    // is no `--agent` flag to look at.
    let will_start = args.has("agent")
        || (args.has("govern") && (work.list.is_some() || work.project.is_some()));
    // What this seat is about, in one word: the task, or — under `--govern` —
    // the scope it answers for. Worked out here rather than where the agent is
    // named below, because the seat's environment is fixed when the seat opens
    // and the brief's path is part of it.
    let subject = work
        .task
        .clone()
        .or_else(|| work.list.clone())
        .or_else(|| work.project.clone())
        .unwrap_or_default();
    // Where this seat's brief will be written, when the kind is one that reads
    // a brief out of a file. Named now and written later: the environment has
    // to be settled before `open`, and the brief cannot be composed until the
    // claim has landed a moment after it. The path is knowable in advance,
    // which is the whole reason this works.
    let brief_at = (will_start && !subject.is_empty() && agent_commands::of(&kind).brief_file())
        .then(|| brief_path(store, &subject));
    // What this seat may reach outside its tree, settled here for the same
    // reason the brief's path is: the environment is fixed when the seat opens,
    // and by then the tree and the task are both known.
    let outside = reach(store, work.task.as_deref(), cwd.as_deref());
    let occupant =
        will_start.then(|| Occupant { kind: &kind, brief: brief_at.as_deref(), reach: &outside });
    // `--govern` is the custodial spelling and the only one: it refuses a task
    // outright, so the flag alone says everything `seat_env`'s `custodian` half
    // needs. Asked here rather than read again below, because the environment
    // is fixed by `open`, which this feeds.
    let custodian = args.has("govern");
    let order = order(&work, cwd.as_deref(), on.as_deref(), args.has("focus"), occupant, custodian);
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
            // The tier travels with the claim, because the claim is what
            // writes the attempt down. `spawn` is the only caller that knows
            // it — an agent claiming at its own shell has no flag to pass and
            // leaves the clause off, which is the honest record of a claim
            // made with nothing stated.
            if let Some(m) = &model {
                flags.push(("model", m));
            }
            if let Some(e) = &effort {
                flags.push(("effort", e));
            }
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

    // The other assignment: this workspace is the custodian of a project. Done
    // here, beside the claim and for the same reason it is here rather than
    // after the agent starts — the agent's `SessionStart` hook runs `wsp
    // brief`, and a slot recorded a second later is a custodian whose first
    // sight of itself is a brief about holding nothing.
    // The scope the slot is keyed on, which is a project id or a worklist slug
    // — see [`cmd_govern::seat_for`]. Everything below reads it as a name to
    // record a seat under and to tell an agent what it is custodian of, and
    // neither of those cares which of the two it is.
    let governing: Option<String> = match args.has("govern") {
        true => work.list.clone().or_else(|| work.project.clone()),
        false => None,
    };
    if let Some(project) = &governing {
        match workspace_of(&seat) {
            Some(ws) => {
                if let Some(was) = cmd_govern::take(store, project, &ws, seat.as_str()) {
                    println!("  {}", p.dim(&format!("{project} seat taken from {}", was.workspace)));
                }
            }
            // The workspace id is herdr's word and the port has none for it, so
            // this is the one fact `spawn` cannot get through `place`. Spoken
            // rather than fatal: the terminal is open and the agent is about to
            // be told what its job is, and the record is one command away from
            // inside it.
            None => eprintln!(
                "wsp: opened {seat} but could not record the {project} seat — \
                 run `wsp govern {project}` in it"
            ),
        }
    }

    // A slot is for an agent. `--govern` without `--agent` would record a
    // position and leave a shell sitting in it, which is a seat that answers
    // for raised hands and cannot read one.
    let mut started: Option<String> = None;
    let mut told = false;
    let mut ordered = false;
    if args.has("agent") || governing.is_some() {
        let name = subject.clone();
        let how = agent_commands::of(&kind);
        // The work order, decided *before* the agent starts rather than after
        // it is listening. It used to be computed below, next to the sentence
        // that delivers it, which was right while every kind was told; a kind
        // that takes its order in argv needs it in hand at `args` time. What
        // decides it has not changed and is still nothing about the backend:
        //
        // A task gives an agent something to be told, and so — since
        // robustness-048 — does a project it is being made custodian of. A bare
        // project workspace is still what the line here used to say of all of
        // them: a place to work rather than an instruction, with `f` in the
        // panel the key that turns one into the other. What changed is that
        // `--govern` is an instruction, and it is the custodial one.
        //
        // What `core-032` changes is not the sentence but whether it is true: a
        // kind that would otherwise arrive with an empty context has its brief
        // written where its own runtime will load it, and then the same
        // sentence every other spawn says is true for it too.
        //
        // Written here rather than before the seat opened, because this is the
        // first moment there is anything to say: the claim has landed, so the
        // brief is about this task rather than about an empty seat.
        let laid = match (&brief_at, work.task.is_some() || governing.is_some()) {
            (Some(path), true) => lay_brief(store, &work, &seat, cwd.as_deref(), path),
            _ => Laid::Elsewhere,
        };
        let order = match (&work.task, &governing) {
            (Some(t), _) => Some(handover(t, Handover::Spawned, route(how, laid))),
            (None, Some(p)) => Some(handover(p, Handover::Custodian, route(how, laid))),
            (None, None) => None,
        };
        // Only for a kind that takes it that way. Handing it to every kind's
        // `args` would put the whole work order on Claude Code's command line,
        // where nothing reads it and every byte of it is visible in `ps`.
        let in_args = how.order_in_args();
        // The seat is passed in because the agent is named after it, and it can
        // be passed in because `open` has happened above: the port splits opening
        // a seat from starting an agent in it, so wsp holds the seat before
        // there is anything sitting there to ask. That ordering is what makes
        // the handle knowable in advance rather than discovered afterwards.
        let spawn = agent_commands::Spawn {
            full: args.has("full"),
            subagents: args.has("subagents"),
            name: &name,
            seat: &seat,
            model: model.as_deref(),
            effort: effort.as_deref(),
            order: match in_args {
                true => order.as_deref(),
                false => None,
            },
            resume: None,
        };
        let agent = Agent { kind: kind.clone(), name: name.clone(), args: how.args(&spawn) };
        // Two waits, and they are different questions: `start` comes back when
        // the agent exists, `wait_ready` when it will listen. Both are inside
        // `start_agent` now, because the thing worth retrying is the pair — an
        // agent that started and then died failed the spawn exactly as one that
        // never started did, and only the second wait can tell.
        match start_agent(place, how, &spawn, &agent, &kind, &Patience::default()) {
            Ok(()) => {
                started = Some(kind.clone());
                // A task gives an agent something to be told, and so — since
                // robustness-048 — does a project it is being made custodian of.
                // A bare project workspace is still what the line here used to
                // say of all of them: a place to work rather than an
                // instruction, with `f` in the panel the key that turns one
                // into the other. What changed is that `--govern` is an
                // instruction, and it is the custodial one.
                if let Some(text) = &order {
                    ordered = true;
                    match in_args {
                        // Already delivered: it went out on the command line
                        // with the agent, so there is nothing to send and a
                        // send would say it twice. The turn it starts is the
                        // agent's own doing and wsp has no submit to press, so
                        // there is nothing here to confirm either — which is
                        // the point of argv rather than a gap in it. See
                        // `agent_commands::Kind::order_in_args`.
                        true => told = true,
                        // Through the kind rather than through the port.
                        // Readiness is established above, and *how* a sentence
                        // reaches an agent of this kind — its own channel, or
                        // the backend typing at it — is the one decision this
                        // file must not make; see `agent_commands`.
                        false => match hand_over(place, how, &spawn, text, &Patience::default()) {
                            Ok(()) => told = true,
                            Err(e) => {
                                eprintln!("wsp: agent started but not working on it: {e}");
                                // The repair first, and spelled so it can be
                                // pasted: `wsp tell` takes a task id or a pane
                                // id, and `-` reads the order from a stream —
                                // the same prose this spawn was given. A task
                                // is the readable form; a custodian has no
                                // task, but the seat contains `:` and resolves
                                // as the pane it is.
                                eprintln!(
                                    "wsp: send it again with `wsp tell {} -` — that repairs either",
                                    work.task.as_deref().unwrap_or(seat.as_str())
                                );
                                unreached(how, place, &spawn);
                            }
                        },
                    }
                }
            }
            Err(e) => {
                eprintln!("wsp: {kind} did not start in {seat}: {e}");
                unreached(how, place, &spawn);
            }
        }
    }

    // The session the agent is running under, recorded now that there is one.
    //
    // The claim above ran *before* `start` — that ordering is this file's and is
    // deliberate, because the agent reads the claim in its `SessionStart` brief
    // — so the binding it wrote could not carry a session, and until 2026-08-17
    // none ever did. This is the first moment anything can, and it is the moment
    // the value is cheapest: the backend has just been asked whether the agent
    // is ready, so it plainly has an opinion about what is in the seat.
    //
    // Best-effort, and silent. A backend that cannot say yet — herdr's detection
    // lags a launch by a second or so — leaves the field empty and the daemon's
    // next `sync` fills it, which is why nothing here waits or complains.
    if started.is_some() {
        if let Ok(rows) = place.census() {
            cmd_agent::learn_sessions(
                store,
                rows.seats()
                    .filter(|r| r.seat == seat)
                    .map(|r| (r.seat.as_str(), r.session.as_str(), r.agent.kind.as_str())),
            );
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
        // What the bare form did not do, said beside what it did. The synopsis
        // used to promise an agent for every spawn, and the sentence this
        // prints on success — "opened a terminal in w5Q:p1" — is true while
        // telling none of the story: three governors read it as somebody
        // working and left a claimed task sitting idle behind a shell prompt.
        // One dim line, on a verb run once per spawn rather than once per
        // request, against a lost governor turn every time it goes unread.
        if started.is_none() && !args.has("agent") && governing.is_none() {
            println!("  {}", p.dim("no agent in it — --agent starts one"));
        }
        if told {
            println!("  {}", p.dim("told it what it is holding"));
        }
    }
    // An agent asked for and not started is a failure however well the seat
    // went: the caller wanted somebody working, and there is nobody.
    if (args.has("agent") || governing.is_some()) && started.is_none() {
        return 1;
    }
    // And a work order that started no turn is the same failure one step later,
    // which is robustness-035 and is the reason this line exists. The seat is
    // open, the claim is written and the agent is alive — all of it true, none
    // of it work — so an exit code is the only thing left that a queue spawning
    // unattended can read. Saying so in the status is what stops a governor
    // concluding the night is under way.
    if ordered && !told {
        return 1;
    }
    0
}

/// `wsp govern <scope> --rotate` — the handover as one verb, and the
/// custodian's last act.
///
/// **The rule the verb encodes: nothing is ended on a promise.** Rotation used
/// to be a three-step instruction in this work order — pass the verdict with
/// `go`, run `spawn -p <scope> --govern`, end your session. The composition was
/// right against the alternatives and wrong in one specific way: it had a step
/// that could half-succeed. `spawn` seats the successor and starts its agent,
/// and the work order can then sit unsent in the composer — observed on an
/// ordinary spawn on 2026-08-25, detected (`hand_over` names it, non-zero), and
/// not repaired, because the third step was a separate instruction to an agent
/// that had already decided it was finished. The slot was filled, so no vacancy
/// fired; the successor held no order; the sentence naming the repair printed
/// into a pane that was closing. Detected was not good enough: the seat-stalled
/// wake fires on exactly this shape but is addressed *above* the seat, so an
/// unattended night woke a person five minutes later instead of nobody at all.
///
/// So one verb does all of it, in an order where every failure degrades to
/// *the predecessor is still seated*:
///
/// 1. Seat the successor and start its agent — [`place.open`], brief,
///    [`start_agent`], exactly as any custodial spawn.
/// 2. Confirm a turn started on the handover — [`confirm_turn`], with one
///    resend of the same order before giving up.
/// 3. Only then move the slot and let go.
///
/// Failing at 2 leaves the caller seated, says what it found, exits non-zero.
/// There is no restore path because nothing needs restoring: **the slot moves
/// last**, where `spawn --govern` evicts the incumbent up front. Holding the
/// seat until the successor's first turn also removes the vacancy window the
/// up-front eviction opens, which is what makes it strictly better than both
/// the old composition and the `--handover` flag argued down before core-049.
///
/// One kind of successor is refused outright: a kind whose work order travels
/// in argv (`opencode`). There is nothing to confirm against — "the turn it
/// starts is the agent's own doing, and wsp has no submit to press", as
/// [`agent_commands::Kind::order_in_args`] puts it — so a rotation into one
/// could only move the slot on a promise, which is the thing this verb refuses
/// to do. Rotate with a kind wsp can hear from.
///
/// # The ending rides the store, because the work order cannot be trusted with it
///
/// A verb that ends the pane it runs in cannot report what happened, so the
/// caller's despawn is not step 4 here — it is handed to the successor, carried
/// in the custodial brief rather than in the typed order. The typed order is
/// the one piece of handover state that does not go through the store, which is
/// why it can be dropped at all; putting the predecessor's death warrant in it
/// would rebuild the exact half-success this verb exists to remove. The record
/// ([`Store::set_handover`]) is written the moment the successor's seat exists,
/// read by the brief ([`crate::cmd_govern::incoming`]), and cleared by
/// [`despawn`] when the pane it names actually goes — one ending, the one
/// `despawn` already has, reused rather than grown anew.
///
/// The brief also says *when*: the seat moves on your first turn, and only then
/// is the ending yours to do. That sentence is not what enforces it. `despawn`
/// refuses the predecessor while the record names the caller as successor and
/// the slot has not moved ([`crate::cmd_govern::rotation_pending`]), because a
/// successor acting on its session-start text without re-reading would kill
/// this pane between step 2 and step 3 — a vacancy produced by the verb built
/// to prevent one.
///
/// The write happens before the agent starts, because the brief is composed at
/// start; if the rotation fails below, it is taken back — a stale record orders
/// the death of a pane that is still the seated custodian, and that must only
/// ever exist while a confirmed successor stands ready to inherit.
pub fn rotate(store: &Store, args: &Args) -> i32 {
    // The successor OPENS a seat, which is a spawn — so it opens wherever a
    // spawn would, through the same one-copy choice (`compound-091`). It used
    // to name herdr here, which meant `--compound` was honoured by every verb
    // that starts an agent except the one that replaces a custodian.
    rotate_on(backend(args).as_ref(), store, args, &Patience::default())
}

/// [`rotate`] against a stated backend and clock, which is the shape every test
/// of it takes: a herdr that answers from a script, and waits measured on a
/// dial rather than on the machine.
fn rotate_on(place: &dyn Place, store: &Store, args: &Args, wait: &Patience) -> i32 {
    let p = Paint::new();
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp govern <project|worklist> --rotate");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(scope) = cmd_govern::scope_of(store, &index, &needle) else {
        eprintln!("wsp: no such project or worklist `{needle}`");
        return 1;
    };

    // Refuse when there is nothing to rotate into, by the same condition that
    // puts the rotate line in front of a custodian: `next` offers it only while
    // a group stands behind the barrier just passed, and seating a successor at
    // the end of a run seats an agent with nothing left to sequence. A project
    // scope has no run to consult, and a list that is held or not yet started
    // has groups still owed, so both rotate as usual.
    if store.worklist(&scope).is_some() {
        let over = crate::worklist::running_position(store, &scope).is_some_and(|pos| pos.finished());
        if over {
            eprintln!("wsp: the {scope} run is finished - there is no group left to sequence");
            eprintln!("wsp: stand down instead: wsp govern {scope} --clear");
            return 1;
        }
    }

    // Rotation is the seat's own act, from the seat's own pane. Anyone else
    // running it would be handing a position away behind its holder's back,
    // and "the caller stays seated" means nothing for a caller that was never
    // seated. Read the way `govern` reads them: workspace from the room, pane
    // exact, because a room can hold more than one agent (worklist-035).
    let env = crate::herdr::Env::read();
    let (Some(ws), Some(me)) =
        (env.workspace_id.clone(), env.pane_id.clone().filter(|p| !p.is_empty()))
    else {
        eprintln!("wsp: {scope} is rotated by whoever holds its seat, from its own pane");
        return 2;
    };
    let governors = store.governors();
    match cmd_govern::governs(&governors, &ws, Some(&me)) {
        Some(held) if held == scope => {}
        Some(other) => {
            eprintln!("wsp: this pane holds the {other} seat, not {scope}");
            return 1;
        }
        // Not the seat, said precisely: named who has it when there is one to
        // name, and where an empty seat is filled from when there is not.
        // Either way this pane does not hold what it is trying to hand over,
        // and every branch here stops before anything is opened.
        None => {
            match cmd_govern::seat_of_scope(&scope, &governors) {
                Some(seat) => eprintln!(
                    "wsp: the {scope} seat is held by {} - only that pane can rotate it",
                    seat.workspace
                ),
                None => eprintln!("wsp: nobody holds the {scope} seat - wsp spawn fills an empty one"),
            }
            return 1;
        }
    }

    // The successor, resolved through the same door any custodial spawn walks:
    // a scope is a project or a worklist, and `resolve` under `--govern`
    // already knows both without being told twice.
    let asked_kind = args.get("kind");
    let asked_on = args.get("on");
    let mut flags: Vec<(&str, &str)> = vec![("govern", "true")];
    if let Some(k) = &asked_kind {
        flags.push(("kind", k.as_str()));
    }
    if let Some(o) = &asked_on {
        flags.push(("on", o.as_str()));
    }
    let spawn_args = Args::synth("spawn", &[scope.as_str()], &flags);
    let work = match resolve(store, &spawn_args, &index) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };
    let on = match placement(store, &spawn_args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };
    let kind = asked_kind.unwrap_or_else(|| DEFAULT_KIND.to_string());
    // Before anything is opened, like every other refusal here. The evidence
    // step two turns on is a turn wsp can see; a kind whose order goes out on
    // the command line gives none, ever.
    if agent_commands::of(&kind).order_in_args() {
        eprintln!(
            "wsp: {kind} takes its work order in argv, where no turn can be confirmed - \
             the slot would move on a promise"
        );
        eprintln!("wsp: rotate with a kind wsp can hear from - --kind claude");
        return 2;
    }
    // A project root, inherited; a worklist scope stands nowhere, exactly as a
    // first custodian's seat does.
    let cwd = work.project.as_deref().and_then(|proj| index.root_of(proj));
    let subject = work.list.clone().or_else(|| work.project.clone()).unwrap_or_default();

    let how = agent_commands::of(&kind);
    let brief_at =
        (!subject.is_empty() && how.brief_file()).then(|| brief_path(store, &subject));
    let outside = reach(store, None, cwd.as_deref());
    // An agent is the whole point of a rotation — a successor with no agent in
    // it is a seat that answers for raised hands and cannot read one — so there
    // is no bare-workspace case here to make this conditional.
    let occupant = Occupant { kind: &kind, brief: brief_at.as_deref(), reach: &outside };
    let open_order = order(&work, cwd.as_deref(), on.as_deref(), false, Some(occupant), true);

    // Step 1. The seat exists before anything is recorded or started — its id
    // is half of the handover record, and the brief cannot name the ending
    // until there is a pane to address.
    let seat = match place.open(&open_order) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };

    // The record goes down before the agent starts, because the successor's
    // brief is composed at start and the ending has to already be in it. Taken
    // back on every failure below: between this line and a confirmed turn, the
    // record is a promise, and a promise wsp failed to keep must not survive
    // as somebody's standing instruction to end a seated custodian's pane.
    store.set_handover(&scope, json!({ "from": me, "to": seat.as_str(), "since": util::now_iso() }));

    let laid = match &brief_at {
        Some(path) => lay_brief(store, &work, &seat, cwd.as_deref(), path),
        None => Laid::Elsewhere,
    };
    let text = handover(&subject, Handover::Custodian, route(how, laid));
    let in_args = how.order_in_args();
    let spawn = agent_commands::Spawn {
        full: false,
        subagents: false,
        name: &subject,
        seat: &seat,
        model: None,
        effort: None,
        order: match in_args {
            true => Some(text.as_str()),
            false => None,
        },
        resume: None,
    };
    let agent = Agent { kind: kind.clone(), name: subject.clone(), args: how.args(&spawn) };

    // Step 2. Started, then heard from: a turn running is the only evidence
    // that the handover was taken, and everything after this point depends on
    // it having been.
    if let Err(e) = start_agent(place, how, &spawn, &agent, &kind, wait) {
        store.clear_handover(&scope);
        eprintln!("wsp: {kind} did not start in {seat}: {e}");
        unreached(how, place, &spawn);
        eprintln!("wsp: nothing moved - the {scope} seat is still yours");
        return 1;
    }
    if !in_args {
        if let Err(e) = confirm_turn(place, how, &spawn, &text, wait) {
            store.clear_handover(&scope);
            eprintln!("wsp: agent started but not working on it: {e}");
            eprintln!("wsp: send the order again with `wsp tell {} -`", seat.as_str());
            unreached(how, place, &spawn);
            eprintln!(
                "wsp: nothing moved - the {scope} seat is still yours, and {} sits idle",
                seat.as_str()
            );
            return 1;
        }
    }

    // Step 3. The slot moves last, now that it is earned. `take` says whom it
    // displaced — which is this pane, by construction — and renames both rooms
    // after the fact.
    let Some(ws_new) = workspace_of(&seat) else {
        // The port gap a first custodian's spawn also hits: the workspace id is
        // herdr's word and the port has none for it. Nothing here may guess it,
        // and the seat must not move onto a record that cannot name its room.
        // The record stays, because the successor did take the handover and its
        // brief does tell it to end this pane once it is the seat.
        eprintln!(
            "wsp: {} took the handover, but its workspace could not be read - \
             the seat has not moved",
            seat.as_str()
        );
        eprintln!("wsp: run `wsp govern {scope}` from {} to finish the move", seat.as_str());
        return 1;
    };
    cmd_govern::take(store, &scope, &ws_new, seat.as_str());

    if args.json() {
        println!(
            "{}",
            json!({
                "rotated": true,
                "scope": scope,
                "successor": seat.as_str(),
                // The same answer the text gives as "your ending is arranged":
                // which pane the successor has been handed the despawn of.
                "predecessor": me,
                "told": true,
            })
        );
    } else {
        println!("{} {}", p.cyan("▣"), p.bold(&format!("{scope} rotated")));
        println!("  {}", p.dim(&format!("successor in {} - its first turn is running", seat.as_str())));
        println!(
            "  {}",
            p.dim("your ending is arranged: the successor ends this pane. Nothing here is left to do")
        );
    }
    0
}

/// The workspace a pane is in, which is the id a slot is recorded against.
///
/// The one herdr-shaped question `spawn` asks outside the port. `place::Seat`
/// is a string wsp is forbidden to parse — the whole point of the handle — and
/// `place::Seated` has no word for a workspace, because starting and stopping
/// work never needed one. A slot does: it is keyed on the workspace for the
/// same reason a claim is, so that an agent cleared and restarted on a new pane
/// is the same custodian rather than a vacancy.
pub(crate) fn workspace_of(seat: &Seat) -> Option<String> {
    crate::herdr::panes()
        .ok()?
        .into_iter()
        .find(|p| p.pane_id == seat.as_str())
        .map(|p| p.workspace_id)
        .filter(|w| !w.is_empty())
}

/// `wsp despawn` — end the agent on a piece of work, and put the work down.
///
/// The other end of [`spawn`], and the reason the port has a `stop` at all: a
/// loop that starts agents one at a time and cannot end one makes despawning
/// part of the loop rather than an edge case.
///
/// # One verb, because the hand procedure was never finished
///
/// Ending an agent took three commands and nothing did all three:
///
///     wsp despawn <id>            ended the agent, released the claim
///     wsp checkout <id> --rm      removed the worktree
///     herdr workspace close <ws>  closed the workspace the pane left behind
///
/// Only the first is a wsp verb anybody reaches for, so the other two were done
/// by whoever noticed — which is why they were not done. What that cost is on
/// robustness-076 and is not small: eighteen worktrees and nineteen workspaces
/// left standing after one overnight batch, every one of them found by
/// accident. `wsp checkout --sweep` cannot help, because it only removes trees
/// whose task is *finished* and everything here sits at `review` by design.
///
/// So this verb does the whole ending. Two of the three are one call already —
/// closing the last pane of a workspace takes the workspace with it, measured
/// against herdr 0.8.0 on 2026-08-19 and argued in `place_herdr`'s module docs —
/// and the third is [`cmd_checkout::discard`], with `--rm`'s refusals and not
/// the sweep's. There is a fourth nobody had counted: a build tree under the
/// state directory is keyed on the workspace, so the workspace going takes its
/// last owner with it, and 9.6G of that residue had accumulated by 2026-08-17.
/// [`crate::cmd_verify::clear_build_key`] takes the one that has just been
/// orphaned.
///
/// **Stop first, release last** — the reverse of the list above, and the
/// argument for it is where the rule about the claim and the seat already
/// lives, in `place.rs`. This is the half of it that is code: the claim is
/// released only once the seat is gone, a seat that was *already* gone counts
/// as gone, and a backend that did not answer is neither. The tree comes after
/// both, because it is the only step that can be done later by hand.
///
/// # What it will not do
///
/// It is aimed by hand at a seat, and it stays that way. Nothing here reaps on
/// idleness: a task at `review` is not evidence its agent has stopped — `review`
/// is where everything sits, because `done` belongs to the person the work is
/// for — and an agent idle for ten minutes may be waiting on a person.
/// Robustness-051 was exactly that, and killing it would have destroyed work to
/// save a directory.
///
/// Every step reports what it *did* rather than what it attempted, including
/// the tree it decided to leave and why. That is the house fault this verb was
/// written against, and a cleanup that prints "removed" over a tree still on
/// disk is worse than one that never ran.
///
/// **It will not show you first, and `-n` is refused rather than ignored.**
/// This verb removes a working tree — the same tree, through the same
/// [`cmd_checkout::discard`] — so it was one of the five that `worklist-050`
/// found parsing `-n`, never reading it, and doing the real thing. Four of the
/// five grew a dry run; this one says it cannot, and the reason is the
/// paragraph above about reporting what was *done*.
///
/// What a despawn does is a run of steps across a live backend, each one
/// decided by the previous one's answer: the claim is released only if the seat
/// closed, the tree is taken only if nobody is standing in it and it holds
/// nothing uncommitted, the build trees go only if herdr says the workspace has
/// actually gone. Every one of those is a question that can only be asked by
/// doing the step before it. A preview would therefore print what it *intends*,
/// which is exactly the sentence this verb was written to stop printing — and
/// it would be wrong on precisely the runs that matter, the ones where a tree
/// is kept because somebody turned out to be in it.
///
/// So the refusal is the honest answer, and it names the read that does work:
/// `wsp checkout <id> --rm -n` answers the tree half, which is the half people
/// are asking about when they type it. The refusal is in `main`, ahead of
/// dispatch, because a word meaning "do not act" that is answered by the verb
/// has already been answered too late.
///
/// No guard on an agent that is busy, and that is a decision rather than an
/// omission. `claim`'s live-holder guard protects you from a *third party* you
/// may not have known was there; this verb is aimed at a seat by somebody who
/// knows what is in it. What ending it costs is the session, not the work — a
/// tree holding anything uncommitted is left exactly where it is, which is the
/// whole of what [`cmd_checkout::discard`] refuses on — and the state it would
/// refuse on is the one a wedged agent reads as, which is when you most want
/// this.
///
/// `--headless` names the backend, the same flag the spawn was placed with, and
/// **wsp does not know which one a seat belongs to** — a claim records a
/// workspace, a binding records a seat id, and neither says what issued it.
/// Asking is the honest version of that gap: the alternative is to try one
/// backend and then the other, which turns "no such seat" into "no such seat
/// anywhere" and makes a stopped agent indistinguishable from a mistyped id.
/// Which record should carry it is the state-model half of the translation
/// layer (the binds note on robustness-061), and is not decided here.
pub fn despawn(store: &Store, args: &Args) -> i32 {
    let keep = args.has("keep-tree");
    let tidy = |seat: &Seat, task: Option<&str>, ws: Option<&str>| swept_up(store, seat, task, ws, keep);
    let pane = cmd_agent::my_pane();
    // What this pane holds the slot of *now*, read here for the same reason the
    // pane is: it is the one thing about the caller that comes out of the
    // environment, and `end_work` is tested without one.
    let governors = store.governors();
    let governs = crate::herdr::Env::read()
        .workspace_id
        .as_deref()
        .and_then(|ws| cmd_govern::governs(&governors, ws, pane.as_deref()));
    let me = Caller { pane: pane.as_deref(), governs: governs.as_deref() };
    end_work(backend(args).as_ref(), store, args, me, &tidy)
}

/// Which pane is running this despawn, and what it holds the seat of.
///
/// Two facts about the caller rather than one, because two of this verb's
/// refusals turn on who is asking: ending the pane you are standing in, and
/// ending the pane you are in the middle of taking a seat from. Both arrive as
/// arguments for the reason on [`end_work`] — a test that had to export
/// `HERDR_PANE_ID` to reach them would be changing a process-wide variable
/// every other test can see.
#[derive(Default, Clone, Copy)]
struct Caller<'a> {
    /// This process's seat, or `None` for a caller standing outside one.
    pane: Option<&'a str>,
    /// The scope whose slot this pane sits in at this moment. During a rotation
    /// this is the fact that changes: `None` until the slot moves, the scope
    /// afterwards.
    governs: Option<&'a str>,
}

/// What the ending took away after the seat itself.
struct Leftovers {
    tree: Tree,
    /// Build trees that were keyed on the workspace the seat took with it.
    builds: Vec<String>,
}

/// The tree and the build trees, once the seat has gone.
///
/// Passed into [`end_work`] as a closure rather than called from inside it, for
/// the reason `me` is passed in below: this is the half of the verb that needs
/// a git repository and a live herdr under it, and a test of the *ordering* —
/// stop, release, then tidy — should not need either. It is also the half that
/// removes directories, and a unit test that reached the real one would be
/// running `git worktree remove` against whatever tree the test runner happens
/// to be standing in.
/// `keep` is `--keep-tree`, and it covers the *checkout* only. A build tree
/// keyed on a workspace that has gone has no owner left to keep it for, so
/// there is nothing for a flag to protect and no flag to remember.
fn swept_up(
    store: &Store,
    seat: &Seat,
    task: Option<&str>,
    workspace: Option<&str>,
    keep: bool,
) -> Leftovers {
    let cwd = std::env::current_dir().unwrap_or_default();
    let here = util::real(&cwd.display().to_string());
    // The seat this despawn has just closed is not somebody standing in the
    // tree. herdr can go on listing a pane for a moment after `pane.close`
    // returns, and reading that back as an occupant is how a cleanup verb comes
    // to refuse on the pane it removed itself.
    let seen: Result<Vec<crate::herdr::Pane>, String> = match crate::herdr::available() {
        // No herdr on this machine is not a pane in the tree: `place_super`
        // runs agents with no terminal at all, and a cleanup that refused
        // without one would remove nothing there, ever.
        false => Ok(Vec::new()),
        // A socket that is there and will not answer is a different fact, and
        // the direction to fail in is the one `sync.rs:41` already argues:
        // silence is not evidence that nobody is standing in the tree.
        true => crate::herdr::panes()
            .map(|ps| ps.into_iter().filter(|p| p.pane_id != seat.as_str()).collect())
            .map_err(|e| format!("herdr did not say who is standing in it: {e}")),
    };
    let standing = |dir: &std::path::Path| -> Option<String> {
        let dir = util::real(&dir.display().to_string());
        if here.starts_with(&dir) {
            return Some("you are standing in it".into());
        }
        let panes = match &seen {
            Ok(panes) => panes,
            Err(why) => return Some(why.clone()),
        };
        panes
            .iter()
            .find(|p| util::real(&p.cwd).starts_with(&dir))
            .map(|p| format!("{} is standing in it", p.pane_id))
    };

    let tree = match task.filter(|_| !keep) {
        Some(t) => cmd_checkout::discard(cmd_checkout::candidates(store, &cwd, t), t, &standing),
        // A seat holding nothing names no tree. There is no path from a seat to
        // a checkout except through the task, and guessing one from the pane's
        // cwd would remove a tree on the strength of where somebody stood.
        None => Tree::Absent,
    };

    // Only once herdr says the workspace has actually gone. A seat with
    // siblings leaves its workspace standing, and a build tree keyed on a live
    // workspace belongs to whoever is still in it.
    let gone = |ws: &str| crate::herdr::workspaces().map(|all| !all.iter().any(|w| w.id == ws));
    let builds = match workspace {
        Some(ws) if gone(ws).unwrap_or(false) => crate::cmd_verify::clear_build_key(store, ws),
        _ => Vec::new(),
    };
    Leftovers { tree, builds }
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
fn end_work(
    place: &dyn Place,
    store: &Store,
    args: &Args,
    me: Caller,
    tidy: &dyn Fn(&Seat, Option<&str>, Option<&str>) -> Leftovers,
) -> i32 {
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
    if me.pane == Some(seat.as_str()) {
        eprintln!("wsp: {seat} is this pane — `wsp release`, then leave");
        return 2;
    }

    // The ending a rotation hands you is owed, but not yet. `rotate` seats you,
    // confirms your first turn, and only then moves the slot — and `confirm_turn`
    // returns when the turn *starts*, so your first turn and its `take` are
    // running at the same time. Ending the predecessor inside that window kills
    // the pane on its way to `take`: the slot never moves, the predecessor is
    // gone, and you are left holding a record naming a pane that no longer
    // exists. **A vacancy produced by the one verb built to prevent one.**
    //
    // A guard rather than a sentence in the brief, which is the whole argument
    // of `core-050`: a step that depends on an agent following prose is a step
    // that half-succeeds. The brief does say to wait; this is what happens when
    // an agent acts on its session-start text without re-reading.
    //
    // Not behind `--force`, and that is deliberate. The seat guard below offers
    // force because what it protects is a thread somebody may knowingly spend;
    // forcing *this* one destroys the rotation in progress, and the message an
    // agent reads here must not name the door that does the damage. It cannot
    // wedge: the slot moving clears it, and where `rotate` could not move the
    // slot itself it already prints the manual `wsp govern <scope>` that does.
    let incoming = cmd_govern::incoming(&store.handovers(), me.pane);
    if cmd_govern::rotation_pending(me.governs, incoming.as_ref()) {
        // Only the pane the record actually names. A successor mid-rotation may
        // still have ordinary agents of its own to end.
        if let Some((scope, from)) = incoming.filter(|(_, from)| from == seat.as_str()) {
            eprintln!("{} {seat} is still the {scope} seat — the rotation has not landed", p.yellow("✗"));
            eprintln!("  {}", p.dim("it is running `wsp govern --rotate`, which moves the slot to you once your first turn is confirmed"));
            eprintln!("  {}", p.dim(&format!("wait, then `wsp despawn --pane {from}` · if it has stopped, take the slot first: wsp govern {scope}")));
            return 1;
        }
    }

    // A governing pane is the one agent that cannot be restarted. Everything
    // else here is replaceable — despawn and respawn is the ordinary way to
    // clear a stuck agent — but a seat's whole value is the thread it has been
    // holding all session, and there is no verb that puts that back.
    //
    // A guard on this verb rather than a gate in front of the agents: nothing
    // under a seat waits on it, and this refusal costs only whoever pointed
    // despawn at the seat. Fail-open by construction — the record names the
    // pane the seat started in, so a seat whose agent has already been replaced
    // stops matching and this says nothing.
    // What this seat is the custodian of, if anything. Read once and used
    // twice: the refusal below, and the brief this seat was started with, which
    // for a custodial spawn is named for the scope rather than for a task.
    let held: Vec<String> = store
        .governors()
        .iter()
        .filter(|(_, rec)| rec.get("pane").and_then(|x| x.as_str()) == Some(seat.as_str()))
        .map(|(proj, _)| proj.clone())
        .collect();
    if !args.has("force") {
        if !held.is_empty() {
            eprintln!("{} {seat} is the seat for {}", p.yellow("✗"), held.join(" "));
            eprintln!("  {}", p.dim("the thread it is holding does not come back — `wsp govern --clear` there first"));
            eprintln!("  {}", p.dim(&format!("wsp despawn --pane {seat} --force   to end it anyway")));
            return 1;
        }
    }

    // Asked before the seat is closed, because after it there is nothing left
    // to ask: the workspace is the id a build tree is keyed on, and a pane that
    // has gone cannot say which one it was in.
    let workspace = workspace_of(&seat);

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

    // An inherited ending is consumed by being done. A rotation leaves a record
    // telling the successor to end exactly this pane; once it has — or once the
    // pane was already gone, which is the same fact to this verb — the record
    // has said its whole say, and leaving it would make every later brief of
    // that pane's successor order an ending that already happened.
    for (scope, rec) in store.handovers() {
        if rec.get("from").and_then(|v| v.as_str()) == Some(seat.as_str()) {
            store.clear_handover(&scope);
        }
    }

    // The brief this seat was started with, if it had one. Removed here because
    // this is the verb that ends a seat, and a brief left behind is a stale
    // answer sitting on disk waiting to be handed to whoever is spawned onto
    // the task next — the file is named for the subject, so the next spawn
    // would read it before writing its own only if the write failed, which is
    // exactly the case where a stale one is worst.
    //
    // Best effort and silent. There is nothing a person does about it, an
    // absent file is the ordinary answer for every kind that never had one, and
    // a line here would be printed on every despawn in the fleet for the
    // benefit of none.
    for subject in task.iter().map(String::as_str).chain(held.iter().map(String::as_str)) {
        let _ = std::fs::remove_file(brief_path(store, subject));
    }

    // Last, and only now: the tree is the one step a person can still do by
    // hand afterwards, so it must not be the step that stops the claim being
    // released. Everything before this line is what `despawn` has always done.
    let Leftovers { tree, builds } = tidy(&seat, task.as_deref(), workspace.as_deref());

    if args.json() {
        println!(
            "{}",
            json!({
                "seat": seat.as_str(),
                "closed": closed,
                "task": task,
                "released": released,
                "tree": match &tree {
                    Tree::Absent => serde_json::Value::Null,
                    Tree::Removed { path, kept } =>
                        json!({ "removed": true, "path": path, "branch_kept": kept.is_some(), "branch": kept }),
                    Tree::Kept { path, why } =>
                        json!({ "removed": false, "path": path, "why": why }),
                },
                "builds": builds,
            })
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
        match &tree {
            // Nothing at all, rather than "no tree": the common ending has no
            // checkout in it and a line saying so on every despawn is a line
            // every reader learns to skip, including on the despawns where the
            // next two matter.
            Tree::Absent => {}
            Tree::Removed { path, kept } => {
                println!("  {}", p.dim(&format!("removed {path}")));
                // The branch as the tree named it, not as the task is named
                // now: a renumbered task's work is on a branch of the old id,
                // and this line is the only thing that says where it went.
                if let Some(branch) = kept {
                    println!("  {}", p.yellow(&format!("branch {branch} kept — it has commits the trunk has not")));
                }
            }
            // Not dim. This is the one line here that is somebody's to act on,
            // and the leak this verb exists to stop is exactly a cleanup step
            // that reported success it had not achieved.
            Tree::Kept { path, why } => println!("  {}", p.yellow(&format!("kept {path} — {why}"))),
        }
        if !builds.is_empty() {
            println!("  {}", p.dim(&format!("removed {} build tree(s) keyed on the workspace", builds.len())));
        }
    }
    0
}

/// `wsp claim <task> --pane <pane>`, called rather than shelled out to.
pub(crate) fn cmd_agent_claim(store: &Store, task: &str, flags: &[(&str, &str)]) -> i32 {
    crate::cmd_agent::claim(store, &Args::synth("claim", &[task], flags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Project, Task};
    use crate::place::Delivery;

    fn seat(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-place-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    /// The store is always reachable, and it is the whole of what a row with
    /// nothing written on it gets.
    ///
    /// `~/wsp/tasks/<id>.md` is the one prompt `core-041`'s replay of `ui-007`
    /// had left, and `render-033` stopped on the same path. Granted whole
    /// rather than as `tasks/`, on `core-027` d2's rule: allowing one directory
    /// moves the same stall one command later, to a handbook or a worklist.
    #[test]
    fn every_seat_may_read_the_store_it_is_recorded_in() {
        let store = seat("reach-store");
        assert_eq!(
            reach(&store, None, None),
            vec![store.root.clone()],
            "wsp knows where it keeps its own record; nothing here is guessed"
        );
    }

    /// A ref outside the tree becomes the directory it sits in, and a ref
    /// inside it becomes nothing at all.
    ///
    /// `refs` is how a row says *the work is over there* — a spec, a design
    /// note, a sibling repository — and it is per-row and finite, which is the
    /// whole reason it is preferred to a standing grant. A relative ref names
    /// something the agent's own worktree already holds, so granting a reach to
    /// it would be a rule about the agent's own directory in a policy whose
    /// only subject is leaving it.
    #[test]
    fn a_ref_that_points_out_of_the_tree_is_the_only_kind_that_earns_a_reach() {
        let store = seat("reach-refs");
        let tree = std::env::temp_dir().join("wsp-reach-refs-tree/.worktrees/ui-009");
        let spec = std::env::temp_dir().join("wsp-reach-refs-spec");
        let mut t = Task::new("read the spec", "ui-009");
        t.refs = vec!["src/model.rs".into(), spec.join("surface.md").display().to_string()];
        store.save_task(&t).unwrap();

        let out = reach(&store, Some("ui-009"), Some(&tree.display().to_string()));
        assert!(
            out.contains(&spec),
            "the directory the ref sits in, because that is opencode's unit — {out:?}"
        );
        assert!(
            !out.iter().any(|d| d.starts_with(&tree)),
            "a relative ref is a file the worktree already holds — {out:?}"
        );
    }

    /// Nothing above the agent's own tree is reachable, however it was asked
    /// for — and a written-down `ref` is the way it would be asked for.
    ///
    /// Three agents asked for `~/claude/wsp/*` and none of them needed it: two
    /// were reaching for the handbook's *"`README.md` at the root"* in the main
    /// checkout while holding a copy in their own tree, and one wanted `git
    /// worktree add`, which a linked worktree runs from where it stands. But
    /// the reason it is refused *mechanically* rather than merely left out is
    /// stronger than any of that: a tree lives at `<checkout>/.worktrees/<task>`
    /// and an `external_directory` rule is a path rule and not an operation
    /// rule, so under `core-041`'s `edit: allow` a grant on any directory above
    /// it is write access to every sibling worktree — every other agent's
    /// uncommitted work — as well as to the shared checkout. `ui-001`'s agent,
    /// allowed the parent by hand, edited `src/cmd_task.rs` in the main
    /// checkout and had to `git apply -R` its way back out.
    #[test]
    fn nothing_above_the_agents_own_tree_is_reachable_however_it_was_asked_for() {
        let store = seat("reach-parent");
        let checkout = std::env::temp_dir().join("wsp-reach-parent-checkout");
        let tree = checkout.join(".worktrees/ui-010");
        let mut t = Task::new("touch the trunk", "ui-010");
        // Written down as a ref, which is the only route it could arrive by and
        // the one that looks most like permission. It is still refused.
        t.refs = vec![checkout.join("README.md").display().to_string()];
        store.save_task(&t).unwrap();

        let out = reach(&store, Some("ui-010"), Some(&tree.display().to_string()));
        assert_eq!(
            out,
            vec![store.root.clone()],
            "a rule on the parent covers every sibling tree under it — {out:?}"
        );
    }

    /// One rule where one rule answers, because opencode's `*` spans separators.
    ///
    /// Measured in `render-014`'s own log: a request for
    /// `/var/…/T/opencode/render-014-before/*` matched a rule written as
    /// `/var/…/T/opencode/*`. So a directory under one already named is noise,
    /// and a policy nobody can read is how a policy stops being read.
    #[test]
    fn a_directory_already_covered_by_one_above_it_is_not_written_twice() {
        let store = seat("reach-nested");
        let mut t = Task::new("two paths, one place", "ui-011");
        // Both under the store, which every seat can already reach.
        t.refs = vec![
            store.root.join("notes/a.md").display().to_string(),
            store.root.join("notes/deeper/b.md").display().to_string(),
        ];
        store.save_task(&t).unwrap();

        let out = reach(&store, Some("ui-011"), Some("/nowhere/near"));
        assert_eq!(
            out,
            vec![store.root.clone()],
            "the store already answers for everything under it — {out:?}"
        );
    }

    /// Every way the tier can be wrong, and the fact that none of them costs a
    /// workspace.
    ///
    /// The three refusals are three different mistakes. A typo is the one the
    /// flag exists to catch early, because the alternative is an agent that
    /// starts, is refused its model by Claude Code in a pane nobody is watching
    /// and is reported here as never having become ready. A tier without
    /// `--agent` is a sentence about an agent that is never started, and
    /// dropping it silently is exactly the failure mode of the `--effort`
    /// warning this validation was written against. A tier on a kind wsp has no
    /// vocabulary for would be passed on by nobody: `Plain::args` sends
    /// nothing, so accepting it would be wsp saying it started `codex` on haiku
    /// and starting it on whatever codex defaults to.
    #[test]
    fn a_mistyped_tier_is_refused_before_anything_is_opened() {
        let (g, i) = (BTreeMap::new(), Index::new(vec![]));
        let nowhere = Scope { governors: &g, index: &i, list: None, project: None };
        let none =
            |flags: &[(&str, &str)]| tier(&Args::synth("spawn", &["t-1"], flags), "claude", true, &nowhere);

        assert_eq!(none(&[]).unwrap(), (None, None), "no flag must send no argument at all");
        assert_eq!(
            none(&[("model", "opus[1m]"), ("effort", "low")]).unwrap(),
            (Some("opus[1m]".into()), Some("low".into())),
            "what was typed is what is passed on"
        );

        let err = none(&[("model", "opsu")]).unwrap_err();
        assert!(err.contains("opsu") && err.contains("sonnet"), "the typo and the list: {err}");
        let err = none(&[("model", "claude-opus-5")]).unwrap_err();
        assert!(err.contains("claude-opus-5"), "a full name is not an alias: {err}");
        let err = none(&[("effort", "hi")]).unwrap_err();
        assert!(err.contains("xhigh"), "an ignored effort is a session that lied: {err}");

        let err = tier(&Args::synth("spawn", &["t-1"], &[("model", "opus")]), "claude", false, &nowhere)
            .unwrap_err();
        assert!(err.contains("--agent"), "a tier with nothing to start it: {err}");

        let err = tier(&Args::synth("spawn", &["t-1"], &[("model", "opus")]), "codex", true, &nowhere)
            .unwrap_err();
        assert!(err.contains("claude"), "wsp does not know how codex spells a model: {err}");
    }

    /// A tier nobody can leave alone is refused for the spawn that would leave
    /// it alone, and allowed for the one that would not.
    ///
    /// Both halves, because a refusal with no way through is a dead end and the
    /// way through is the half that will be doubted. The measurement is on
    /// `Kind::unattended`; what is asserted here is only that a background spawn
    /// reads it and a focused one does not, and that the sentence a person gets
    /// names the tier they typed rather than the rule that caught it.
    #[test]
    fn a_tier_that_cannot_be_left_alone_is_refused_unless_somebody_is_going_to_the_pane() {
        let (g, i) = (BTreeMap::new(), Index::new(vec![]));
        let nowhere = Scope { governors: &g, index: &i, list: None, project: None };
        let background = |m: &str| {
            tier(&Args::synth("spawn", &["t-1"], &[("model", m)]), "claude", true, &nowhere)
        };

        let err = background("haiku").unwrap_err();
        assert!(err.contains("haiku"), "the tier that was typed: {err}");
        assert!(err.contains("manual mode"), "and what is actually missing on it: {err}");
        assert!(err.contains("--focus") && err.contains("sonnet"), "and both ways on: {err}");

        assert!(background("haiku[1m]").is_err(), "a bigger window is the same tier");
        assert!(background("sonnet").is_ok(), "the tiers that hold their own are untouched");
        assert!(background("opus[1m]").is_ok(), "including with the suffix on");

        let flags = [("model", "haiku"), ("focus", "true")];
        let focused = tier(&Args::synth("spawn", &["t-1"], &flags), "claude", true, &nowhere);
        assert!(focused.is_ok(), "somebody is going to the pane: {focused:?}");
    }

    /// A tree with a seat in the middle of it: `wsp` is governed, `robustness`
    /// and `data` are under it and empty, `tooling` is above it.
    fn governed_tree() -> (BTreeMap<String, Value>, Index) {
        let mut wsp = Project::new("wsp");
        wsp.parent = Some("tooling".into());
        let mut rob = Project::new("robustness");
        rob.parent = Some("wsp".into());
        let seated = [(
            "wsp".to_string(),
            json!({ "workspace": "w9", "host": util::hostname() }),
        )]
        .into_iter()
        .collect();
        (seated, Index::new(vec![Project::new("tooling"), wsp, rob]))
    }

    /// The default, and the sentence it exists for: a spawn onto work with a
    /// seat above it starts at sonnet and medium effort with nothing typed.
    ///
    /// Both shapes of "above", because the trigger is `seat_for`'s walk and not
    /// a lookup: `wsp` has the seat, so work in `wsp` finds it at its own level
    /// and work in `robustness` finds it one step up — the same walk a hand
    /// raised in `robustness` takes to reach the same agent.
    #[test]
    fn governed_work_with_no_tier_stated_starts_at_sonnet_and_medium() {
        let (g, i) = governed_tree();
        let at = |project| {
            let scope = Scope { governors: &g, index: &i, list: None, project: Some(project) };
            tier(&Args::synth("spawn", &["t-1"], &[("agent", "true")]), "claude", true, &scope)
        };

        let both = (Some("sonnet".to_string()), Some("medium".to_string()));
        assert_eq!(at("wsp").unwrap(), both, "the seat is on this very project");
        assert_eq!(at("robustness").unwrap(), both, "and one step up is still above it");
        assert_eq!(
            at("tooling").unwrap(),
            (None, None),
            "a seat answers for what is under it and not for what is over it"
        );
    }

    /// And the seat itself is exempt: `--govern` onto the same scope keeps
    /// whatever the settings file says.
    ///
    /// d5 measured the residue of a cheap tier and it was precisely the seat's
    /// own work — no review note, and the change description left in an
    /// overview that is then injected into every later spawn on the task. The
    /// role that sequences, reviews and writes the notes is the worst place in
    /// the fleet to route down, so the flag that takes it is the one flag that
    /// turns this off. Workers cheap, seats on the settings tier.
    #[test]
    fn a_govern_spawn_onto_the_same_scope_keeps_the_settings_tier() {
        let (g, i) = governed_tree();
        let scope = Scope { governors: &g, index: &i, list: None, project: Some("robustness") };
        let flags = [("govern", "true")];
        let out = tier(&Args::synth("spawn", &["robustness"], &flags), "claude", true, &scope);
        assert_eq!(
            out.unwrap(),
            (None, None),
            "a seat spawned under a seat is still a seat"
        );
    }

    /// One word about the tier suppresses the whole default rather than filling
    /// in the other half of it.
    ///
    /// `--effort high` on governed work is a person saying *this one is hard*,
    /// and answering it with `sonnet at high effort` would be wsp finishing a
    /// sentence somebody else started — a spawn that ran at a tier its spawner
    /// never chose, which is the failure the printing is here to prevent and
    /// not one to reintroduce from the other end.
    #[test]
    fn one_word_about_the_tier_suppresses_the_default_rather_than_completing_it() {
        let (g, i) = governed_tree();
        let scope = Scope { governors: &g, index: &i, list: None, project: Some("robustness") };
        let stated = |flags: &[(&str, &str)]| {
            tier(&Args::synth("spawn", &["t-1"], flags), "claude", true, &scope).unwrap()
        };

        assert_eq!(
            stated(&[("effort", "high")]),
            (None, Some("high".into())),
            "the model is left to the settings file, not defaulted to sonnet"
        );
        assert_eq!(
            stated(&[("model", "opus[1m]")]),
            (Some("opus[1m]".into()), None),
            "and the effort is left alone, not defaulted to medium"
        );
    }

    /// Ungoverned work spawns exactly as it did before this existed: no tier on
    /// the command line, nothing on the claim, nothing printed.
    ///
    /// Three ways to be ungoverned and all of them are the ordinary state — no
    /// seat anywhere, no agent to start one for, and a kind that has no
    /// vocabulary for these words. The last is a decline and not a refusal: a
    /// tier nobody typed may never be the reason a spawn fails.
    #[test]
    fn work_with_no_seat_above_it_spawns_exactly_as_it_did_before() {
        let (g, i) = governed_tree();
        let none = BTreeMap::new();
        let flags = [("agent", "true")];
        let out = |governors, kind, agent, project| {
            let scope = Scope { governors, index: &i, list: None, project };
            tier(&Args::synth("spawn", &["t-1"], &flags), kind, agent, &scope).unwrap()
        };

        assert_eq!(out(&none, "claude", true, Some("wsp")), (None, None), "nobody is governing");
        assert_eq!(out(&g, "claude", true, None), (None, None), "work in no project at all");
        assert_eq!(
            out(&g, "claude", false, Some("wsp")),
            (None, None),
            "a terminal with no agent in it has no tier to run at"
        );
        assert_eq!(
            out(&g, "codex", true, Some("wsp")),
            (None, None),
            "and a kind wsp cannot say `sonnet` to is left alone rather than refused"
        );
    }

    /// And the refusal happens before `place.open`, which is the whole reason
    /// it is read at the top of `place_work` rather than where the agent starts.
    ///
    /// A tier checked after the workspace exists has already cost a workspace, a
    /// claim and a worktree, and the person now has to `wsp despawn` before they
    /// can retype the word. `Opens` panics in `start`, so this also asserts
    /// nothing tried to run an agent at a tier that was refused.
    #[test]
    fn a_refused_tier_leaves_no_workspace_behind() {
        let _guard = no_backend();
        let store = seat("tier");
        store.save_project(&Project::new("robustness")).unwrap();

        let place = Opens(std::cell::RefCell::new(Vec::new()));
        let flags = [("project", "robustness"), ("agent", "true"), ("model", "opsu")];
        let code = place_work(&place, &store, &Args::synth("spawn", &[], &flags));

        assert_eq!(code, 2, "a mistyped tier is a usage error");
        assert!(place.0.borrow().is_empty(), "a workspace was opened for a spawn that cannot start");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// A worklist is something to **govern** and not a place to work, and that
    /// is the whole of what `--govern` had to learn.
    ///
    /// `governors.json` is keyed on a scope now — a project id or a worklist
    /// slug — so `wsp spawn -p <slug> --govern` is the same command pointed at
    /// the other half of one key space, and it needs no flag of its own. What
    /// it must *not* do is write the slug into `project`: that field is what
    /// the pane is standing in, it fills `WSP_PROJECT` and resolves the cwd,
    /// and a custodian whose brief was about a project that does not exist is
    /// the near miss the guard above this refuses to make.
    #[test]
    fn a_worklist_is_something_to_govern_rather_than_a_place_to_work() {
        use crate::model::Worklist;
        let store = seat("govern-list");
        store.save_worklist(&Worklist::new("batch", "Overnight batch")).unwrap();
        store.save_project(&Project::new("robustness")).unwrap();
        let index = Index::new(store.projects());

        let named = |flags: &[(&str, &str)]| resolve(&store, &Args::synth("spawn", &[], flags), &index);
        let w = named(&[("project", "batch"), ("govern", "true")]).expect("a list is a scope");
        assert_eq!(w.list.as_deref(), Some("batch"));
        assert_eq!(w.project, None, "and not a project to stand in");

        // Bare, it is the same answer: the slug is a name `--govern` knows.
        let w = resolve(&store, &Args::synth("spawn", &["batch"], &[("govern", "true")]), &index)
            .expect("named without -p");
        assert_eq!(w.list.as_deref(), Some("batch"));

        // Without `--govern` it is not a spawn target at all, and the message
        // says so in the words of the thing that was asked for.
        let err = named(&[("project", "batch")]).err().expect("a list is not a place to work");
        assert!(err.contains("batch"), "{err}");
        // A project still resolves exactly as it did, `--govern` or not.
        for flags in [vec![("project", "robustness")], vec![("project", "robustness"), ("govern", "true")]] {
            let w = named(&flags).unwrap();
            assert_eq!(w.project.as_deref(), Some("robustness"));
            assert_eq!(w.list, None);
        }

        let _ = std::fs::remove_dir_all(&store.root);
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
            list: None,
        };
        let o = order(&work, Some("~/claude/wsp"), Some("mb2"), false, None, false);
        assert_eq!(o.label, "robustness/004 · a title");
        assert_eq!(o.cwd.as_deref(), Some("~/claude/wsp"), "expanded by the backend, not here");
        assert_eq!(o.on.as_deref(), Some("mb2"));
        // `show` is the one thing here that is scaffolding rather than shape:
        // until `arrange` has an implementor there is no spec to declare focus
        // into, so this is where `--focus` has to be said. See `Order::show`.
        assert!(!o.show, "--focus has nowhere else to be said yet");
        assert_eq!(o.env.get("WSP_TASK").map(String::as_str), Some("t-260817-004"));
        assert_eq!(o.env.get("WSP_PROJECT").map(String::as_str), Some("robustness"));

        // A project spawn has no task, and says so by absence rather than by an
        // empty string somebody downstream has to test for.
        let proj =
            Work { task: None, project: Some("robustness".into()), label: "robustness".into(), list: None };
        let o = order(&proj, None, None, true, None, false);
        assert!(o.env.get("WSP_TASK").is_none());
        assert!(o.on.is_none());
        assert!(o.show);
    }

    /// The kind's own configuration is part of the order, and it is composed
    /// where the `WSP_*` set is composed rather than beside it.
    ///
    /// `core-038`: it was composed at the call site, in `spawn` and nowhere
    /// else, and `resume` built the same order out of the same parts and took
    /// only the first of them — so a resumed opencode ran with no permission
    /// policy and `core-020` d1's brake could never fire. What this asserts is
    /// the shape that makes that unwriteable: a caller says who is going to sit
    /// in the seat, and one function answers what that means.
    #[test]
    fn a_seat_opened_for_an_agent_carries_the_kinds_own_configuration() {
        let work = Work {
            task: Some("oc-001".into()),
            project: Some("core".into()),
            label: "core/001 · a title".into(),
            list: None,
        };
        let brief = std::path::PathBuf::from("/tmp/briefs/oc-001.md");
        let o = order(
            &work,
            None,
            None,
            false,
            Some(Occupant { kind: "opencode", brief: Some(&brief), reach: &[] }),
            false,
        );
        let cfg = o.env.get("OPENCODE_CONFIG_CONTENT").expect("opencode was given no config");
        assert!(cfg.contains("permission"), "the brake is what the config is for: {cfg}");
        assert!(cfg.contains("/tmp/briefs/oc-001.md"), "the brief is named in it: {cfg}");
        assert_eq!(o.env.get("WSP_TASK").map(String::as_str), Some("oc-001"),
            "the kind's half is added to what every seat gets, not instead of it");

        // A seat with nothing starting in it is a terminal in the right tree,
        // and configuring a runtime nobody is launching would be a variable in
        // a shell somebody else is about to type in.
        let bare = order(&work, None, None, false, None, false);
        assert!(bare.env.get("OPENCODE_CONFIG_CONTENT").is_none());

        // And every kind that needs nothing gets byte-for-byte what it got
        // before the channel existed — asserted as the kind's own contribution.
        //
        // `claude.env == bare.env` stood here and is not the claim it reads as:
        // `seat_env` opens with `place::shed_env`, and comparing two of those
        // is comparing two readings of the live process environment taken at
        // different instants. `place::shed_keys` carries why, and what it cost
        // (`robustness-099`). Not fixed by taking `util::env_lock` instead: the
        // neighbours are right to set what they read, and a claim about one
        // kind should not need the whole suite to hold still.
        assert!(
            agent_commands::of("claude").env(Some(&brief), &[]).is_empty(),
            "a kind that needs no configuring is unchanged"
        );
        // …and the seat it is composed into is an ordinary one, which is the
        // other half of `unchanged` and the half `seat_env` could break.
        let claude = order(&work, None, None, false, Some(Occupant { kind: "claude", brief: Some(&brief), reach: &[] }), false);
        assert_eq!(claude.env.get("WSP_TASK").map(String::as_str), Some("oc-001"));
        assert!(claude.env.get("OPENCODE_CONFIG_CONTENT").is_none(), "one kind's spelling reached another's seat");
    }

    /// The other half of the same order: an agent spawned from inside an agent
    /// is a new session and inherits none of the spawning session's identity.
    ///
    /// Left in, `CLAUDE_CODE_CHILD_SESSION` tells the child it is somebody's
    /// sub-session and it saves no transcript — so it works, looks right, and
    /// leaves no record, which is worst exactly when somebody is measuring. The
    /// argument and the measurement are on `place::CHILD_MARKER`; what this
    /// asserts is that the strip is in the order rather than in a `env -u`
    /// incantation the caller has to remember.
    #[test]
    fn a_spawned_agent_is_not_handed_the_spawning_session() {
        let work = Work { task: None, project: None, label: "probe".into(), list: None };
        let o = order(&work, None, None, false, None, false);
        assert_eq!(
            o.env.get(crate::place::CHILD_MARKER).map(String::as_str),
            Some(""),
            "the spawned agent will save no transcript and say so in one truncated line"
        );
        for (k, v) in &o.env {
            assert!(
                !crate::place::shed(k) || v.is_empty(),
                "{k}={v} is the caller's session, handed to the seat"
            );
        }
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
        let spawned = work_order("t-260815-033", Handover::Spawned);
        assert!(spawned.contains("t-260815-033"));
        assert!(!spawned.contains("wsp brief"), "the hook has already injected it: {spawned}");

        let running = work_order("t-260815-033", Handover::Running);
        assert!(running.contains("t-260815-033"));
        assert!(running.contains("wsp brief --session"), "the whole payload in one call: {running}");
    }

    /// The third case, and it is a different job rather than a different route
    /// to the same one.
    ///
    /// What it must not say is the thing every other work order says: pick up
    /// this task and finish it. A custodian that claims work is the failure the
    /// position was built out of — the seat on the night this came from
    /// borrowed robustness-022 to have somewhere to stand, and every surface in
    /// wsp then described it as an agent working that task.
    #[test]
    fn a_custodian_is_told_the_job_rather_than_handed_a_task() {
        let text = work_order("robustness", Handover::Custodian);
        assert!(text.contains("custodian of the robustness project"), "{text}");
        assert!(!text.starts_with("You have been claimed"), "that is the other job: {text}");
        assert!(text.contains("should not claim"), "and it has to be said: {text}");
        // The four things the seat actually did, and the one it must not
        // become. A gate is the failure mode with the widest blast radius —
        // every agent under it waiting on a round-trip — so it is named.
        for owed in ["sequence", "direction", "review", "record", "authorise"] {
            assert!(text.contains(owed), "the work order drops `{owed}`: {text}");
        }
        // No fetch. `--govern` records the slot before the agent starts, so its
        // `SessionStart` hook has already run `wsp brief` with the slot in
        // place — asking again at request 1 is a whole context re-read.
        assert!(!text.contains("wsp brief"), "the hook has already injected it: {text}");
        // And the rotation (`core-049`, one verb since `core-050`). The
        // command is spelled out, because a sentence that says "rotate" without
        // the command is one an agent at 3am improvises around; and the
        // three-step composition it replaced is gone, not standing beside it —
        // its third step was the one that could be skipped.
        for owed in ["worklist go", "govern robustness --rotate", "still the seat"] {
            assert!(text.contains(owed), "the work order drops the rotation step `{owed}`: {text}");
        }
        assert!(!text.contains("--govern"), "the old composition, still in prose beside the verb: {text}");
        assert!(!text.contains("end your session"), "an ending the agent must remember is an ending that gets skipped: {text}");
    }

    /// The seat a custodian runs in carries `WSP_TERSE=1`, because a
    /// coordinating agent re-reads `brief` and `wip` several times an hour for
    /// a whole run and the blocks `--terse` drops are paid for by every one of
    /// those readings (`core-049`). A worker's seat sets nothing: the variable
    /// is the custodial spelling, not a default.
    #[test]
    fn a_custodians_seat_reads_terse_and_a_workers_does_not() {
        let proj = Work {
            task: None,
            project: Some("robustness".into()),
            label: "robustness".into(),
            list: None,
        };
        let plain = order(&proj, None, None, false, None, false);
        assert!(
            plain.env.get("WSP_TERSE").is_none(),
            "a bare project seat is not terse: {plain:?}"
        );
        assert!(
            plain.env.get("WSP_TASK").is_none() && !plain.env.contains_key("WSP_TERSE"),
            "nothing but the custodial half may turn it on"
        );

        let governing = order(&proj, None, None, false, None, true);
        assert_eq!(
            governing.env.get("WSP_TERSE").map(String::as_str),
            Some("1")
        );

        // Same for a seat on a worklist — the scope a per-batch rotation is
        // actually run on — and the session payload is unaffected either way:
        // that protection lives in `Depth::of`, which puts `--session` above
        // this variable, and is asserted there rather than duplicated here.
        let list = Work {
            task: None,
            project: None,
            label: "governor · tuning".into(),
            list: Some("tuning".into()),
        };
        let on_list = order(&list, None, None, false, None, true);
        assert_eq!(
            on_list.env.get("WSP_TERSE").map(String::as_str),
            Some("1")
        );
        assert!(
            on_list.env.get("WSP_PROJECT").is_none(),
            "a worklist is not a place to stand"
        );
    }

    /// A kind with no session hook says the same sentence every other spawn
    /// says, because by the time it is said the brief is already in its
    /// context.
    ///
    /// This is `core-032`. Before it, an unbriefed kind got `Handover::Running`
    /// — *"please run `wsp brief --session`"* — which is a round-trip at
    /// request 1, and under `core-020` d2's brake it is also the agent's first
    /// `bash`: driving d1 showed every opencode spawn stopping there, on the
    /// one command that tells it what it is for.
    ///
    /// The brief is not *in* this string, which is d7's correction and the
    /// whole of what changed after the revert: it is in a file the kind's own
    /// runtime loads. So what is asserted here is that the sentence stops
    /// asking, and the sentence is the one Claude Code already gets.
    #[test]
    fn a_kind_that_arrives_holding_its_brief_is_not_sent_for_it() {
        let text = handover("t-260815-033", Handover::Spawned, Route::Inline);
        assert!(!text.contains("wsp brief"), "sent to fetch what it already holds: {text}");
        assert!(text.contains("Your brief is already above"), "{text}");
        assert_eq!(
            text,
            handover("t-260815-033", Handover::Spawned, Route::Hook),
            "two ways of arriving briefed, one sentence"
        );
    }

    /// And a custodian says the same, because its sentence makes the same
    /// claim about what is above it.
    #[test]
    fn a_custodian_that_arrives_holding_its_brief_is_not_sent_for_it_either() {
        let text = handover("robustness", Handover::Custodian, Route::Inline);
        assert!(text.contains("custodian of the robustness project"), "{text}");
        assert!(!text.contains("wsp brief"), "{text}");
    }

    /// Which route each kind is on, and it is a fact about where a brief can be
    /// put rather than about how the order travels.
    ///
    /// `claude` arrives briefed from its `SessionStart` hook and must not be
    /// given it twice — `core-032` d2. `opencode` has an `instructions` key
    /// that loads a file wsp names, driven in d7. Every other kind has neither,
    /// so it is told to fetch.
    ///
    /// **`Route::Inline` is not `order_in_args`, and asserting that is the
    /// point of this test.** They were the same predicate for one commit: the
    /// brief went into a `--prompt` argv element, herdr refuses `agent.start`
    /// args holding any control character, and three spawns of three were
    /// refused with `invalid_agent_argument`.
    #[test]
    fn a_kind_is_given_a_brief_where_its_own_runtime_will_read_one() {
        let laid = Laid::Written;
        assert!(route(agent_commands::of("claude"), laid) == Route::Hook);
        assert!(route(agent_commands::of("opencode"), laid) == Route::Inline);
        assert!(route(agent_commands::of("codex"), laid) == Route::Fetch);
    }

    /// A brief that could not be written is a brief that is not there, and the
    /// order must say so.
    ///
    /// The failure this guards is silent in every layer beneath it: opencode
    /// ignores an `instructions` path that does not exist without a word, so an
    /// agent whose file failed to write starts perfectly well and is told its
    /// brief is above it when nothing is. That is `core-027`'s failure exactly
    /// — an order that reads as a poor model rather than a wrong order —
    /// arriving through the new carrier.
    #[test]
    fn a_brief_that_was_not_written_is_not_claimed_to_be_there() {
        let text = handover("t-260815-033", Handover::Spawned, route(agent_commands::of("opencode"), Laid::Failed));
        assert_eq!(text, work_order("t-260815-033", Handover::Running));
        assert!(text.contains("wsp brief --session"), "{text}");
    }

    /// A kind that has to fetch gets the wording that already existed for an
    /// agent whose context does not hold its brief, rather than a fourth
    /// sentence.
    #[test]
    fn a_kind_that_cannot_be_handed_a_brief_is_told_to_fetch_one() {
        let text = handover("t-260815-033", Handover::Spawned, Route::Fetch);
        assert_eq!(text, work_order("t-260815-033", Handover::Running));
        assert!(text.contains("wsp brief --session"), "{text}");
    }

    /// The work order is ASCII, and it is not a style rule.
    ///
    /// robustness-035: a spawned agent sat with its work order typed into the
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
        for how in [Handover::Spawned, Handover::Running, Handover::Custodian] {
            let text = work_order("t-260815-033", how);
            assert!(
                text.is_ascii(),
                "non-ASCII in a work order, which is what t-260817-004 was: {text}"
            );
        }
    }

    /// A backend reading off a script, one answer per poll, holding the last one
    /// for ever after.
    ///
    /// The script is the whole point: what killed robustness-041 was a *sequence*
    /// of readings rather than any single one, and a fake that can only be put
    /// in a state cannot express "empty, and then not".
    ///
    /// It holds no clock. Time is [`util::Dial`]'s, wound by the wait's own
    /// `rest` and by nothing else — which is the honest model, because waiting
    /// is the only thing in a poll loop that takes any. [`STEP`] is what one
    /// poll costs.
    struct Reads {
        script: std::cell::RefCell<std::collections::VecDeque<crate::place::Result<State>>>,
        last: std::cell::RefCell<crate::place::Result<State>>,
    }

    /// What one poll costs on the tests' clock.
    const STEP: Duration = Duration::from_millis(1);

    impl Reads {
        fn of(script: Vec<crate::place::Result<State>>) -> Reads {
            Reads {
                last: std::cell::RefCell::new(
                    script.last().cloned().unwrap_or(Ok(State::Unknown)),
                ),
                script: std::cell::RefCell::new(script.into()),
            }
        }
    }

    impl Place for Reads {
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            match self.script.borrow_mut().pop_front() {
                Some(s) => s,
                None => self.last.borrow().clone(),
            }
        }
        fn open(&self, _: &Order) -> crate::place::Result<Seat> {
            panic!("waiting does not open seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            panic!("waiting does not start agents")
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("waiting does not talk to agents")
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("waiting does not end seats")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("waiting is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("waiting does not subscribe")
        }
        fn here(&self) -> Option<Seat> {
            panic!("waiting is about a seat it was handed")
        }
    }

    /// A kind with a fixed answer about whether its agent is alive, which counts
    /// how often it was asked.
    ///
    /// Counted because the cost of asking is a subprocess: the rule is once per
    /// `gone` and not once per poll, and a rule about frequency is not checked
    /// by a test that only looks at the answer.
    struct Says(Option<bool>, std::cell::Cell<u32>);

    impl agent_commands::Kind for Says {
        fn running(&self, _: &agent_commands::Spawn) -> Option<bool> {
            self.1.set(self.1.get() + 1);
            self.0
        }
        fn args(&self, _: &agent_commands::Spawn) -> Vec<String> {
            Vec::new()
        }
        fn tier(&self, _: Option<&str>, _: Option<&str>) -> std::result::Result<(), String> {
            Ok(())
        }
        fn address(
            &self,
            _: &dyn Place,
            _: &agent_commands::Spawn,
        ) -> Option<agent_commands::Address> {
            None
        }
        fn tell(&self, _: &dyn Place, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("readiness does not deliver work orders")
        }
        fn ran(&self, _: &str, _: &str, _: i64) -> Option<agent_commands::Ran> {
            None
        }
    }

    /// A grace of thirty polls and a deadline of two thousand, on a clock that
    /// only moves when the wait rests. Nothing here sleeps.
    const GRACE: Duration = Duration::from_millis(30);
    const READY: Duration = Duration::from_millis(2_000);
    /// Ten polls to start a turn in, and two presses. Small because the clock is
    /// the test's: what these buy is arithmetic a reader can check in their
    /// head, so a bound below is a statement about the loop rather than about
    /// the machine it ran on.
    const TAKEN: Duration = Duration::from_millis(10);
    const PRESSES: u32 = 2;

    fn waiting_on(
        place: &Reads,
        how: &dyn agent_commands::Kind,
        seat: &Seat,
        clock: &util::Dial,
    ) -> Result<(), String> {
        let spawn =
            agent_commands::Spawn { full: false, subagents: false, name: "t-260817-010", seat, model: None, effort: None, order: None, resume: None };
        // The retry fields come from `retrying` because this test is about a
        // single attempt: a literal here would have to be updated every time the
        // retry's numbers move, for a wait that never reads them.
        let wait = Patience { ready: READY, taken: TAKEN, nudges: PRESSES, poll: STEP, gone: GRACE, clock, ..retrying(clock) };
        wait_ready(place, how, &spawn, "claude", &wait)
    }

    /// A backend that can be started into more than once, counting the starts.
    ///
    /// The states come off [`Reads`]' single script rather than one per attempt,
    /// which is deliberate: what a retry is has to be readable as a *sequence*
    /// over one seat — died, emptied, started, ready — and a fake that resets
    /// its script per attempt would let a test pass that only ever saw the
    /// first.
    ///
    /// `tell` still panics, and that is an assertion rather than a stub: nothing
    /// in the retry path may send a work order, because a work order that may
    /// already be sitting in a composer is the one thing this must not
    /// duplicate. Every test below would fail loudly if it did.
    struct Restarts {
        reads: Reads,
        starts: std::cell::Cell<u32>,
    }

    impl Restarts {
        fn of(script: Vec<crate::place::Result<State>>) -> Restarts {
            Restarts { reads: Reads::of(script), starts: std::cell::Cell::new(0) }
        }
    }

    impl Place for Restarts {
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            self.starts.set(self.starts.get() + 1);
            Ok(())
        }
        fn state(&self, seat: &Seat) -> crate::place::Result<State> {
            self.reads.state(seat)
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("a retry must never send a work order")
        }
        fn open(&self, _: &Order) -> crate::place::Result<Seat> {
            panic!("a retry stays in the seat it was given")
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("a retry does not end the seat")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("a retry is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("a retry does not subscribe")
        }
        fn here(&self) -> Option<Seat> {
            panic!("a retry is about a seat it was handed")
        }
    }

    /// Two further starts, a backoff of one poll and a re-arm of one grace, on
    /// the test's clock. The numbers are small so the counts below are
    /// arithmetic rather than a statement about the machine.
    fn retrying<'a>(clock: &'a util::Dial) -> Patience<'a> {
        Patience {
            ready: READY,
            taken: TAKEN,
            nudges: PRESSES,
            poll: STEP,
            gone: GRACE,
            attempts: 2,
            backoff: STEP,
            steeper: 1,
            rearm: GRACE,
            clock,
        }
    }

    fn starting(place: &Restarts, how: &dyn agent_commands::Kind, seat: &Seat, wait: &Patience) -> Result<(), String> {
        let spawn =
            agent_commands::Spawn { full: false, subagents: false, name: "robustness-080", seat, model: None, effort: None, order: None, resume: None };
        let agent = Agent { kind: "claude".into(), name: "robustness-080".into(), args: Vec::new() };
        start_agent(place, how, &spawn, &agent, "claude", wait)
    }

    /// **The failure this task is named for, recovered.** An agent that starts
    /// and dies on its way up leaves a seat with a shell in it and nothing in
    /// the composer, so there is nothing a second start could duplicate — which
    /// is the whole reason this one case is retried and a failed handover is
    /// not.
    ///
    /// The script is the sequence Ed measured on 2026-08-18: the seat reads
    /// empty past the grace, the runtime cannot say the agent is alive, and the
    /// spawn used to stop there and report a claimed task nobody was working.
    #[test]
    fn an_agent_that_died_on_its_way_up_is_started_again() {
        let dead = Says(None, std::cell::Cell::new(0));
        let dial = util::Dial::new();
        let place = Restarts::of(
            std::iter::repeat_with(|| Ok(State::Empty))
                .take(32)
                .chain([Ok(State::Idle)])
                .collect(),
        );
        assert_eq!(starting(&place, &dead, &Seat::new("w64:p1"), &retrying(&dial)), Ok(()));
        assert_eq!(place.starts.get(), 2, "the first start died and the second was made");
    }

    /// And the seat that will not clear is the one a retry must leave alone.
    ///
    /// An agent alive and never ready is the case where a second `agent.start`
    /// would either be refused by herdr — `is_agent_terminal()` still holds —
    /// or, worse, land beside the first. So [`re_arm`] refuses and the spawn
    /// stops at reporting, which is what the governor's decision asked for: a
    /// partial retry, not a rewrite of the seat model.
    #[test]
    fn a_seat_that_will_not_empty_is_never_started_into_twice() {
        let quiet = Says(None, std::cell::Cell::new(0));
        let dial = util::Dial::new();
        let place = Restarts::of(vec![Ok(State::Working)]);
        let why = starting(&place, &quiet, &Seat::new("w64:p1"), &retrying(&dial)).unwrap_err();
        assert_eq!(place.starts.get(), 1, "one agent in the seat, and it stayed one");
        assert!(why.contains("still holding"), "it says why it did not try again: {why}");
    }

    /// A spawn that works costs nothing, which is the bill every healthy spawn
    /// pays for this feature and the reason the backoff is not a sleep before
    /// the first attempt.
    #[test]
    fn a_start_that_works_first_time_pays_no_backoff() {
        let alive = Says(Some(true), std::cell::Cell::new(0));
        let dial = util::Dial::new();
        let place = Restarts::of(vec![Ok(State::Idle)]);
        let began = dial.now();
        assert_eq!(starting(&place, &alive, &Seat::new("w64:p1"), &retrying(&dial)), Ok(()));
        assert_eq!(place.starts.get(), 1);
        assert_eq!(dial.now(), began, "a healthy spawn waits for nothing");
    }

    /// The bound, and it is what stops a retry becoming a loop. Three starts for
    /// two further attempts, and then the failure is reported rather than tried
    /// a fourth time.
    #[test]
    fn a_seat_that_never_holds_an_agent_is_given_up_on() {
        let dead = Says(None, std::cell::Cell::new(0));
        let dial = util::Dial::new();
        let place = Restarts::of(vec![Ok(State::Empty)]);
        let why = starting(&place, &dead, &Seat::new("w64:p1"), &retrying(&dial)).unwrap_err();
        assert_eq!(place.starts.get(), 3, "the first attempt and the two it is allowed");
        assert!(why.contains("started and then stopped"), "the last failure is what is said: {why}");
    }

    /// **The failure this task is named for, at the moment it is decided.** A
    /// backend that cannot see a live agent must not end its spawn.
    ///
    /// The script is what was recorded on 2026-08-17: `start` returns, and the
    /// seat reads empty for a stretch because herdr's detection has not caught
    /// up with the agent sitting in it. The old rule returned on the first of
    /// those readings, so the work order was never sent and a claimed task sat
    /// in front of an idle agent until somebody noticed.
    ///
    /// Both halves of the new rule are asserted here: the wait survives the
    /// blind stretch, and the runtime is asked about it once per [`GRACE`] —
    /// not once per poll, because asking is a subprocess.
    ///
    /// The count is exact, and that it can be is the point. Asserted against the
    /// wall clock it was **flaky** — nine asks where eight were allowed,
    /// passing alone and failing under the full suite, because the throttle is
    /// per elapsed second and three other agents building beside it stretched
    /// two hundred polls over enough time for one more. The clock the wait reads
    /// is now a parameter and the wait's own `rest` drives it one [`STEP`] per
    /// poll, so the answer below is arithmetic: two hundred polls of one
    /// millisecond, asked every thirty, is six.
    #[test]
    fn a_seat_the_backend_cannot_see_does_not_end_a_spawn_whose_agent_is_alive() {
        let alive = Says(Some(true), std::cell::Cell::new(0));
        let dial = util::Dial::new();
        let place = Reads::of(
            std::iter::repeat_with(|| Ok(State::Empty))
                .take(200)
                .chain([Ok(State::Starting), Ok(State::Idle)])
                .collect(),
        );
        assert_eq!(waiting_on(&place, &alive, &Seat::new("w2C:p1"), &dial), Ok(()));
        assert_eq!(alive.1.get(), 6, "once per grace over two hundred polls, and no oftener");
    }

    /// And the other side of it: a spawn that really did fail still fails, and
    /// does not wait out the deadline to say so.
    ///
    /// This is what the fast verdict was for and why it is kept rather than
    /// replaced by patience. Thirty seconds of silence in front of a person who
    /// has just typed `wsp spawn` is its own kind of wrong answer.
    #[test]
    fn an_agent_that_never_started_is_reported_without_waiting_out_the_deadline() {
        let dead = Says(Some(false), std::cell::Cell::new(0));
        let dial = util::Dial::new();
        let place = Reads::of(vec![Ok(State::Empty)]);
        assert_eq!(
            waiting_on(&place, &dead, &Seat::new("w2C:p1"), &dial),
            Err("claude started and then stopped in w2C:p1".into())
        );
        assert_eq!(dead.1.get(), 1, "one question, asked once the grace was up");
        assert!(dial.elapsed() < READY, "waited out the whole deadline to say so");
        assert!(dial.elapsed() >= GRACE, "gave up before a launch has had time to finish");
        // A kind with no runtime to ask is not thereby immortal: the seat has
        // still been empty for longer than a launch takes.
        let mute = Says(None, std::cell::Cell::new(0));
        let place = Reads::of(vec![Ok(State::Empty)]);
        assert!(waiting_on(&place, &mute, &Seat::new("w2C:p1"), &util::Dial::new()).is_err());
    }

    /// One empty reading between two live ones is a gap in what the backend can
    /// see, and nothing is asked about it.
    ///
    /// The cheap half of the rule, and the one that does the most work: a single
    /// dropout costs nothing at all, so the expensive question is only reached by
    /// a seat that has looked empty for two seconds together.
    #[test]
    fn a_single_dropout_between_two_live_readings_is_not_worth_asking_about() {
        let never = Says(Some(false), std::cell::Cell::new(0));
        let place = Reads::of(vec![
            Ok(State::Starting),
            Ok(State::Empty),
            Err(Refusal::Unreachable("socket".into())),
            Ok(State::Starting),
            Ok(State::Idle),
        ]);
        assert_eq!(waiting_on(&place, &never, &Seat::new("w2C:p1"), &util::Dial::new()), Ok(()));
        assert_eq!(never.1.get(), 0, "a subprocess was run for a single blink");
    }

    /// A seat that is gone is not a seat whose agent is quiet, and no amount of
    /// patience will make a closed pane answer.
    #[test]
    fn a_pane_that_has_been_closed_is_said_plainly_and_at_once() {
        let alive = Says(Some(true), std::cell::Cell::new(0));
        let place = Reads::of(vec![Err(Refusal::NoSeat(Seat::new("w2C:p1")))]);
        assert_eq!(
            waiting_on(&place, &alive, &Seat::new("w2C:p1"), &util::Dial::new()),
            Err("w2C:p1 is gone".into())
        );
    }

    /// A backend that takes a work order and then does what the night of
    /// 2026-08-17 did: holds it in the composer until somebody presses return.
    ///
    /// `starts_after` is the number of presses it takes to start a turn —
    /// `Some(0)` is a healthy handover, `None` is the seat nothing rescues, and
    /// `Some(1)` is every failure Ed recovered by hand. The counter is the
    /// assertion that matters twice over: a loop that presses when it did not
    /// need to is typing into somebody's work, and one that presses for ever is
    /// the silence this task was filed about wearing a different face.
    struct Composer {
        starts_after: Option<u32>,
        can_press: bool,
        /// Whether this backend *watched* for the turn and can say it never
        /// started, which is what herdr's `agent.prompt` wait answers and what
        /// the port spells [`Refusal::NotTaken`].
        ///
        /// `false` is every backend that cannot, and the composer's silence has
        /// to be found by looking. The two answers describe the same seat and
        /// the handover should reach the same end from both — what differs is
        /// how long it spends finding out, which is what the test below is for.
        watches: bool,
        pressed: std::cell::Cell<u32>,
        heard: std::cell::RefCell<Vec<String>>,
    }

    impl Composer {
        fn of(starts_after: Option<u32>, can_press: bool) -> Composer {
            Composer {
                starts_after,
                can_press,
                watches: false,
                pressed: std::cell::Cell::new(0),
                heard: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn watching(starts_after: Option<u32>) -> Composer {
            Composer { watches: true, ..Composer::of(starts_after, true) }
        }
    }

    impl Place for Composer {
        fn tell(&self, _: &Seat, text: &str) -> crate::place::Result<Delivery> {
            self.heard.borrow_mut().push(text.to_string());
            match self.watches && self.starts_after != Some(0) {
                true => Err(Refusal::NotTaken),
                false => Ok(Delivery::Started),
            }
        }
        fn nudge(&self, _: &Seat) -> crate::place::Result<()> {
            if !self.can_press {
                return Err(Refusal::Unsupported("press submit"));
            }
            self.pressed.set(self.pressed.get() + 1);
            Ok(())
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            Ok(match self.starts_after {
                Some(n) if self.pressed.get() >= n => State::Working,
                // Idle, which is the whole of the defect: the agent is well,
                // it is waiting for input, and what it is waiting on is the
                // work order it was already given.
                _ => State::Idle,
            })
        }
        fn open(&self, _: &Order) -> crate::place::Result<Seat> {
            panic!("a handover does not open seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            panic!("the agent is already running")
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("a handover that fails leaves the agent standing")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("a handover is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("a handover does not subscribe")
        }
        fn here(&self) -> Option<Seat> {
            panic!("a handover is about a seat it was handed")
        }
    }

    /// A kind that says its sentence through the backend, which is what
    /// `agent_commands::Claude` does and what makes the port's `tell` the one
    /// under test.
    struct Speaks;

    impl agent_commands::Kind for Speaks {
        fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> crate::place::Result<Delivery> {
            place.tell(seat, text)
        }
        fn args(&self, _: &agent_commands::Spawn) -> Vec<String> {
            Vec::new()
        }
        fn tier(&self, _: Option<&str>, _: Option<&str>) -> std::result::Result<(), String> {
            Ok(())
        }
        fn address(
            &self,
            _: &dyn Place,
            _: &agent_commands::Spawn,
        ) -> Option<agent_commands::Address> {
            None
        }
        fn running(&self, _: &agent_commands::Spawn) -> Option<bool> {
            None
        }
        fn ran(&self, _: &str, _: &str, _: i64) -> Option<agent_commands::Ran> {
            None
        }
    }

    fn handing_over(place: &Composer, clock: &util::Dial) -> Result<(), String> {
        let seat = Seat::new("w3M:p1");
        let spawn = agent_commands::Spawn {
            full: false,
            subagents: false,
            name: "robustness-035",
            seat: &seat,
            model: None,
            effort: None,
            order: None,
            resume: None,
        };
        // The retry fields come from `retrying` because this test is about a
        // single attempt: a literal here would have to be updated every time the
        // retry's numbers move, for a wait that never reads them.
        let wait = Patience { ready: READY, taken: TAKEN, nudges: PRESSES, poll: STEP, gone: GRACE, clock, ..retrying(clock) };
        hand_over(place, &Speaks, &spawn, "you have been claimed onto robustness-035", &wait)
    }

    /// The healthy handover, and the assertion is that nothing else happens: an
    /// agent that starts a turn is not typed at, and the sentence is not paid
    /// for twice.
    #[test]
    fn a_work_order_the_agent_starts_on_is_not_pressed_again() {
        let place = Composer::of(Some(0), true);
        let dial = util::Dial::new();
        assert_eq!(handing_over(&place, &dial), Ok(()));
        assert_eq!(place.pressed.get(), 0, "return was pressed at an agent already working");
        assert_eq!(place.heard.borrow().len(), 1, "the work order was sent more than once");
        assert_eq!(dial.elapsed(), Duration::ZERO, "a spawn that went well waited for it");
    }

    /// **The failure this task is named for, and the recovery that closes it.**
    ///
    /// The sentence arrives, the submit is swallowed, the agent sits idle in
    /// front of it. One press starts the turn — which is exactly what
    /// `herdr agent send-keys <pane> enter` did by hand, four times in one
    /// burst on 2026-08-17.
    ///
    /// The second assertion is the one that would be easy to lose: the retry is
    /// a *submit* and never a second `tell`. Sending the work order again would
    /// leave two copies of it in the composer, and the agent would read the
    /// duplicate as something it had been told twice.
    #[test]
    fn a_work_order_left_sitting_in_the_composer_is_submitted_again() {
        let place = Composer::of(Some(1), true);
        let dial = util::Dial::new();
        assert_eq!(handing_over(&place, &dial), Ok(()));
        assert_eq!(place.pressed.get(), 1, "one press was enough by hand and should be here");
        assert_eq!(place.heard.borrow().len(), 1, "the work order was sent twice over");
        assert_eq!(dial.elapsed(), TAKEN, "one window of patience before pressing, and one only");
    }

    /// And when nothing rescues it, the spawn fails and says so.
    ///
    /// A claimed task in front of an idle agent is what this used to report as
    /// success, so the assertion is on the `Err` rather than on the recovery:
    /// the caller prints it, exits non-zero, and an unattended queue learns that
    /// nothing has started. The press count is bounded arithmetic — three
    /// windows and two presses — because a loop that goes on pressing is the
    /// same silence with more typing in it.
    ///
    /// The wording is asserted on too, because it is the deliverable of
    /// agent-009: the message names both places the order can be rather than
    /// asserting one. "Sitting in w3M:p1 unsent" was printed about a composer
    /// that was empty when somebody looked three seconds later, twice more that
    /// week; a reader acting on the assertion would go digging for text that is
    /// not there instead of sending the order again.
    #[test]
    fn an_agent_that_never_takes_the_work_order_fails_the_spawn_rather_than_reporting_one() {
        let place = Composer::of(None, true);
        let dial = util::Dial::new();
        assert_eq!(
            handing_over(&place, &dial),
            Err("no turn started in w3M:p1: the work order went to the pane, and \
                 is either sitting unsent or was dropped — wsp cannot see which"
                .into())
        );
        assert_eq!(place.pressed.get(), PRESSES, "the press loop is not bounded");
        assert_eq!(dial.elapsed(), TAKEN * (PRESSES + 1), "the wait is not bounded either");
    }

    /// A backend with nothing to press does not get pressed, and the spawn
    /// still fails.
    ///
    /// The second instance from the same night: `wsp spawn --on` into a session
    /// that was never logged in reported every line of success it has. There is
    /// no keystroke that fixes that one, and the point of the answer is that the
    /// caller stops rather than that it recovers — a turn that has not started
    /// is a spawn that has not happened, whatever the backend can or cannot do
    /// about it.
    #[test]
    fn a_backend_with_nothing_to_press_says_the_turn_never_started() {
        let place = Composer::of(None, false);
        let dial = util::Dial::new();
        assert_eq!(
            handing_over(&place, &dial),
            Err("w3M:p1 took the work order and started nothing".into())
        );
        assert_eq!(place.pressed.get(), 0, "something was pressed on a backend with no keys");
        assert_eq!(dial.elapsed(), TAKEN, "a backend that cannot be pressed was waited on twice");
    }

    /// **A backend that watched the turn never start is believed rather than
    /// polled, and it reaches the same end.**
    ///
    /// herdr's `agent.prompt` now holds its reply until the agent's status moves
    /// and refuses when it does not, so the five seconds this used to spend
    /// discovering that were being spent twice — once by the server, then again
    /// by wsp asking a question it had just been answered. The submit is pressed
    /// at once instead, and the clock is the assertion: nothing is waited for
    /// before the press, and what is waited for after it is the rescue.
    ///
    /// The recovery itself is untouched, and that is the point of pairing this
    /// with the test above rather than replacing it. herdr can say a prompt was
    /// delivered and the agent unmoved; it cannot say why, and a folder-trust
    /// modal holding the keyboard looks from its side exactly like a composer
    /// that was not ready. Only one of those is a thing a keystroke fixes, so
    /// the keystroke stays.
    #[test]
    fn a_backend_that_says_the_turn_never_started_is_not_asked_again_before_pressing() {
        let place = Composer::watching(Some(1));
        let dial = util::Dial::new();
        assert_eq!(handing_over(&place, &dial), Ok(()));
        assert_eq!(place.pressed.get(), 1, "the work order still needed its submit");
        assert_eq!(place.heard.borrow().len(), 1, "the work order was sent more than once");
        assert_eq!(dial.elapsed(), Duration::ZERO, "wsp waited for what it had already been told");
    }

    /// The same backend on a healthy handover: `Ok` from something that watched
    /// is a turn that started, and it costs one look and no waiting.
    #[test]
    fn a_handover_a_backend_confirmed_itself_costs_no_waiting() {
        let place = Composer::watching(Some(0));
        let dial = util::Dial::new();
        assert_eq!(handing_over(&place, &dial), Ok(()));
        assert_eq!(place.pressed.get(), 0, "return was pressed at an agent already working");
        assert_eq!(dial.elapsed(), Duration::ZERO, "a spawn that went well waited for it");
    }

    /// A watching backend whose agent nothing rescues still fails, and the
    /// bound is the presses rather than the answer that started it.
    #[test]
    fn a_turn_no_submit_starts_fails_the_spawn_whoever_noticed_first() {
        let place = Composer::watching(None);
        let dial = util::Dial::new();
        assert_eq!(
            handing_over(&place, &dial),
            Err("no turn started in w3M:p1: the work order went to the pane, and \
                 is either sitting unsent or was dropped — wsp cannot see which"
                .into())
        );
        assert_eq!(place.pressed.get(), PRESSES, "the press loop is not bounded");
        assert_eq!(dial.elapsed(), TAKEN * PRESSES, "the look it was spared was not spared");
    }

    /// A backend that only remembers what it was asked to open.
    struct Opens(std::cell::RefCell<Vec<Order>>);

    impl Place for Opens {
        fn open(&self, order: &Order) -> crate::place::Result<Seat> {
            self.0.borrow_mut().push(order.clone());
            Ok(Seat::new("w9:p1"))
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("spawn does not end seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            panic!("no agent was asked for")
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("no agent was asked for")
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            panic!("spawn does not ask how the work is going")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("spawn is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("spawn does not wait for anything")
        }
        fn here(&self) -> Option<Seat> {
            panic!("spawn opens a seat rather than asking which one it is in")
        }
    }

    /// A backend that remembers what it was asked to open and what it was asked
    /// to start, and answers ready.
    struct Started(std::cell::RefCell<Vec<Agent>>, std::cell::RefCell<Vec<Order>>);

    impl Place for Started {
        fn open(&self, order: &Order) -> crate::place::Result<Seat> {
            self.1.borrow_mut().push(order.clone());
            Ok(Seat::new("w9:p1"))
        }
        fn start(&self, _: &Seat, agent: &Agent) -> crate::place::Result<()> {
            self.0.borrow_mut().push(agent.clone());
            Ok(())
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            Ok(State::Idle)
        }
        // The other end of the same test: `despawn` is what takes the brief
        // away again.
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            Ok(())
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("a kind that takes its order in argv is not told it as well")
        }
        // Asked for after the start, to report what is sitting in the seat.
        // Nothing of this test's is downstream of it.
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            Ok(crate::place::Census::heard("", Vec::new()))
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("spawn does not wait for anything")
        }
        fn here(&self) -> Option<Seat> {
            panic!("spawn opens a seat rather than asking which one it is in")
        }
    }

    /// Driven through the whole command: the brief that goes out in an
    /// unbriefed kind's argv is about the seat that was just opened, and the
    /// task it holds is the one that was just claimed into it.
    ///
    /// This is the failure `cmd_brief::At` exists to prevent and the reason
    /// `core-032` is not a one-line change. `cmd_brief::Briefing::live` reads
    /// where it is out of the process environment, and the process here is the
    /// *spawning* session — a governor, usually, holding a task of its own. Composed
    /// that way, every agent it started would be handed the governor's task,
    /// the governor's tree and the governor's pane, and would read all three as
    /// its own. So the assertion is on the pair: the claimed task is in the
    /// brief and the one this session is holding is not.
    #[test]
    fn a_spawn_composes_the_brief_for_the_seat_it_opened_and_not_for_itself() {
        let _guard = no_backend();
        // The seat this process is in, unset — because `Briefing::live` reads
        // exactly these, and a test that inherited a real pane from the session
        // running it would pass on the wrong brief being right by accident. It
        // removes rather than exports, so nothing else in the process trips
        // over it; see `a_despawn_will_not_end_the_seat_it_is_running_in`.
        std::env::remove_var("HERDR_PANE_ID");
        std::env::remove_var("HERDR_WORKSPACE_ID");
        let store = seat("brief");
        let mut proj = Project::new("core");
        proj.body = "## Handbook\nread the source map first\n".into();
        store.save_project(&proj).unwrap();
        for (id, title, overview) in [
            ("t-1", "the one being handed over", "compose the brief into the order"),
            ("t-2", "what the spawner is holding", "not this agent's business"),
        ] {
            let mut t = crate::model::Task::new(title, id);
            t.project = Some("core".into());
            t.body = format!("## Overview\n{overview}\n");
            store.save_task(&t).unwrap();
        }

        let place = Started(std::cell::RefCell::new(Vec::new()), std::cell::RefCell::new(Vec::new()));
        let flags = [("agent", "true"), ("kind", "opencode"), ("no-tree", "true")];
        assert_eq!(place_work(&place, &store, &Args::synth("spawn", &["t-1"], &flags)), 0);

        // The seat's environment names the file, and the file is where the
        // brief went. Read out of the order the backend was asked to open,
        // because that is the value herdr actually receives.
        let opened = place.1.borrow().first().cloned().expect("no seat was opened");
        let cfg: serde_json::Value =
            serde_json::from_str(opened.env.get("OPENCODE_CONFIG_CONTENT").expect("no config on the seat"))
                .expect("valid config");
        let named = cfg["instructions"][0].as_str().expect("no brief named").to_string();
        let brief = std::fs::read_to_string(&named).expect("named a file that is not there");

        // The brief, and the task it is about.
        assert!(brief.contains("compose the brief into the order"), "no overview: {brief}");
        assert!(brief.contains("read the source map first"), "no handbook: {brief}");
        assert!(brief.contains("t-1"), "{brief}");
        // And not the other one. A brief composed for this process rather than
        // for the seat would have named whatever the caller was holding.
        assert!(!brief.contains("not this agent's business"), "the spawner's task: {brief}");

        // Outside every working tree, which is `core-020` d2's correction: a
        // file wsp leaves in a checkout makes it permanently dirty.
        assert!(named.starts_with(store.state.to_str().unwrap()), "not beside the state: {named}");

        // The order itself, no longer asking for what the agent already has.
        let order = place.0.borrow().first().cloned().expect("nothing was started")
            .args
            .windows(2)
            .find(|w| w[0] == "--prompt")
            .map(|w| w[1].clone())
            .expect("no work order in the argv");
        assert!(order.contains("You have been claimed onto t-1"), "{order}");
        assert!(!order.contains("wsp brief --session"), "sent to fetch what it was given: {order}");
        // One argv element, and nothing in it herdr will refuse: the encoder
        // rejects any control character, which is what put the brief in a file
        // rather than in this string.
        assert!(!order.contains('\n'), "a multi-line work order is refused by herdr: {order:?}");

        // And the ending takes it away. A brief left behind is a stale answer
        // waiting to be handed to whoever is spawned onto the task next.
        assert_eq!(end_work(&place, &store, &Args::synth("despawn", &["t-1"], &[]), Caller::default(), &|_, _, _| {
            Leftovers { tree: Tree::Absent, builds: Vec::new() }
        }), 0);
        assert!(!std::path::Path::new(&named).exists(), "the brief outlived the seat: {named}");
    }

    /// The whole of robustness-010, at the one line where it happens.
    ///
    /// Every softer answer to two agents in one checkout has been tried in this
    /// repository and each failed the same way — an agent that has to remember
    /// a rule is halfway past it when it matters. So the tree is not something
    /// an agent asks for, it is where `spawn` opens the pane; and the assertion
    /// that matters is that a task seat is *not* opened at the project root.
    ///
    /// A project seat still is. It has nothing to branch for and nothing to
    /// land, and it is where somebody reading the whole tree wants to stand.
    #[test]
    fn a_task_seat_is_opened_in_a_checkout_of_its_own_and_a_project_seat_is_not() {
        let _guard = no_backend();
        let store = seat("tree");
        let root = store.root.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        for argv in [
            vec!["init", "--quiet", "-b", "master"],
            vec!["commit", "--quiet", "--allow-empty", "-m", "first"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&argv)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        let mut proj = Project::new("robustness");
        proj.roots = vec![root.display().to_string()];
        store.save_project(&proj).unwrap();
        let mut task = crate::model::Task::new("one tree each", "t-1");
        task.project = Some("robustness".into());
        store.save_task(&task).unwrap();

        let opened = |args: Args| {
            let place = Opens(std::cell::RefCell::new(Vec::new()));
            place_work(&place, &store, &args);
            let cwd = place.0.borrow().first().and_then(|o| o.cwd.clone()).unwrap_or_default();
            cwd
        };

        let cwd = opened(Args::synth("spawn", &["t-1"], &[]));
        assert!(
            cwd.ends_with(".worktrees/t-1"),
            "a task seat was opened in the shared trunk: {cwd}"
        );
        assert!(std::path::Path::new(&cwd).join(".git").exists(), "{cwd} is not a checkout");

        let cwd = opened(Args::synth("spawn", &[], &[("project", "robustness")]));
        assert_eq!(
            util::real(&cwd),
            util::real(&root.display().to_string()),
            "a project seat left the trunk"
        );

        // `--govern` seats an agent on a project, and a task is not one. The
        // near miss is the expensive one: a claim on a piece of work plus a
        // sentence telling the agent it is answerable for everything above it,
        // which is the confusion the slot exists to end. Refused before
        // anything is opened, so a typo costs nothing.
        let place = Opens(std::cell::RefCell::new(Vec::new()));
        let code = place_work(
            &place,
            &store,
            &Args::synth("spawn", &["t-1"], &[("govern", "true")]),
        );
        assert_eq!(code, 2, "--govern on a task was accepted");
        assert!(place.0.borrow().is_empty(), "it opened a workspace before refusing");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// A spawn asks for the screen only when somebody asked for it.
    ///
    /// *Asks*, and the distinction is load-bearing: the ask is honoured all the
    /// way down and the screen moves anyway, because the panel installed into
    /// the new workspace swaps a pane and `pane.swap` focuses unconditionally.
    /// That is `fork-002` and it cannot be tested here — this test owns what
    /// wsp requests, which is the only half wsp decides.
    ///
    /// This line read `!args.has("no-focus")` until 2026-08-17, so the screen
    /// moved onto every new seat unless the caller remembered to say otherwise —
    /// and `spawn` is run by the queue as much as by hand, which made a batch
    /// started overnight a sequence of jumps away from whatever was being read.
    /// herdr's `workspace.create` defaults `focus` to false and [`Order::show`]
    /// says the same, so the flip is wsp stopping opting in rather than a new
    /// rule. `--no-focus` still parses, and now names the default.
    #[test]
    fn a_spawn_leaves_the_screen_where_it_was_unless_asked() {
        let _guard = no_backend();
        let store = seat("focus");
        store.save_project(&Project::new("robustness")).unwrap();

        let shown = |flags: &[(&str, &str)]| {
            let mut f = vec![("project", "robustness")];
            f.extend_from_slice(flags);
            let place = Opens(std::cell::RefCell::new(Vec::new()));
            place_work(&place, &store, &Args::synth("spawn", &[], &f));
            let show = place.0.borrow().first().map(|o| o.show);
            show.expect("nothing was opened")
        };

        assert!(!shown(&[]), "a spawn with no opinion took the screen");
        assert!(shown(&[("focus", "true")]), "--focus asked and was not obeyed");
        assert!(!shown(&[("no-focus", "true")]), "--no-focus asks for what already happens");

        let _ = std::fs::remove_dir_all(&store.root);
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
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("despawn does not talk to agents")
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            panic!("despawn does not ask how the work is going")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("despawn is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("despawn does not wait for anything")
        }
        // Which seat this is arrives as an argument to `end_work`, deliberately:
        // see `a_despawn_will_not_end_the_seat_it_is_running_in`.
        fn here(&self) -> Option<Seat> {
            panic!("despawn is told which seat it is running in")
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
    /// An empty store comes with it, on the same argument one step further out:
    /// a store these tests did not write is a store somebody else did, and
    /// `place_work` reads one — see [`crate::util::isolated`].
    fn no_backend() -> util::Isolated {
        let env = util::isolated("spawn-no-backend");
        std::env::set_var("HERDR_SOCKET_PATH", "/nonexistent/wsp-despawn-tests.sock");
        env
    }

    /// The tidying half, stubbed, and what each test asserts instead.
    ///
    /// [`swept_up`] is the only part of `despawn` that needs a git repository
    /// and a live herdr, and the tests below are about the *ordering* — stop,
    /// release, then tidy — and about what is reported. Reaching the real one
    /// here would run `git worktree remove` in whatever tree the test runner is
    /// standing in, which is somebody's actual work. What it did is recorded so
    /// the one thing worth asserting about it can be: that it ran last, and only
    /// on an ending that got that far.
    #[derive(Default)]
    struct Tidied(std::cell::RefCell<Vec<(String, Option<String>)>>);

    impl Tidied {
        fn f(&self) -> impl Fn(&Seat, Option<&str>, Option<&str>) -> Leftovers + '_ {
            |seat: &Seat, task: Option<&str>, _ws: Option<&str>| {
                self.0.borrow_mut().push((seat.as_str().to_string(), task.map(String::from)));
                Leftovers { tree: Tree::Absent, builds: Vec::new() }
            }
        }
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
        let tidied = Tidied::default();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), Caller::default(), &tidied.f());

        assert_eq!(code, 1, "a despawn that ended nothing must not report success");
        assert_eq!(place.asked.borrow().len(), 1, "it did try");
        assert!(store.claims().contains_key("t-260816-095"), "the claim went with the agent still there");
        assert!(store.bindings().contains_key("w1:p1"), "and so did the binding");
        assert!(tidied.0.borrow().is_empty(), "a seat still standing keeps its tree too");

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
        let tidied = Tidied::default();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), Caller::default(), &tidied.f());

        assert_eq!(code, 0);
        assert_eq!(place.asked.borrow().as_slice(), &[Seat::new("w1:p1")], "the bound seat");
        assert!(!store.claims().contains_key("t-260816-095"), "the claim outlived the seat");
        assert!(!store.bindings().contains_key("w1:p1"));
        assert!(store.worked().contains_key("t-260816-095"), "no trace of who had it");
        let t = store.task("t-260816-095").expect("the task");
        assert_eq!(t.status_raw, "doing", "work nobody is on is still work");
        assert!(t.body.contains("released"), "the log should say it was put down:\n{}", t.body);
        assert_eq!(
            tidied.0.borrow().as_slice(),
            &[("w1:p1".to_string(), Some("t-260816-095".to_string()))],
            "the tree is tidied last, and named by the task the release ended"
        );

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
        let tidied = Tidied::default();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), Caller::default(), &tidied.f());

        assert_eq!(code, 0);
        assert!(!store.claims().contains_key("t-260816-095"));

        // A backend that did not answer is not the same sentence, and must not
        // release: silence is not evidence that the seat is gone.
        working(&store, "t-260816-094", "w1:p2");
        let quiet = Ends::refusing(Refusal::Unreachable("no socket".into()));
        let untidied = Tidied::default();
        assert_eq!(end_work(&quiet, &store, &Args::synth("despawn", &["094"], &[]), Caller::default(), &untidied.f()), 1);
        assert!(untidied.0.borrow().is_empty(), "an ending that released nothing must not remove a tree");
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
        let tidied = Tidied::default();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), Caller { pane: Some("w1:p1"), ..Caller::default() }, &tidied.f());

        assert_eq!(code, 2);
        assert!(place.asked.borrow().is_empty(), "it asked the backend to end this pane");
        assert!(store.claims().contains_key("t-260816-095"), "and it dropped its own claim");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    // ---- rotation ----------------------------------------------------------

    /// A herdr that is not herdr: answers every call with one fixture. That is
    /// everything a rotation asks of a live herdr — one `pane.list`, so the
    /// successor's workspace id is readable (`place` has no word for a room),
    /// and whatever renames a moved slot triggers, whose answers nothing here
    /// reads. Same shape as herdr's own tests' stand-in; private to that file,
    /// which is why this is written out.
    fn herdr_stand_in(path: &std::path::Path, n: usize, reply: serde_json::Value) {
        use std::io::{BufRead, BufReader, Write};
        let _ = std::fs::remove_file(path);
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((stream, _)) = listener.accept() else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    break;
                }
                let Ok(req) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    continue;
                };
                let out = json!({ "id": req["id"], "result": reply });
                let mut stream = stream;
                let _ = stream.write_all(format!("{out}\n").as_bytes());
                let _ = stream.flush();
            }
        });
    }

    /// The pane the stand-in answers with: the successor's seat, in workspace
    /// `w9`. The room is what a moved slot is recorded against.
    fn successor_pane() -> serde_json::Value {
        json!({ "panes": [{
            "pane_id": "w9:p2", "workspace_id": "w9", "tab_id": "", "label": "",
            "agent": "", "agent_status": "", "cwd": "/", "title": "",
            "focused": false, "session_id": "", "agent_name": "",
        }] })
    }

    /// A backend that opens one seat, starts whatever it is told, delivers every
    /// sentence, and answers state off a script — the first readings popped in
    /// order, then the last held for ever. Idle first makes the agent ready;
    /// Working after it starts the turn.
    struct Seats {
        opened: std::cell::RefCell<Vec<Order>>,
        started: std::cell::Cell<u32>,
        told: std::cell::RefCell<Vec<String>>,
        states: std::cell::RefCell<std::collections::VecDeque<crate::place::Result<State>>>,
        last: crate::place::Result<State>,
    }

    impl Seats {
        fn of(script: Vec<crate::place::Result<State>>) -> Seats {
            Seats {
                opened: std::cell::RefCell::new(Vec::new()),
                started: std::cell::Cell::new(0),
                told: std::cell::RefCell::new(Vec::new()),
                last: script.last().cloned().unwrap_or(Ok(State::Unknown)),
                states: std::cell::RefCell::new(script.into()),
            }
        }
    }

    impl Place for Seats {
        fn open(&self, order: &Order) -> crate::place::Result<Seat> {
            self.opened.borrow_mut().push(order.clone());
            Ok(Seat::new("w9:p2"))
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            self.started.set(self.started.get() + 1);
            Ok(())
        }
        fn tell(&self, _: &Seat, text: &str) -> crate::place::Result<Delivery> {
            self.told.borrow_mut().push(text.to_string());
            Ok(Delivery::Started)
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            match self.states.borrow_mut().pop_front() {
                Some(s) => s,
                None => self.last.clone(),
            }
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("rotation does not end seats")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("rotation is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("rotation does not subscribe")
        }
        fn here(&self) -> Option<Seat> {
            panic!("rotation opens a seat rather than asking which one it is in")
        }
    }

    /// The waits, on the tests' clock: nothing here sleeps. One poll of
    /// readiness, ten polls of confirmation, no start retries.
    fn handover_wait<'a>(clock: &'a util::Dial) -> Patience<'a> {
        Patience {
            ready: STEP,
            taken: TAKEN,
            nudges: PRESSES,
            poll: STEP,
            gone: GRACE,
            attempts: 0,
            backoff: STEP,
            steeper: 1,
            rearm: GRACE,
            clock,
        }
    }

    /// A store of its own plus the caller env, which rotation reads the way
    /// `govern` does: from herdr's context variables.
    fn rotating_as(tag: &str, ws: &str, pane: &str) -> (util::Isolated, Store) {
        let env = util::isolated(tag);
        std::env::set_var("HERDR_WORKSPACE_ID", ws);
        std::env::set_var("HERDR_PANE_ID", pane);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        (env, store)
    }

    /// Cleared before any assertion can panic: these are process-wide, the
    /// isolation guard does not know them, and the test next door must not
    /// inherit this one's pane.
    fn stop_being_a_seat() {
        std::env::remove_var("HERDR_WORKSPACE_ID");
        std::env::remove_var("HERDR_PANE_ID");
    }

    /// **The whole verb, working:** the successor is seated, its first turn is
    /// confirmed, and only then does the slot move. The ending rides the store,
    /// addressed to the pane that inherited it.
    #[test]
    fn a_rotation_moves_the_seat_only_once_the_successors_turn_is_running() {
        let (_env, store) = rotating_as("rotate-ok", "w1", "w1:p9");
        let sock = _env.path("herdr.sock");
        herdr_stand_in(&sock, 24, successor_pane());
        std::env::set_var("HERDR_SOCKET_PATH", &sock);

        store.save_project(&Project::new("core")).unwrap();
        cmd_govern::take(&store, "core", "w1", "w1:p9");

        let dial = util::Dial::new();
        let place = Seats::of(vec![Ok(State::Idle), Ok(State::Working)]);
        let args = Args::synth("govern", &["core"], &[("rotate", "true"), ("kind", "plain")]);
        let code = rotate_on(&place, &store, &args, &handover_wait(&dial));
        stop_being_a_seat();

        assert_eq!(code, 0);
        assert_eq!(place.started.get(), 1, "one successor");

        // The slot moved, and to whom: the record names the successor's room
        // and pane now, where before the verb ran it named the caller's.
        let rec = &store.governors()["core"];
        assert_eq!(rec["workspace"], "w9");
        assert_eq!(rec["pane"], "w9:p2");

        // The ending rode the store rather than the typed order, addressed to
        // the pane that inherited it.
        let h = store.handovers();
        assert_eq!(h["core"]["from"], "w1:p9", "the pane to end");
        assert_eq!(h["core"]["to"], "w9:p2", "the pane told to end it");

        // Delivered once and confirmed - not sent twice on a healthy handover.
        let told = place.told.borrow();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("custodian of the core project"), "{told:?}");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **The failure the verb exists for, degrading the way it was designed
    /// to:** the successor started but never took the handover, so nothing is
    /// ended, the record is taken back, and the caller is still the seat.
    #[test]
    fn a_rotation_that_never_starts_a_turn_ends_nothing_and_leaves_the_caller_seated() {
        let (_env, store) = rotating_as("rotate-stall", "w1", "w1:p9");
        store.save_project(&Project::new("core")).unwrap();
        cmd_govern::take(&store, "core", "w1", "w1:p9");

        let dial = util::Dial::new();
        // Idle for ever: ready to be told, never taking.
        let place = Seats::of(vec![Ok(State::Idle)]);
        let args = Args::synth("govern", &["core"], &[("rotate", "true"), ("kind", "plain")]);
        let code = rotate_on(&place, &store, &args, &handover_wait(&dial));
        stop_being_a_seat();

        assert_eq!(code, 1, "an unconfirmed handover is not a successful one");
        // The slot never moved: failing at step two is the predecessor still
        // seated, which is the state before the attempt and the safest one.
        let rec = &store.governors()["core"];
        assert_eq!(rec["workspace"], "w1");
        assert_eq!(rec["pane"], "w1:p9");
        // The promise was kept only while it could still be honoured: a stale
        // record orders the death of a seated custodian's pane.
        assert!(store.handovers().is_empty(), "{:?}", store.handovers());
        // And the repair was tried before giving up - the resend is the half a
        // person used to do after reading stderr.
        assert_eq!(place.told.borrow().len(), 2, "{:?}", place.told.borrow());
        assert_eq!(
            place.opened.borrow().len(), 1,
            "one successor was seated, and sits there idle"
        );

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// Rotation is the seat's own act. A pane holding nothing, holding another
    /// scope, or standing outside any room at all is refused before anything
    /// is opened, and a finished run has nothing to rotate into by the same
    /// condition that puts the rotate line in front of a custodian.
    #[test]
    fn a_rotation_refuses_before_anything_is_opened_when_the_caller_or_the_run_do_not_qualify() {
        use crate::model::{Group, Status, Task, Worklist, WorklistStatus};

        // No caller identity at all.
        {
            let env = util::isolated("rotate-nobody");
            std::env::remove_var("HERDR_WORKSPACE_ID");
            std::env::remove_var("HERDR_PANE_ID");
            let store = Store::at(env.home(), env.state());
            store.ensure_dirs().unwrap();
            store.save_project(&Project::new("core")).unwrap();
            cmd_govern::take(&store, "core", "w1", "w1:p9");
            let dial = util::Dial::new();
            let place = Seats::of(vec![Ok(State::Idle)]);
            let args = Args::synth("govern", &["core"], &[("rotate", "true"), ("kind", "plain")]);
            assert_eq!(rotate_on(&place, &store, &args, &handover_wait(&dial)), 2);
            assert!(place.opened.borrow().is_empty(), "nothing was opened");
            assert!(cmd_govern::governs(&store.governors(), "w1", Some("w1:p9")).is_some(),
                "and the seat stayed where it was");
        }

        // A caller that holds a different scope than the one named.
        {
            let (_env, store) = rotating_as("rotate-wrong-scope", "w2", "w2:p2");
            store.save_project(&Project::new("core")).unwrap();
            store.save_project(&Project::new("other")).unwrap();
            cmd_govern::take(&store, "other", "w2", "w2:p2");
            let dial = util::Dial::new();
            let place = Seats::of(vec![Ok(State::Idle)]);
            let args = Args::synth("govern", &["core"], &[("rotate", "true"), ("kind", "plain")]);
            assert_eq!(rotate_on(&place, &store, &args, &handover_wait(&dial)), 1);
            assert!(place.opened.borrow().is_empty());
            stop_being_a_seat();
            let _ = std::fs::remove_dir_all(&store.root);
        }

        // A caller that holds nothing at all, naming a seat somebody else
        // holds. The refusal names the holder - and stops, where a fall-through
        // here would seat a successor for a handover nobody asked this pane to
        // make.
        {
            let (_env, store) = rotating_as("rotate-unseated", "w3", "w3:p1");
            store.save_project(&Project::new("core")).unwrap();
            cmd_govern::take(&store, "core", "w1", "w1:p9");
            let dial = util::Dial::new();
            let place = Seats::of(vec![Ok(State::Idle)]);
            let args = Args::synth("govern", &["core"], &[("rotate", "true"), ("kind", "plain")]);
            assert_eq!(rotate_on(&place, &store, &args, &handover_wait(&dial)), 1);
            assert!(place.opened.borrow().is_empty(), "{:?}", place.opened.borrow());
            assert!(store.handovers().is_empty());
            stop_being_a_seat();
            let _ = std::fs::remove_dir_all(&store.root);
        }

        // A successor whose work order wsp could never see taken. Moving the
        // slot into one would be exactly the promise the verb refuses to end
        // anything on, so it is refused before the seat opens - and `opencode`
        // is not merely an example here: it is the one kind that takes its
        // order in argv today.
        {
            let (_env, store) = rotating_as("rotate-argv-kind", "w1", "w1:p9");
            store.save_project(&Project::new("core")).unwrap();
            cmd_govern::take(&store, "core", "w1", "w1:p9");
            let dial = util::Dial::new();
            let place = Seats::of(vec![Ok(State::Idle)]);
            let args = Args::synth("govern", &["core"], &[("rotate", "true"), ("kind", "opencode")]);
            assert_eq!(rotate_on(&place, &store, &args, &handover_wait(&dial)), 2);
            assert!(place.opened.borrow().is_empty(), "no successor was seated");
            stop_being_a_seat();
            let _ = std::fs::remove_dir_all(&store.root);
        }

        // A run whose last barrier has been passed: `next` offers the rotate
        // line only while `n < of`, and the verb refuses on the same fact -
        // seating a successor at the end of a run seats an agent with nothing
        // left to sequence.
        {
            let (_env, store) = rotating_as("rotate-finished", "w1", "w1:p9");
            store.save_project(&Project::new("core")).unwrap();
            let mut t = Task::new("landed", "t-1");
            t.project = Some("core".into());
            t.set_status(Status::Done);
            store.save_task(&t).unwrap();
            let mut w = Worklist::new("batch", "Overnight batch");
            w.set_status(WorklistStatus::Running);
            w.set_groups(&[Group {
                members: vec!["t-1".into()],
                cap: None,
                stop: String::new(),
                verdict: "2026-08-20T00:00:00Z clean".into(),
                landed: Vec::new(),
            }]);
            store.save_worklist(&w).unwrap();
            cmd_govern::take(&store, "batch", "w1", "w1:p9");

            let dial = util::Dial::new();
            let place = Seats::of(vec![Ok(State::Idle)]);
            let args = Args::synth("govern", &["batch"], &[("rotate", "true"), ("kind", "plain")]);
            assert_eq!(rotate_on(&place, &store, &args, &handover_wait(&dial)), 1);
            assert!(place.opened.borrow().is_empty(), "no successor was seated");
            assert_eq!(
                cmd_govern::governs(&store.governors(), "w1", Some("w1:p9")).as_deref(),
                Some("batch"),
                "the caller keeps the seat",
            );
            stop_being_a_seat();
            let _ = std::fs::remove_dir_all(&store.root);
        }
    }

    /// The boundary from the other side: with a group still owed behind the
    /// barrier just passed, the rotation goes ahead - which is the ordinary
    /// per-barrier case, scoped to a worklist rather than a project.
    #[test]
    fn a_run_with_a_group_still_owed_behind_the_barrier_still_rotates() {
        use crate::model::{Group, Task, Worklist, WorklistStatus};

        let (_env, store) = rotating_as("rotate-midrun", "w1", "w1:p9");
        let sock = _env.path("herdr.sock");
        herdr_stand_in(&sock, 24, successor_pane());
        std::env::set_var("HERDR_SOCKET_PATH", &sock);

        store.save_project(&Project::new("core")).unwrap();
        let mut landed = Task::new("landed", "t-1");
        landed.project = Some("core".into());
        landed.set_status(crate::model::Status::Done);
        store.save_task(&landed).unwrap();
        let mut owed = Task::new("still running", "t-2");
        owed.project = Some("core".into());
        store.save_task(&owed).unwrap();

        let mut w = Worklist::new("batch", "Overnight batch");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[
            Group {
                members: vec!["t-1".into()],
                cap: None,
                stop: String::new(),
                verdict: "2026-08-20T00:00:00Z clean".into(),
                landed: Vec::new(),
            },
            Group {
                members: vec!["t-2".into()],
                cap: None,
                stop: String::new(),
                verdict: String::new(),
                landed: Vec::new(),
            },
        ]);
        store.save_worklist(&w).unwrap();
        cmd_govern::take(&store, "batch", "w1", "w1:p9");

        let dial = util::Dial::new();
        let place = Seats::of(vec![Ok(State::Idle), Ok(State::Working)]);
        let args = Args::synth("govern", &["batch"], &[("rotate", "true"), ("kind", "plain")]);
        let code = rotate_on(&place, &store, &args, &handover_wait(&dial));
        stop_being_a_seat();

        assert_eq!(code, 0);
        let rec = &store.governors()["batch"];
        assert_eq!(rec["workspace"], "w9");
        assert_eq!(store.handovers()["batch"]["to"], "w9:p2");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// The inherited ending is consumed by being done. Once the pane a record
    /// names has been despawned - or was already gone, which is the same fact
    /// to the verb - the record must go with it, or every later brief of that
    /// successor orders an ending that already happened.
    #[test]
    fn despawning_the_predecessor_consumes_the_ending_it_was_told_to_do() {
        let _env = no_backend();
        let store = seat("rotate-consume");
        working(&store, "t-260816-095", "w1:p1");
        store.set_handover("core", json!({ "from": "w1:p1", "to": "w9:p2" }));
        // Another rotation, aimed at a different pane: untouched.
        store.set_handover("verb", json!({ "from": "w8:p8", "to": "w9:p3" }));

        let place = Ends::ok();
        let tidied = Tidied::default();
        let code =
            end_work(&place, &store, &Args::synth("despawn", &[], &[("pane", "w1:p1")]), Caller::default(), &tidied.f());

        assert_eq!(code, 0);
        let left = store.handovers();
        assert!(!left.contains_key("core"), "the ending that was owed is done: {left:?}");
        assert!(left.contains_key("verb"), "somebody else's ending is not this verb's to spend: {left:?}");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// The window `rotate` holds open, and the one move that ruins it.
    ///
    /// `confirm_turn` returns when the successor's turn *starts*, and `take`
    /// runs after it — so the successor's first turn and the slot's move are in
    /// flight together. A successor acting on its session-start text without
    /// re-reading can despawn its predecessor inside that window, killing the
    /// pane on its way to `take`: the slot never moves, the predecessor is
    /// gone, and a live successor holds a record naming a pane that no longer
    /// exists. A vacancy produced by the one verb built to prevent one.
    ///
    /// Refused rather than sequenced by prose, which is the whole argument of
    /// `core-050`. The seat guard below already stopped the plain call — the
    /// predecessor does still hold the slot — but it stopped it with the wrong
    /// sentence, naming `--force` and `wsp govern --clear` as the way through,
    /// and both of those are the damage. So the refusal that matters is the
    /// forced one: that is the assertion that fails without this guard.
    #[test]
    fn a_successor_cannot_end_its_predecessor_before_the_slot_has_moved() {
        let _env = no_backend();
        let store = seat("rotate-early");
        working(&store, "t-260816-095", "w1:p1");
        cmd_govern::take(&store, "core", "w1", "w1:p1");
        store.set_handover("core", json!({ "from": "w1:p1", "to": "w9:p2" }));

        let place = Ends::ok();
        let tidied = Tidied::default();
        let ending = Args::synth("despawn", &[], &[("pane", "w1:p1")]);
        // The successor, mid-rotation: named `to`, holding no slot yet.
        let early = Caller { pane: Some("w9:p2"), governs: None };
        assert_eq!(end_work(&place, &store, &ending, early, &tidied.f()), 1);
        assert!(place.asked.borrow().is_empty(), "nothing was ended");
        assert!(store.handovers().contains_key("core"), "and the ending is still owed");
        // The gap. `--force` skips the seat guard, which is the door the seat
        // guard's own message sends an agent to — and forcing here kills the
        // pane that is still on its way to `take`.
        let forced = Args::synth("despawn", &[], &[("pane", "w1:p1"), ("force", "true")]);
        assert_eq!(end_work(&place, &store, &forced, early, &tidied.f()), 1);
        assert!(place.asked.borrow().is_empty(), "--force is not the way out of this one");

        // Once the slot has moved, the ending is the successor's to do — this
        // is the same call, one state later, and it goes through.
        cmd_govern::take(&store, "core", "w9", "w9:p2");
        let now = Caller { pane: Some("w9:p2"), governs: Some("core") };
        assert_eq!(end_work(&place, &store, &ending, now, &tidied.f()), 0);
        assert_eq!(place.asked.borrow().len(), 1, "the predecessor was ended");
        assert!(!store.handovers().contains_key("core"), "and the record is consumed");

        let _ = std::fs::remove_dir_all(&store.root);
    }

}
