//! `wsp worklist` — composing a queue of groups, running it, and the barrier
//! between the two.
//!
//! The record is [`crate::model::Worklist`] and where it is up to is
//! [`crate::worklist`]; this is the noun in between, with the same shape as
//! `project` and `machine` — a subcommand, a slug, and `ls` when nothing else
//! is said. Two halves, and the line between them is the barrier: **composing**
//! is `new`, `add`, `rm`, `mv`, `group`, `edit` and the two reading verbs;
//! **running** is `next`, `go`, `hold`, `followup`, `done`.
//!
//! # The barrier, which is what the second half is
//!
//! A queue of groups has a barrier between every pair of groups, and a run is
//! the sequence of them being passed. [`next`] says which of four things is
//! true of the one in front — members may start, members are going, the barrier
//! is shut, or there is nothing left — and each answer names the one command to
//! run next. [`go`] passes a barrier. [`hold`] stops the queue handing out any
//! more work, and takes nothing back. [`done`] closes the list.
//!
//! **Nothing spawns.** `next` names what may start and the governor runs `wsp
//! spawn` per member: the moment the queue spawns it needs a spawn policy —
//! kind, tier, machine, mandate — and every one of those was a judgement in all
//! three real runs.
//!
//! Two facts are written by the running half and neither is a position. The
//! list's `status` is one — to start, to stop, to be finished with it — and
//! [`crate::model::Group::verdict`] is the other, which is the sentence
//! somebody wrote to pass a barrier. Both are *decisions*; where the run has
//! got to is still derived and still never written.
//!
//! # The write-ahead-only window, and why it needs no field
//!
//! A worklist is composed by an agent and read by a person, and a person may
//! edit it **only ahead of the work**. That is not a permission model. Editing
//! a group that has already run rewrites history; editing the group being run
//! changes the membership of a barrier that is already being waited on, which
//! is the `batch` handbook failure — the written plan disagreeing with what is
//! actually happening — with the disagreement now inside one record instead of
//! between two.
//!
//! So a running list is not read-only, it is **write-ahead-only**, and the line
//! falls out of the derived position rather than being stored: frozen is
//! *ordinal at or behind the position*, and it moves on its own as the run
//! advances. Every one of `add`, `rm`, `mv` and `group` asks [`Window`] first.
//!
//! Two fields of the group being run are not yet in flight, and stay open:
//! a member's own line until that member starts ([`member`]), and the stop
//! until its barrier row opens ([`amend_running_stop`], which takes a logged
//! reason). Each one is read once, at a moment the run has not reached yet.
//!
//! One more edit reaches the running group, and it is not a person's. The
//! group's own barrier check may add rows to it with `followup --blocking`
//! ([`followup`], `wsp-210`). The window protects the barrier being waited on,
//! and here the barrier is the one doing the adding.
//!
//! Read with [`Reading::Settled`], never with `Landed`. The window is decided
//! on a path a person types at interactively, and the landed reading is a git
//! process per member; the settled reading is free, and being one group out at
//! the moment somebody is composing group 5 costs nothing that matters. The
//! barrier is the caller that has to be right, and it pays for that itself.
//!
//! **A refusal that only says no is the notification failure again**, so
//! [`Window::refuse`] names which group the list is up to, what is holding it,
//! and which groups may be edited instead. A person told "no" goes and edits
//! the file by hand; a person told "groups 3 to 5 are still ahead of the work"
//! does the thing that was wanted.
//!
//! # What the reading verbs cost
//!
//! `ls` and `show` are what an agent runs repeatedly, and everything an agent
//! runs repeatedly is paid for in context on every request of every session. So
//! both print ids and status words and nothing else — no titles, since the
//! caller is about to run `wsp spawn <id>`, which prints the title itself — and
//! `--json` carries the rest for whatever would rather parse it.

use std::collections::BTreeSet;

use serde_json::json;

// The same fold, and not a second copy of it: this is one act — prose arriving
// on several lines and being stored on one — and `## Groups` and `## Log` are
// both parsed line by line, so a divergence between two implementations would
// be a divergence in what each section can hold. `worklist-045` made it
// `pub(crate)` for its second caller; this is the third.
use crate::cmd_task::fold;
use crate::model::{Group, Landed, Worklist, WorklistStatus, DEFAULT_POLICY, MANUAL};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::worklist::{self, Landing, Position, Reading, Segment, Standing};
use crate::Args;

pub fn dispatch(store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(|s| s.as_str()).unwrap_or("ls") {
        // `new` and `add` are two verbs here where `project` has one, because a
        // worklist has two things to add to: itself, and the list inside it.
        // `wsp worklist add batch` with nothing after it is the mistake that
        // makes, and it is answered by name below.
        "new" | "create" => new(store, args),
        "add" => add(store, args),
        "rm" | "remove" => rm(store, args),
        "mv" | "move" => mv(store, args),
        "group" => group(store, args),
        "member" => member(store, args),
        "edit" => edit(store, args),
        "ls" | "list" => list(store, args),
        "show" | "get" => show(store, args),
        // Running it, which is the barrier. Everything above composes a plan.
        "next" => next(store, args),
        "go" | "start" => go(store, args),
        "hold" | "stop" => hold(store, args),
        // A barrier check's third answer: go or hold, with a short tail of
        // rows attached to the run. `wsp-210`.
        "followup" | "follow-up" => followup(store, args),
        // A person's pause, and its way back. Not `hold`/`go`: those are the
        // barrier's "does not pass" and its pass — see [`park`].
        "park" | "pause" => park(store, args),
        "resume" | "unpark" => resume(store, args),
        "done" | "finish" => done(store, args),
        // `wsp-134`: the steps wsp takes itself. Started by the verbs that
        // make one due, and by hand to repair a trigger that was lost.
        "advance" => crate::cycle::advance(store, args),
        other => {
            eprintln!("wsp worklist: unknown subcommand `{other}`");
            2
        }
    }
}

/// Resolve a typed slug: exact, then unique prefix. No fuzzy title match — a
/// worklist is named by whoever made it and a guess here ends up editing the
/// wrong plan.
fn find(store: &Store, needle: &str) -> Option<Worklist> {
    if let Some(w) = store.worklist(needle) {
        return Some(w);
    }
    let all = store.worklists();
    let mut hits = all.iter().filter(|w| w.id.starts_with(needle));
    let first = hits.next()?.clone();
    hits.next().is_none().then_some(first)
}

/// The worklist a needle names, or the sentence saying why it named none.
fn worklist_or_why(store: &Store, needle: &str) -> Result<Worklist, String> {
    if let Some(w) = find(store, needle) {
        return Ok(w);
    }
    let names: Vec<String> = store.worklists().into_iter().map(|w| w.id).collect();
    if names.is_empty() {
        return Err(format!("wsp: no worklist `{needle}` — there are none yet, and `wsp worklist new {needle} \"title\"` makes one"));
    }
    // Named rather than counted, and truncated rather than wrapped: with three
    // lists the names *are* the answer, and with thirty the first few plus
    // `worklist ls` is.
    Err(format!(
        "wsp: no worklist matching `{needle}` — there is {}",
        util::truncate(&names.join(", "), 60)
    ))
}

// ---- the window -------------------------------------------------------

/// Where the line between what has run and what has not sits, right now.
///
/// Nothing here is stored and nothing here is a permission. It is the derived
/// position with one question asked of it — *may a hand still change this
/// group* — and it moves on its own as the run advances.
struct Window {
    /// The list, as this window was read for it.
    id: String,
    /// Has the run begun at all? A `draft` list is entirely ahead of the work
    /// by definition, whatever its members' statuses happen to say.
    ///
    /// This is the one thing the position alone gets wrong, and it is worth the
    /// exception. A governor composes a plan out of the backlog, and the
    /// backlog legitimately holds work already at `review` — design-only tasks
    /// reach it before anything is spawned at all. Under a status-blind rule
    /// the person reading that plan would be refused on group 1 *because it is
    /// finished*, on a list that has never been started, which is a false
    /// refusal on exactly the reading pass the design exists to invite. The
    /// window protects a barrier being waited on; a draft has no barrier.
    started: bool,
    /// The first group not finished, 1-based, under [`Reading::Settled`].
    at: Option<usize>,
    of: usize,
    /// The members of the group at `at`, so a refusal can name what is holding
    /// it without asking the store a second time.
    members: Vec<Standing>,
}

/// The window, through [`worklist::reached`] rather than off the raw position, and that
/// matters in the one direction that loses something. A position that has
/// slipped back onto a group already passed reports a *smaller* ordinal, which
/// makes `first_open` smaller, which unfreezes the group actually being run —
/// the frozen-window failure arrived at from underneath. The verdict floor
/// stops it, and the reading is free either way.
fn window(store: &Store, w: &Worklist) -> Window {
    let p: Position = worklist::position(store, w, Reading::Settled);
    Window {
        id: w.id.clone(),
        started: w.status() != WorklistStatus::Draft,
        at: p.at,
        of: p.of,
        members: p.members,
    }
}

impl Window {
    /// The lowest ordinal a hand may still edit. Groups at or behind the
    /// position are frozen, and a group one past the end is always open —
    /// appending is the one edit that can never disagree with anything.
    fn first_open(&self) -> usize {
        if !self.started {
            return 1;
        }
        match self.at {
            Some(at) => at + 1,
            None => self.of + 1,
        }
    }

    fn allows(&self, ordinal: usize) -> bool {
        ordinal >= self.first_open()
    }

    /// Say no, and say what may be done instead.
    ///
    /// Three lines at most: what the refusal is about, what the list is
    /// actually waiting on, and which groups are still open. The second line is
    /// the one that stops this being a wall — somebody refused on group 2 wants
    /// to know whether group 2 is nearly done or stuck on a member nobody has
    /// started, and the answer is already in hand.
    fn refuse(&self, ordinal: usize, what: &str) -> i32 {
        let behind = self.at.is_some_and(|at| ordinal < at) || self.at.is_none();
        if behind {
            eprintln!(
                "wsp: {what} group {ordinal} of `{}` — it is behind the run, and editing it rewrites what has already happened",
                self.id
            );
        } else {
            eprintln!(
                "wsp: {what} group {ordinal} of `{}` — it is the group being run, and its membership is what the barrier is waiting on",
                self.id
            );
        }

        match self.at {
            Some(at) => {
                let holding: Vec<String> = self
                    .members
                    .iter()
                    .filter(|s| !s.finished())
                    .map(|s| format!("{} {}", s.id, s.settlement.word()))
                    .collect();
                if holding.is_empty() {
                    // Nothing holding it and still the position: its work is
                    // done and its barrier has not been passed, which is a
                    // better answer than "where it is up to" for somebody who
                    // has just been refused an edit to the group behind it.
                    eprintln!(
                        "     group {at} of {} is settled and its barrier has not been passed",
                        self.of
                    );
                } else {
                    // Truncated rather than wrapped: a group of eleven is a
                    // group whose count is the useful part, and the first few
                    // ids are what somebody goes and looks at.
                    eprintln!(
                        "     group {at} of {} is waiting on {}",
                        self.of,
                        util::truncate(&holding.join(" · "), 88)
                    );
                }
            }
            None => eprintln!("     every one of its {} groups has finished", self.of),
        }

        let first = self.first_open();
        if first < self.of {
            eprintln!(
                "     groups {first} to {} are still ahead of the work — wsp worklist show {}",
                self.of, self.id
            );
        } else if first == self.of {
            eprintln!("     group {first} is still ahead of the work — wsp worklist show {}", self.id);
        } else {
            eprintln!(
                "     nothing in it is still ahead of the work — `wsp worklist add {} <task>…` puts a new group at the end",
                self.id
            );
        }
        1
    }
}

// ---- new --------------------------------------------------------------

pub fn new(store: &Store, args: &Args) -> i32 {
    let Some(raw) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp worklist new <slug> \"title\"");
        return 2;
    };
    // Slugified on the way in, exactly as `project add` does it. The two share
    // one key space, and a key space where one side can name something the
    // other cannot is not one key space.
    let slug = util::slugify(&raw);
    if slug.is_empty() {
        eprintln!("wsp: `{raw}` does not reduce to a usable slug");
        return 2;
    }
    if let Some(why) = store.scope_taken(&slug, None) {
        eprintln!("wsp: `{slug}` is not free: {why}");
        eprintln!("     a worklist slug and a project id are one name, because a governor seat is keyed on it");
        return 1;
    }

    let title = match args.text(2) {
        t if t.trim().is_empty() => raw.clone(),
        t => t,
    };
    // Frontmatter, so the check is not about taste: a control byte on a
    // `title:` line is not an ugly title, it is a file the parser reads
    // differently.
    if let Some(why) = util::terminal_output(&title) {
        eprintln!("wsp: {why}");
        return 2;
    }

    let w = Worklist::new(&slug, &title);
    if let Err(e) = store.save_worklist(&w) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("worklist-added", json!({ "id": w.id }));
    store.git_commit(&format!("wsp: add worklist {}", w.id));

    if args.json() {
        println!("{}", worklist_json(store, &w));
    } else {
        println!("added worklist {} ({})", w.id, w.title);
        println!("next: wsp worklist add {} <task>…   each call is one group", w.id);
    }
    0
}

// ---- add --------------------------------------------------------------

pub fn add(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "usage: wsp worklist add <slug> <task>… [--group N] [--agent \"kind [model] [effort]\"]\n       wsp worklist add <slug> <parent> --sub";
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("{USAGE}");
        eprintln!("       (`wsp worklist new <slug> \"title\"` makes the list itself)");
        return 2;
    };
    let mut w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };

    let typed: Vec<String> = args.rest.iter().skip(2).cloned().collect();
    if typed.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
    let members = if args.has("sub") { match sub_tasks(store, &typed) {
            Ok(m) => m,
            Err(code) => return code,
        }
    } else {
        match resolve_members(store, &typed) {
            Ok(m) => m,
            Err(code) => return code,
        }
    };

    let mut groups = w.groups();
    if let Some(code) = already_in(&groups, &members, &w.id) {
        return code;
    }

    // Where it lands. No `--group` means a group of its own at the end, which
    // is how a list is composed: one call, one group, in the order they were
    // typed. `--group N` reaches into an existing group, and `N` one past the
    // end is the same thing as no flag at all.
    let win = window(store, &w);
    let ordinal = match args.get("group") {
        Some(v) => match ordinal_of(&v, groups.len() + 1) {
            Ok(n) => n,
            Err(code) => return code,
        },
        None => groups.len() + 1,
    };
    if !win.allows(ordinal) {
        return win.refuse(ordinal, "will not add to");
    }

    let new_group = ordinal > groups.len();
    if new_group {
        let agent = match agent_for_new(args, &groups, groups.len()) {
            Ok(a) => a,
            Err(code) => return code,
        };
        groups.push(Group { members: members.clone(), agent, ..Group::default() });
    } else {
        groups[ordinal - 1].members.extend(members.iter().cloned());
        // Joining a group, `--agent` is who runs what was just added — its
        // members' own lines, `wsp-150` — since the group already has one.
        if let Some(v) = args.get("agent") {
            match policy_word(&v) {
                Ok(a) if a == MANUAL => {
                    eprintln!("wsp: a member is not run by hand on its own — whether wsp runs a group is the group's line: `wsp worklist group {} {ordinal} --agent {MANUAL}`", w.id);
                    return 2;
                }
                Ok(a) => join_with_agent(&mut groups[ordinal - 1], &members, &a),
                Err(code) => return code,
            }
        }
    }

    // The log names the members and the ordinal names itself. Ordinals are
    // positions, rewritten on every write, so "added to group 3" is a sentence
    // that stops being true the moment a group is inserted above it — the ids
    // are the half that stays true, and they lead.
    w.log(&format!(
        "{} → group {ordinal}{}",
        members.join(" "),
        if new_group { " (new)" } else { "" }
    ));
    if args.has("sub") {
        // Said out loud because `--sub` is a resolution and not a rule: what
        // went in is what was open when it was typed, and a sub-task added
        // under that parent tomorrow is not in this list. A group that grows
        // under a governor at 3am is the stale-plan failure inverted.
        w.log(&format!("--sub {} resolved to the {} open now, not live", typed[0], members.len()));
    }
    save(store, &mut w, &groups, "add", &format!("add {}", members.join(" ")));

    if args.json() {
        println!("{}", worklist_json(store, &w));
    } else {
        println!(
            "{} → {} group {ordinal} of {}",
            members.join("  "),
            w.id,
            w.groups().len()
        );
        if new_group {
            println!("  {}", Paint::new().dim(&agent_line(&groups[ordinal - 1])));
        }
    }
    0
}

/// The `agent:` line a group created now is given — `wsp-134`, and Ed's
/// "configured at the creation of the group, with a sensible default".
///
/// `--agent` when it is said. Otherwise the group before it, so a list
/// running on opencode goes on running on opencode without anybody having to
/// remember to say so each time; and only when there is no group before it,
/// [`DEFAULT_POLICY`].
///
/// **A group before it with no line is inherited too, as no line.** That is a
/// list composed before `wsp-134`, run by a governor spawning by hand under
/// rules its prose carries and this file does not — ux-revamp's opencode-only
/// floor was one. Defaulting its next group to `claude` would have wsp start
/// claude agents in the middle of that run. Such a list turns wsp on with
/// `--agent`, said once.
fn agent_for_new(args: &Args, groups: &[Group], at: usize) -> Result<String, i32> {
    if let Some(v) = args.get("agent") {
        return policy_word(&v);
    }
    // A group put in front of all the others (`mv --after 0`) has none before
    // it, and takes the line of the group it now stands in front of: the list
    // is the same list, and on one from before this that line is none.
    let neighbour = at.checked_sub(1).and_then(|i| groups.get(i)).or_else(|| groups.get(at));
    Ok(match neighbour {
        Some(g) => g.agent.trim().to_string(),
        None => DEFAULT_POLICY.to_string(),
    })
}

/// A typed `--agent`: `manual`, or a kind followed by a model and an effort
/// in either order. The kind is not checked against a list here, for
/// [`crate::agent_commands::of`]'s reason: the backend refuses an unknown kind
/// with its whole catalogue, which is a better list than one kept in wsp.
fn policy_word(v: &str) -> Result<String, i32> {
    let v = v.trim();
    if v.is_empty() || v == "true" {
        eprintln!("wsp: --agent names who runs the group — `claude`, `opencode <model>`, `claude sonnet medium`, or `{MANUAL}`");
        return Err(2);
    }
    Ok(v.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// A group's member ids, each with its own agent line beside it where it has
/// one — `wsp-150`. Beside the id rather than on a line of its own, because
/// the override is a fact about that member and a reader scanning the group
/// for who runs on what is reading the ids.
fn member_ids(p: &Paint, g: &Group) -> String {
    g.members
        .iter()
        .map(|m| match g.member_agents.get(m) {
            Some(a) => format!("{m} {}", p.dim(&format!("({a})"))),
            None => m.clone(),
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// What a reader is told about who runs a group, in one line.
fn agent_line(g: &Group) -> String {
    match g.policy() {
        Some(_) if !g.member_agents.is_empty() => format!(
            "agent: {} — wsp spawns its members, verifies each on the kind it ran on, and checks the barrier; a member marked beside its id runs on its own",
            g.agent
        ),
        Some(_) => format!("agent: {} — wsp spawns its members, verifies each, and checks the barrier", g.agent),
        None => "agent: manual — the governor spawns this group by hand".to_string(),
    }
}

/// `--sub <parent>`: the parent's **open** sub-tasks, as they stand at the
/// moment the flag is typed.
///
/// One parent, because the flag names a piece of work that was already
/// decomposed — two of the three hand-run groups were exactly that — and a list
/// of parents would be a different verb wearing this one's name.
///
/// Direct children only. `descendants_of` would drag a grandchild into a group
/// beside its own parent, which is two tasks that certainly touch the same
/// files, and that is the one composition rule the whole design rests on.
fn sub_tasks(store: &Store, typed: &[String]) -> Result<Vec<String>, i32> {
    if typed.len() != 1 {
        eprintln!("wsp: --sub takes one parent — its open sub-tasks are the group");
        return Err(2);
    }
    let parent = match store.task_or_why(&typed[0]) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return Err(1);
        }
    };
    let tasks = store.tasks();
    let kids: Vec<String> = crate::resolve::children_of(&tasks, &parent.id)
        .into_iter()
        .filter(|t| t.status().is_open())
        .map(|t| t.id.clone())
        .collect();
    if kids.is_empty() {
        // Named rather than silently adding nothing: an empty group is a
        // barrier that opens on the first look, and a decomposition that has
        // not happened yet is worth being told about while there is time.
        eprintln!("wsp: `{}` has no open sub-tasks — nothing to make a group of", parent.id);
        return Err(1);
    }
    Ok(kids)
}

/// Typed references to the ids the record holds.
///
/// Resolved here and stored resolved, because a worklist references tasks by id
/// and every reader of it — the position, the barrier, the panel — looks them up
/// by exactly that. A suffix or a title fragment that resolves today is a
/// dangling member the day somebody files a second task the words fit.
fn resolve_members(store: &Store, typed: &[String]) -> Result<Vec<String>, i32> {
    let mut out: Vec<String> = Vec::new();
    for needle in typed {
        match store.task_or_why(needle) {
            Ok(t) => {
                if out.contains(&t.id) {
                    eprintln!("wsp: `{needle}` names {}, which is already in this call", t.id);
                    return Err(2);
                }
                out.push(t.id);
            }
            Err(why) => {
                eprintln!("{why}");
                return Err(1);
            }
        }
    }
    Ok(out)
}

/// Refuse a member the list already holds, naming where it is.
///
/// A task in two groups of one list holds two barriers, which cannot both be
/// what anybody meant, and the second one would never open on work that landed
/// for the first. `mv` is how a member changes group.
fn already_in(groups: &[Group], members: &[String], id: &str) -> Option<i32> {
    for m in members {
        if let Some((i, _)) = groups.iter().enumerate().find(|(_, g)| g.members.contains(m)) {
            eprintln!("wsp: {m} is already in group {} of `{id}` — wsp worklist mv {id} {m} --group N", i + 1);
            return Some(1);
        }
    }
    None
}

/// A typed group number, against how far the list reaches.
fn ordinal_of(v: &str, max: usize) -> Result<usize, i32> {
    match v.trim().parse::<usize>() {
        Ok(n) if n >= 1 && n <= max => Ok(n),
        Ok(_) => {
            eprintln!("wsp: there is no group {v} — the list reaches {max}");
            Err(2)
        }
        Err(_) => {
            eprintln!("wsp: a group is a number, not `{v}`");
            Err(2)
        }
    }
}

// ---- rm ---------------------------------------------------------------

pub fn rm(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp worklist rm <slug> <task>…");
        return 2;
    };
    let mut w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let typed: Vec<String> = args.rest.iter().skip(2).cloned().collect();
    if typed.is_empty() {
        eprintln!("usage: wsp worklist rm <slug> <task>…");
        return 2;
    }

    // `-n`, and this verb is the cheapest real dry run in wsp: the loop below
    // touches nothing but `groups`, which is a copy, and the single `save`
    // after it is the whole act. So the preview is this function with one call
    // skipped, rather than a second account of what it would have done — which
    // is the arrangement `checkout --rm -n` had to build machinery to get.
    let dry = args.has("dry-run");
    let mut groups = w.groups();
    let win = window(store, &w);
    let mut gone: Vec<String> = Vec::new();
    // The 1-based position each group had *before* any of this, carried in
    // lockstep so a dropped group can be named by the number the reader can see
    // in `wsp worklist show`. The live ordinals shift as groups go, which is
    // exactly why the reported one must not be read off the mutated list.
    let mut ordinal: Vec<usize> = (1..=groups.len()).collect();
    let mut emptied: Vec<usize> = Vec::new();
    // The window is read once and the ordinals shift underneath it, which is
    // safe rather than lucky: a group is only ever dropped after it has passed
    // the check, so every drop is ahead of the position, and dropping a group
    // ahead of the position renumbers only the groups after it — which are
    // ahead of it too. Nothing frozen can be pulled into reach.
    for needle in &typed {
        // Against the membership rather than against the store, and the store
        // second. A member the store has never heard of is exactly the one this
        // has to be able to remove — an archived task is a dangling id nothing
        // else here is allowed to take out.
        let id = member_named(&groups, needle, store);
        let Some((gi, id)) = id else {
            eprintln!("wsp: `{needle}` is not in `{}` — wsp worklist show {}", w.id, w.id);
            return 1;
        };
        if !win.allows(gi + 1) {
            return win.refuse(gi + 1, "will not remove from");
        }
        groups[gi].members.retain(|m| m != &id);
        groups[gi].member_agents.remove(&id);
        // A group emptied by hand is a barrier that opens on the first look,
        // which is a group that no longer means anything. It goes, and it is
        // said so rather than left as a numbered blank.
        if groups[gi].members.is_empty() {
            groups.remove(gi);
            emptied.push(ordinal.remove(gi));
        }
        gone.push(id);
    }

    if !dry {
        w.log(&format!("removed {}", gone.join(" ")));
        save(store, &mut w, &groups, "rm", &format!("rm {}", gone.join(" ")));
    }

    if args.json() {
        // The list as it stands, which under `-n` is the list as it stands
        // *unchanged* — the preview's answer is the two fields beside it, and
        // reporting the groups this call computed but did not save would be a
        // record of a list that does not exist.
        let mut out = worklist_json(store, &w);
        if let Some(o) = out.as_object_mut() {
            o.insert("removed".into(), json!(gone));
            o.insert("groups_dropped".into(), json!(emptied));
            o.insert("dry_run".into(), json!(dry));
        }
        println!("{}", out);
    } else {
        // Named rather than counted, because a group number is what the reader
        // then has to type — `worklist mv --group N`, `worklist group <slug> N`
        // — and because dropping group 2 of 4 is what renumbers 3 and 4. A
        // count says something went and leaves the renumbering to be
        // discovered.
        let did = if dry { "would remove" } else { "removed" };
        let left = if dry { "would be left empty and dropped" } else { "left empty and dropped" };
        let tail = match emptied.as_slice() {
            [] => String::new(),
            [n] => format!(" · group {n} {left}"),
            ns => {
                let ns = ns.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
                format!(" · groups {ns} {left}")
            }
        };
        println!("{did} {} from {}{tail}", gone.join("  "), w.id);
    }
    0
}

/// Which group a typed reference names a member of, and the id it is stored
/// under.
///
/// Two passes, and the order is the point. An id written in the file matches
/// first, so a member the store has forgotten is still removable; only then is
/// the store asked to turn a suffix or a title fragment into an id.
fn member_named(groups: &[Group], needle: &str, store: &Store) -> Option<(usize, String)> {
    for (i, g) in groups.iter().enumerate() {
        if g.members.iter().any(|m| m == needle) {
            return Some((i, needle.to_string()));
        }
    }
    let id = store.find_task(needle)?.id;
    groups.iter().position(|g| g.members.contains(&id)).map(|i| (i, id))
}

// ---- mv ---------------------------------------------------------------

pub fn mv(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "usage: wsp worklist mv <slug> <task> --group N   (or --after N for a new group)";
    let (Some(needle), Some(who)) = (args.rest.get(1).cloned(), args.rest.get(2).cloned()) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let mut w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };

    let mut groups = w.groups();
    let Some((from, id)) = member_named(&groups, &who, store) else {
        eprintln!("wsp: `{who}` is not in `{}` — wsp worklist show {}", w.id, w.id);
        return 1;
    };

    let win = window(store, &w);
    if !win.allows(from + 1) {
        return win.refuse(from + 1, "will not take a member out of");
    }

    // `--group` joins a group that exists; `--after` makes one that does not.
    // `--after 0` is the only way to put a group in front of everything, and it
    // is refused by the window on any list that has started, which is correct.
    let (mut at, fresh) = match (args.get("group"), args.get("after")) {
        (Some(_), Some(_)) => {
            eprintln!("wsp: --group joins a group and --after makes one — not both");
            return 2;
        }
        (Some(v), None) => match ordinal_of(&v, groups.len() + 1) {
            Ok(n) => (n, n > groups.len()),
            Err(code) => return code,
        },
        (None, Some(v)) => match v.trim().parse::<usize>() {
            Ok(n) if n <= groups.len() => (n + 1, true),
            Ok(_) => {
                eprintln!("wsp: there is no group {v} to go after — the list reaches {}", groups.len());
                return 2;
            }
            Err(_) => {
                eprintln!("wsp: a group is a number, not `{v}`");
                return 2;
            }
        },
        (None, None) => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    if !win.allows(at) {
        return win.refuse(at, "will not move a member into");
    }
    if !fresh && at == from + 1 {
        println!("{id} is already in group {at} of {}", w.id);
        return 0;
    }

    groups[from].members.retain(|m| m != &id);
    // A member's own line goes where it goes: it is about the member, and the
    // group it lands in has no opinion on it.
    let own = groups[from].member_agents.remove(&id);
    // The source emptying shifts everything after it up one, including the
    // destination that was just named. Corrected here rather than by refusing
    // the move: a group with one member in it is the ordinary case for the
    // serial half of a list, and moving that member is the ordinary edit.
    let mut emptied = false;
    if groups[from].members.is_empty() {
        groups.remove(from);
        emptied = true;
        if at > from + 1 {
            at -= 1;
        }
    }

    if fresh {
        let agent = match agent_for_new(args, &groups, at - 1) {
            Ok(a) => a,
            Err(code) => return code,
        };
        groups.insert(at - 1, Group { members: vec![id.clone()], agent, ..Group::default() });
    } else {
        groups[at - 1].members.push(id.clone());
    }
    if let Some(a) = own.filter(|a| *a != groups[at - 1].agent.trim()) {
        groups[at - 1].member_agents.insert(id.clone(), a);
    }

    w.log(&format!("{id} → group {at}{}", if fresh { " (new)" } else { "" }));
    save(store, &mut w, &groups, "mv", &format!("mv {id} to group {at}"));

    if args.json() {
        println!("{}", worklist_json(store, &w));
    } else {
        let tail = if emptied { " · the group it left was emptied and dropped" } else { "" };
        println!("{id} → {} group {at} of {}{tail}", w.id, groups.len());
    }
    0
}

// ---- group ------------------------------------------------------------

pub fn group(store: &Store, args: &Args) -> i32 {
    const USAGE: &str =
        "usage: wsp worklist group <slug> N [--parallel N|none] [--agent \"kind [model] [effort]\"|manual] [--stop \"…\"|-|--stop --from FILE] [--why \"…\"|-]";
    let (Some(needle), Some(n)) = (args.rest.get(1).cloned(), args.rest.get(2).cloned()) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let mut w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let mut groups = w.groups();
    if groups.is_empty() {
        eprintln!("wsp: `{}` has no groups yet — wsp worklist add {} <task>…", w.id, w.id);
        return 1;
    }
    let ordinal = match ordinal_of(&n, groups.len()) {
        Ok(n) => n,
        Err(code) => return code,
    };
    // `--from` is where a `--stop` is read from and it is nothing else here.
    // The verb sets two fields and the check below already refuses to run
    // without being told which, so a `--from` naming no field is answered with
    // the shape rather than guessed at. The guess is cheap to make today, while
    // `--stop` is the only prose on this verb; it is a reading that would have
    // to be taken back the first time there is a second sentence to give.
    if args.has("from") && !args.has("stop") {
        eprintln!("wsp: --from is where a stop condition is read from — `wsp worklist group <slug> N --stop --from FILE`");
        return 2;
    }
    if !args.has("parallel") && !args.has("stop") && !args.has("agent") {
        eprintln!("{USAGE}");
        eprintln!("       --parallel caps the work; --agent says who runs it; --stop is the prose read at the barrier after it");
        return 2;
    }

    // The reason is settled before any prose is read, since both may want the
    // stream and only one of them can have it.
    let why = match reason(args) {
        Ok(why) => why,
        Err(code) => return code,
    };

    let win = window(store, &w);
    if !win.allows(ordinal) {
        // The one edit the group being run still takes: its stop, which
        // nothing has read as a barrier yet. See [`amend_running_stop`].
        let only_stop = args.has("stop") && !args.has("parallel") && !args.has("agent");
        let live = win.started && win.at == Some(ordinal);
        if live && only_stop {
            let Some(why) = why else {
                eprintln!("wsp: group {ordinal} of `{}` is being run, so its stop is corrected with a reason, which is logged", w.id);
                eprintln!("     `wsp worklist group {} {ordinal} --stop --from FILE --why -` — the reason on stdin, the stop from the file", w.id);
                return 2;
            };
            return match stop_prose(args) {
                Ok(text) => amend_running_stop(store, args, &w.id, ordinal, text, &why),
                Err(code) => code,
            };
        }
        let code = win.refuse(ordinal, "will not change");
        if live {
            eprintln!("     its stop alone may still be corrected, with a reason: `--stop --from FILE --why -`");
        }
        return code;
    }

    let mut said: Vec<String> = Vec::new();
    if let Some(v) = args.get("parallel") {
        match v.trim() {
            // `--parallel none` takes the cap off. Absent is a real state: as
            // many as the machine allows, which is what every group in all
            // three hand-run lists actually wanted.
            "" | "none" | "-" | "true" => {
                groups[ordinal - 1].cap = None;
                said.push("no cap".into());
            }
            // Refused rather than read as either of its meanings. "hold this
            // group" and "no cap" are both plausible readings of zero, and a
            // group silently taking one of them is how you find out at 3am
            // which one this build chose.
            "0" => {
                eprintln!("wsp: x0 would mean either `run nothing` or `no cap`, so it means neither — `--parallel none` takes the cap off");
                return 2;
            }
            other => match other.parse::<usize>() {
                Ok(n) => {
                    groups[ordinal - 1].cap = Some(n);
                    said.push(format!("x{n}"));
                }
                Err(_) => {
                    eprintln!("wsp: --parallel is a count, not `{other}`");
                    return 2;
                }
            },
        }
    }
    if let Some(v) = args.get("agent") {
        match policy_word(&v) {
            Ok(a) => {
                said.push(format!("agent: {a}"));
                groups[ordinal - 1].agent = a;
            }
            Err(code) => return code,
        }
    }
    if args.has("stop") {
        match stop_prose(args) {
            Ok(text) => {
                said.push(if text.is_empty() {
                    "no stop condition".into()
                } else {
                    format!("stop: {}", util::truncate(&text, 48))
                });
                groups[ordinal - 1].stop = text;
            }
            Err(code) => return code,
        }
    }

    let because = why.map(|y| format!(" — why: {y}")).unwrap_or_default();
    w.log(&format!("group {ordinal} — {} · {}{because}", groups[ordinal - 1].members.join(" "), said.join(", ")));
    save(store, &mut w, &groups, "group", &format!("group {ordinal} {}", said.join(", ")));

    if args.json() {
        println!("{}", worklist_json(store, &w));
    } else {
        println!("{} group {ordinal}: {}", w.id, said.join(" · "));
    }
    0
}

/// Correct the stop of the group being run, before its barrier is checked.
/// `wsp-206`.
///
/// **The stop is the one field of a live group nothing has read as a barrier
/// yet.** Membership, cap and line are what is in flight — members started on
/// them — so they stay frozen. The stop is read once, as a barrier, by the agent
/// [`crate::cycle`] starts when every member holds; until then it is a sentence
/// about the future. In `wsp-unattended` a verifier found that group 2's stop
/// asserted a price the vendor had cancelled. Refused here, the correction went
/// on the member as a decision. The barrier agent was then handed the false stop
/// whole and trusted to read past it, and the governor sent a hand `tell` to make
/// sure, which a run is not allowed.
///
/// So the window opens for the stop alone, on two terms:
///
/// - **A reason, logged.** The stop is still the plan, and an edit to it after
///   the run has started is a decision about the run. The list's log is where
///   such a decision is recorded, beside `go`'s and `hold`'s.
/// - **Only until the barrier row is open.** [`crate::cycle`] composes the
///   barrier agent's work order with the stop in it when it opens the row. After
///   that the stop has been read, and changing the record would make it disagree
///   with what the agent was given. A `hold` settles that row, and the recheck
///   is composed again from the list, so it reads the corrected stop.
///
/// The check and the write happen under the store lock, which is also where
/// `open_barrier` creates the row. The two are ordered, so an accepted amendment
/// is the stop the barrier is handed.
fn amend_running_stop(store: &Store, args: &Args, id: &str, ordinal: usize, text: String, why: &str) -> i32 {
    let outcome = store.locked(|| -> Result<(Worklist, bool), i32> {
        // Read again inside the lock: a `go` in another process may have
        // passed this barrier since the window was first read.
        let mut w = store.worklist(id).ok_or(1)?;
        let mut groups = w.groups();
        let win = window(store, &w);
        if win.at != Some(ordinal) {
            return Err(win.refuse(ordinal, "will not amend the stop of"));
        }
        let open = store.tasks().into_iter().find(|t| {
            crate::cycle::is_barrier(t, &w.id, ordinal)
                && !matches!(t.status(), crate::model::Status::Review | crate::model::Status::Done)
        });
        if let Some(b) = open {
            eprintln!(
                "wsp: will not amend group {ordinal}'s stop — its barrier is open on {} ({}), which was handed the stop as it stands",
                b.id,
                b.status().as_str()
            );
            eprintln!("     if that stop is wrong, the barrier holds; its recheck is handed the stop as amended then");
            return Err(1);
        }
        let g = &mut groups[ordinal - 1];
        if g.stop == text {
            return Ok((w, false));
        }
        g.stop = text;
        w.log(&format!(
            "group {ordinal} stop amended while it runs — why: {why} · now: {}",
            if g.stop.is_empty() { "none" } else { &g.stop }
        ));
        w.set_groups(&groups);
        if let Err(e) = store.save_worklist(&w) {
            eprintln!("wsp: write failed: {e}");
            return Err(1);
        }
        Ok((w, true))
    });
    let (w, changed) = match outcome {
        Ok(o) => o,
        Err(code) => return code,
    };
    // Committed outside the lock, as `member` does: the lock orders the edit
    // against the barrier opening, and a commit is seconds nobody should wait
    // behind.
    if changed {
        store.log_event("worklist-edited", json!({ "id": w.id, "what": "stop", "why": why }));
        store.git_commit(&format!("wsp: worklist {} group {ordinal} stop amended while it runs", w.id));
    }

    if args.json() {
        println!("{}", worklist_json(store, &w));
    } else {
        let stop = &w.groups()[ordinal - 1].stop;
        let said = if stop.is_empty() { "no stop condition".to_string() } else { util::truncate(stop, 60) };
        if !changed {
            println!("{} group {ordinal}: the stop already reads that, so nothing was amended · {said}", w.id);
            return 0;
        }
        println!("{} group {ordinal}: stop amended while it runs · {said}", w.id);
        println!(
            "  {}",
            Paint::new().dim(&format!(
                "why: {} · the barrier is handed this one; an agent already started was handed the one before",
                util::truncate(why, 60)
            ))
        );
    }
    0
}

/// `--why`, the reason an edit is logged with. Optional ahead of the run, and
/// required by [`amend_running_stop`].
///
/// It takes a typed sentence or the stream. It does not take `--from`, which on
/// this verb is already the stop's source. So the stop comes from a file and
/// the reason from stdin, or one of them is typed, and both on stdin is refused
/// before either is read. The typed sentence gets the check [`typed_stop`] makes,
/// since a reason for a corrected stop is written in the same vocabulary.
fn reason(args: &Args) -> Result<Option<String>, i32> {
    let Some(raw) = args.get("why") else { return Ok(None) };
    if !matches!(raw.trim(), "-" | "true") {
        if let Some(bad) = util::terminal_output(&raw) {
            eprintln!("wsp: {bad}");
            eprintln!("     a backtick inside double quotes runs a command. `--why -` reads the reason from stdin, where a shell never sees it");
            return Err(2);
        }
        return Ok(Some(fold(&raw)).filter(|s| !s.is_empty()));
    }
    let stop_streams = match args.get("from") {
        Some(f) => matches!(f.trim(), "-" | "true"),
        None => matches!(args.get("stop").as_deref().map(str::trim), Some("-" | "true")),
    };
    if stop_streams {
        eprintln!("wsp: the stop and its reason cannot both come from stdin — `--stop --from FILE --why -`");
        return Err(2);
    }
    if util::stdin_is_tty() {
        eprintln!("wsp: nothing is piped in — `--why -` reads the reason from a stream");
        return Err(2);
    }
    match crate::cmd_task::read_source("-") {
        Ok(text) => match fold(&text) {
            t if t.is_empty() => {
                eprintln!("wsp: nothing on stdin — `--why -` wants the reason there");
                Err(2)
            }
            t => Ok(Some(t)),
        },
        Err(e) => {
            eprintln!("wsp: cannot read stdin: {e}");
            Err(1)
        }
    }
}

// ---- member -----------------------------------------------------------

/// `wsp worklist member <slug> <task> --agent "kind [model] [effort]"|none`:
/// who runs one member, where that is not the group's line. `wsp-150`.
///
/// **Its window is the member's, not the group's.** A group's line is frozen
/// with its membership once the run reaches it, because the barrier and every
/// member still to start run on it. A member's line is about one spawn and
/// its verifier, and nothing has read it until that spawn: so it may change
/// inside the group being run, for a member still at `todo` that nobody has
/// claimed. That is the edit `wsp-runs-itself` needed on 2026-10-04, and the
/// only way to it was to retire the list. A member that has started is
/// refused, because its agent is already running on the line it started with
/// and its verifier is chosen by that line — changing it would make the record
/// say one kind ran and start the verifier on another.
///
/// The check and the write are under the store lock, which is the lock
/// `cycle::take_member` reads the line under when it starts the member: the
/// two are ordered, so an edit that is accepted is the line that runs.
///
/// `none` takes the member's line away, and it runs on the group's again.
/// `manual` is refused: whether wsp runs a group is the group's line, and a
/// member wsp skipped would be a barrier waiting on something nobody starts.
pub fn member(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "usage: wsp worklist member <slug> <task> --agent \"kind [model] [effort]\"|none";
    let (Some(needle), Some(who), Some(raw)) = (args.rest.get(1).cloned(), args.rest.get(2).cloned(), args.get("agent"))
    else {
        eprintln!("{USAGE}");
        eprintln!("       a member's own line overrides its group's for its spawn and its verifier; none takes it away");
        return 2;
    };
    let line = match raw.trim() {
        "none" | "-" => None,
        MANUAL => {
            eprintln!("wsp: a member is not run by hand on its own — whether wsp runs a group is the group's line: `wsp worklist group <slug> N --agent {MANUAL}`");
            return 2;
        }
        _ => match policy_word(&raw) {
            Ok(a) => Some(a),
            Err(code) => return code,
        },
    };
    let w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let Some((_, id)) = member_named(&w.groups(), &who, store) else {
        eprintln!("wsp: `{who}` is not in `{}` — wsp worklist show {}", w.id, w.id);
        return 1;
    };

    let outcome = store.locked(|| -> Result<(Worklist, usize, Option<String>, bool), i32> {
        // Read again inside the lock: the list, the window and the member's
        // standing are all things a start in another process may have changed.
        let mut w = store.worklist(&w.id).ok_or(1)?;
        let mut groups = w.groups();
        let Some(gi) = groups.iter().position(|g| g.members.contains(&id)) else {
            eprintln!("wsp: `{id}` is no longer in `{}` — wsp worklist show {}", w.id, w.id);
            return Err(1);
        };
        let win = window(store, &w);
        if win.started {
            if win.at.map_or(true, |at| gi + 1 < at) {
                return Err(win.refuse(gi + 1, &format!("will not change {id}'s agent line in")));
            }
            if let Some(why) = member_started(store, &id) {
                eprintln!("wsp: will not change {id}'s agent line — it has started ({why}), and runs on the line it started with");
                eprintln!("     its verifier follows that line too; a member still at todo with no claim may change");
                return Err(1);
            }
        }
        let was = groups[gi].member_agents.get(&id).cloned();
        match &line {
            // The group's own line said again is no override, and storing it
            // as one would keep this member on it after the group's changes.
            Some(a) if *a != groups[gi].agent.trim() => {
                groups[gi].member_agents.insert(id.clone(), a.clone());
            }
            _ => {
                groups[gi].member_agents.remove(&id);
            }
        }
        if groups[gi].member_agents.get(&id) == was.as_ref() {
            return Ok((w, gi + 1, was, false));
        }
        let said = groups[gi].member_agents.get(&id).cloned().unwrap_or_else(|| "the group's".into());
        w.log(&format!("{id} runs on {said}"));
        // Written in the lock and committed outside it, as `take_member` does:
        // the lock orders the edit against a start, and a commit is seconds
        // nobody else should wait behind.
        w.set_groups(&groups);
        if let Err(e) = store.save_worklist(&w) {
            eprintln!("wsp: write failed: {e}");
            return Err(1);
        }
        Ok((w, gi + 1, groups[gi].member_agents.get(&id).cloned(), true))
    });
    let (w, ordinal, now, changed) = match outcome {
        Ok(o) => o,
        Err(code) => return code,
    };
    if changed {
        store.log_event("worklist-edited", json!({ "id": w.id, "what": "member" }));
        let said = now.as_deref().unwrap_or("the group's");
        store.git_commit(&format!("wsp: worklist {} member {id} agent {said}", w.id));
    }

    if args.json() {
        println!("{}", worklist_json(store, &w));
    } else {
        let g = &w.groups()[ordinal - 1];
        match now {
            Some(a) => println!("{id} in {} group {ordinal}: agent {a}", w.id),
            None => println!("{id} in {} group {ordinal}: the group's agent, {}", w.id, group_agent_word(g)),
        }
        if g.policy().is_none() {
            println!("  {}", Paint::new().dim("the group is run by hand, so nothing starts on it until the group has an agent: line"));
        }
    }
    0
}

/// Whether a member has started, and the words for how, or `None` when it is
/// still a `todo` nobody has claimed — the one standing in which its line has
/// not been read yet.
fn member_started(store: &Store, id: &str) -> Option<String> {
    let t = store.find_task(id)?;
    let claim = store.claims().get(id).map(crate::cmd_agent::claim_where);
    match (t.status(), claim) {
        (crate::model::Status::Todo, None) => None,
        (s, None) => Some(s.as_str().to_string()),
        (s, Some(c)) => Some(format!("{} · claimed in {c}", s.as_str())),
    }
}

/// A group's line as a word, `manual` when there is none.
fn group_agent_word(g: &Group) -> String {
    match g.agent.trim() {
        "" => MANUAL.to_string(),
        a => a.to_string(),
    }
}

/// `--agent` on `add` into a group that exists: the members' own lines.
/// Nothing when it names the group's line, which is what they run on anyway.
fn join_with_agent(g: &mut Group, members: &[String], line: &str) {
    if line == g.agent.trim() {
        return;
    }
    for m in members {
        g.member_agents.insert(m.clone(), line.to_string());
    }
}

/// The prose read at a barrier: the text as typed, the stream, or the file
/// `--stop --from` names.
///
/// `-` because this is the sentence a shell is worst at carrying. Stop prose is
/// the reason a night stopped — *"if any of the three goes badly, flag and stop
/// rather than push through"* — and it is written in the vocabulary of the
/// work, which means backticks and identifiers, and every backtick inside
/// double quotes runs a command. `-` is the path that never meets a shell.
///
/// **And a file, which is the spelling this field is actually reached for by.**
/// A stop condition is written *ahead* of the run, by whoever is composing the
/// list, and it is written in a file — so `-` alone means `cat`ing that file
/// into a pipe in order to name it, and that friction is what ends with
/// somebody typing the sentence between double quotes after all. Every other
/// prose intake in the CLI takes both spellings — [`crate::cmd_task`]'s
/// `payload_source` and `prose_source`, [`crate::cmd_agent::from_source`] — and
/// this one took one. A lesser gap than `install --why`'s, since the safe form
/// worked here and nobody was returned to the unsafe one, and still the half
/// that gets used. `worklist-047`.
///
/// `--from` is read only once `--stop` has asked for it. This verb sets two
/// fields and already refuses to run without being told which — `--parallel`
/// and `--stop` with neither given is a usage error — so a `--from` naming no
/// field would be the first thing here that guesses. See [`group`], which
/// answers a bare `--from` with the shape rather than obeying it.
///
/// Folded to one paragraph, and that is not tidying. `## Groups` is a structure
/// parsed line by line: a blank line inside a `stop:` block ends it, so prose
/// stored with one would come back next read as prose that stops at the gap.
fn stop_prose(args: &Args) -> Result<String, i32> {
    let raw = args.get("stop").unwrap_or_default();
    // `--stop` with nothing usable after it names the stream, on the same
    // reading `edit --from` gives it: a missing argument is a mistake worth
    // answering, not an editor session nobody asked for.
    let asked_for_prose = matches!(raw.trim(), "-" | "true");
    let (src, named) = match args.get("from") {
        // Two sources for one sentence, refused rather than resolved: a caller
        // who gave both does not know which is being recorded, and picking
        // either is the same silent loss in a smaller hat. The reading
        // `cmd_agent::from_source` gives the same collision.
        Some(_) if !asked_for_prose => {
            eprintln!("wsp: a sentence and --from are two stop conditions — give one");
            return Err(2);
        }
        // A lone `--from`, or `--from -`: a dash is not a path, it is the
        // conventional name for the stream.
        Some(path) if matches!(path.as_str(), "true" | "-") => ("-".into(), "stdin".to_string()),
        // Named the way the rest of the CLI names a path, so the receipt says
        // `~/lists/phase-five/g2.md` and not an absolute one nobody typed.
        Some(path) => {
            let named = util::contract(&util::expand(&path));
            (path, named)
        }
        None => match raw.trim() {
            "-" | "true" => ("-".to_string(), "stdin".to_string()),
            "" | "none" => return Ok(String::new()),
            // Not `raw.trim()`: see [`typed_stop`], which has to see the
            // whitespace before anything trims or folds it away.
            _ => return typed_stop(&raw),
        },
    };
    if src == "-" && util::stdin_is_tty() {
        eprintln!("wsp: nothing is piped in — `--stop -` reads the prose from a stream");
        return Err(2);
    }
    match crate::cmd_task::read_source(&src) {
        Ok(text) => {
            let text = fold(&text);
            if text.is_empty() {
                // An empty stream reads exactly like "take the stop condition
                // off", and the two want different things done about them.
                eprintln!("wsp: nothing on {named} — `--stop none` is how a stop condition is taken off");
                return Err(2);
            }
            Ok(text)
        }
        Err(e) => {
            eprintln!("wsp: cannot read {named}: {e}");
            Err(1)
        }
    }
}

/// A stop condition typed on the command line, checked for the one thing that
/// says it is not one.
///
/// Every other typed intake makes this check — `add`, `rename`, `say`, `flag`'s
/// row text, `project set`, `note` and its neighbours — and these two did not.
/// It is what catches the substitution the caller cannot see: the backticks a
/// stop condition is full of ran before wsp was handed anything, and what
/// arrives is fluent prose with the load-bearing nouns replaced by a command's
/// output. See [`crate::util::terminal_output`].
///
/// Before the fold, because folding collapses runs of whitespace and a carriage
/// return is whitespace: the evidence that this came off a terminal is the
/// first thing `fold` would destroy.
fn typed_stop(raw: &str) -> Result<String, i32> {
    if let Some(why) = util::terminal_output(raw) {
        eprintln!("wsp: {why}");
        // Names the cause, because the shape of the mistake is not visible in
        // what arrived.
        eprintln!("     a backtick inside double quotes runs a command. `--stop --from FILE` reads the prose out of a file, where a shell never sees it");
        return Err(2);
    }
    Ok(fold(raw))
}

/// Write the queue back and record it: the file, the event, the commit.
///
/// One place, because every editing verb owes the same four things and a verb
/// that forgets the commit leaves the store's history disagreeing with the
/// store — which is the failure this record exists to be immune to.
fn save(store: &Store, w: &mut Worklist, groups: &[Group], what: &str, msg: &str) -> i32 {
    w.set_groups(groups);
    if let Err(e) = store.save_worklist(w) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("worklist-edited", json!({ "id": w.id, "what": what }));
    store.git_commit(&format!("wsp: worklist {} {msg}", w.id));
    0
}

// ---- ls ---------------------------------------------------------------
//
// Three segments, newest first, and the closed ones behind a count. The
// predicate and the ordering are `worklist::listing` — the panel's
// `worklists` section draws the same answer at both sidebar widths, and a
// segment worked out twice is two segments by the end of the month.
//
// **This verb is on the expensive side of the context line.** A person opening
// a panel pays nothing; `wsp worklist ls` is run by every governor at every
// barrier and lands in a transcript that is re-read on every later request. So
// what is added here is one column and at most three heading lines, and what
// is taken away grows: the closed segment is one line however many worklists
// are in it, and at roughly one new worklist a night that is the half of this
// change that pays.

pub fn list(store: &Store, args: &Args) -> i32 {
    // Read once, whatever is drawn: `--all` reveals rows this already holds
    // rather than asking a second question of the store.
    let listed = worklist::listing(store, store.worklists());
    let all = args.has("all");

    if args.json() {
        // No collapse and no `--all` here: an abridgement is a thing done for
        // a reader, and a parser is not one.
        let out: Vec<_> = listed.iter().map(listed_json).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }
    if listed.is_empty() {
        println!("no worklists yet — wsp worklist new <slug> \"title\"");
        return 0;
    }
    for line in list_lines(&Paint::new(), &listed, all) {
        println!("{line}");
    }
    0
}

/// The table: a heading per segment that has anything in it, its rows newest
/// first, and the closed ones counted rather than drawn.
///
/// Built rather than printed so the segmenting can be asserted on. The width
/// of the id column follows the widest slug drawn, so hiding the closed
/// segment does not leave the table indented for a name that is not on it.
fn list_lines(p: &Paint, listed: &[worklist::Listed], all: bool) -> Vec<String> {
    let drawn: Vec<&worklist::Listed> =
        listed.iter().filter(|l| all || l.segment != Segment::Closed).collect();
    let hidden = listed.len() - drawn.len();

    let w_id = drawn.iter().map(|l| l.list.id.chars().count()).max().unwrap_or(8).max(8);
    let mut out = vec![format!(
        "{}  {}  {}  {}  {}  {}",
        p.dim(&util::pad("WORKLIST", w_id)),
        p.dim(&util::pad("STATUS", 7)),
        p.dim("GROUPS"),
        p.dim("AT"),
        p.dim("OPEN"),
        p.dim("TITLE")
    )];

    let mut segment = None;
    for l in &drawn {
        if segment != Some(l.segment) {
            segment = Some(l.segment);
            out.push(p.dim(heading(l.segment)));
        }
        out.push(format!(
            "{}  {}  {}  {}  {}  {}",
            util::pad(&l.list.id, w_id),
            util::pad(l.list.status().as_str(), 7),
            util::pad(&l.at.of.to_string(), 6),
            util::pad(&at_mark(l), 2),
            util::pad(&count(l.open), 4),
            p.dim(&util::truncate(&l.list.title, 40))
        ));
    }

    // Never hide quietly: a list with the closed segment taken out of it reads
    // exactly like a list that has none, and the difference is a worklist
    // somebody may still want to look at. `cmd_project.rs`'s abridged-decision
    // line is the shape.
    if hidden > 0 {
        out.push(p.dim(&format!("⋯ {hidden} closed · wsp worklist ls --all")));
    }
    out
}

/// The segment heading, and the one place the axis is named.
///
/// `unjudged` carries its gloss because the word is doing the arguing: the
/// rows under it are on worklists whose `status` says `done`, and a reader who
/// takes the heading for a status reads the list as broken. `running` and
/// `closed` say what they are.
fn heading(s: Segment) -> &'static str {
    match s {
        Segment::Unjudged => "unjudged — the run is over, the rows are not",
        Segment::Parked => "parked — paused by somebody, and only `resume` moves it",
        other => other.word(),
    }
}

/// Where the run is up to, in one column.
///
/// The four answers and their argument live on [`worklist::at_mark`], which is
/// where they moved when the panel's section became the second caller: two
/// surfaces drawing the same mark from two spellings of the question is how
/// `ls` and a panel come to disagree about one run.
fn at_mark(l: &worklist::Listed) -> String {
    worklist::at_mark(l).text()
}

/// A count, or the mark for none.
///
/// Zero is drawn as `·` rather than `0` for the reason the `AT` column takes
/// one: a column of numbers with a `0` in it is read as a small number, and
/// what is meant is that there is nothing there.
fn count(n: usize) -> String {
    match n {
        0 => "·".to_string(),
        n => n.to_string(),
    }
}

/// One row for a parser: the worklist object every other verb emits, and the
/// five facts this reading adds to it.
///
/// `gone` and the position's `unwritten` have no column in the table — the
/// `AT` mark says only that one of them is not empty — so this is where a
/// caller that wants to know *which* group, or *which* member, reads it
/// without running `show`.
fn listed_json(l: &worklist::Listed) -> serde_json::Value {
    let mut out = worklist_json_at(&l.list, &l.at);
    if let Some(o) = out.as_object_mut() {
        o.insert("segment".into(), json!(l.segment.word()));
        o.insert("open".into(), json!(l.open));
        o.insert("gone".into(), json!(l.gone));
        o.insert("unwritten".into(), json!(l.at.unwritten));
        o.insert("activity".into(), json!(util::iso_at(l.activity)));
    }
    out
}

// ---- what a passed group left behind ----------------------------------
//
// `Position::slipped` is the verdict floor's receipt: a member of a group the
// run has already passed that does not read as finished. Its own doc comment
// ends *"a caller that prints nothing about it is the floor quietly covering
// the thing it was put in to survive"* — and for a while `report` was the only
// caller that printed anything, so `show`, `next` on a finished list, `done`
// and every `--json` object between them covered it. That is why the words and
// the mark live here instead of at one call site: a block one caller in four
// prints is how this happened, and a block nothing can assert on is why it went
// unnoticed.
//
// [`Position::unwritten`] is the same receipt one level up, and it gets the
// same treatment for the same reason: it was born because `ls` drew `!` on
// `phase-two` while `show` drew the group as an ordinary passed tick.

/// What being behind means, in the words a reader can act on.
const BEHIND: &str = "in a group already passed, and not finished — the run does not go back for them";

/// What an unwritten barrier means, likewise. There is no verb that fills one
/// after the fact — `go` passes only the barrier the position stands at, and
/// the floor has walked past this one — so the sentence stops at what is true:
/// nothing was asked, and nothing records what was decided.
const UNWRITTEN: &str =
    "a barrier somebody crossed without writing at it — no stop condition was read, and nothing records what was decided";

/// The `behind` block, in the caller's own label column.
///
/// `head` is the label already painted and `indent` the column the sentence
/// under it hangs from — the running verbs put a bold word at the margin and
/// `show` has an eight-wide dim column — and both of them put two spaces after
/// the head, which is the only thing this needs to know about either.
///
/// Empty in every ordinary run, which is what makes it affordable on a verb a
/// governor types at every barrier.
fn behind_lines(p: &Paint, slipped: &[Standing], head: &str, indent: usize) -> Vec<String> {
    if slipped.is_empty() {
        return Vec::new();
    }
    let ids: Vec<&str> = slipped.iter().map(|s| s.id.as_str()).collect();
    let mut out = vec![format!("{head}  {}", ids.join("  "))];
    out.extend(
        util::wrap(BEHIND, 78usize.saturating_sub(indent))
            .iter()
            .map(|line| format!("{}{}", " ".repeat(indent), p.dim(line))),
    );
    out
}

/// The same shape for the barriers crossed without a verdict, off
/// [`Position::unwritten`].
fn unwritten_lines(p: &Paint, ordinals: &[usize], head: &str, indent: usize) -> Vec<String> {
    if ordinals.is_empty() {
        return Vec::new();
    }
    let named = match ordinals {
        [one] => format!("group {one}"),
        many => format!("groups {}", many.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")),
    };
    let mut out = vec![format!("{head}  {named}")];
    out.extend(
        util::wrap(UNWRITTEN, 78usize.saturating_sub(indent))
            .iter()
            .map(|line| format!("{}{}", " ".repeat(indent), p.dim(line))),
    );
    out
}

/// The same members, for a `--json` object: the id and the word the store holds
/// for it, which together are the disagreement.
fn behind_json(slipped: &[Standing]) -> serde_json::Value {
    json!(slipped
        .iter()
        .map(|s| json!({ "id": s.id, "status": s.settlement.word() }))
        .collect::<Vec<_>>())
}

/// The mark in front of a group in the plan: where the run is, and the one
/// case where "passed" is not the whole truth.
///
/// A group behind the position drew `✓` whatever its members now say, which is
/// exactly the same glyph as one that genuinely landed — so the plan, which is
/// where somebody is looking, was the surface that said least about it. `!` is
/// the mark a panel row already uses for something that wants a person.
fn group_mark(p: &Paint, at: Option<usize>, ordinal: usize, slipped_in: &BTreeSet<usize>) -> String {
    match at {
        Some(a) if a == ordinal => p.cyan("→"),
        _ if slipped_in.contains(&ordinal) => p.yellow("!"),
        Some(a) if a > ordinal => p.dim("✓"),
        None => p.dim("✓"),
        _ => " ".to_string(),
    }
}

/// Which groups the plan marks `!`, off the shared reading — see
/// [`crate::worklist::flagged_groups`].
fn flagged_in(pos: &Position) -> BTreeSet<usize> {
    worklist::flagged_groups(pos)
}

// ---- show -------------------------------------------------------------

pub fn show(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp worklist show <slug> [--log]");
        return 2;
    };
    let w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };

    // Settled, and only settled. This is a reading verb somebody runs while
    // composing, and the landed reading is a git process per member; the two
    // disagreeing is itself worth seeing, and the barrier is where that is
    // paid for.
    let pos = worklist::position(store, &w, Reading::Settled);
    let groups = w.groups();
    let dangling = worklist::dangling(store, &w);

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "worklist": worklist_json(store, &w),
                "groups": groups.iter().enumerate().map(|(i, g)| json!({
                    "ordinal": i + 1,
                    "parallel": g.cap,
                    "agent": g.agent,
                    "member_agents": g.member_agents,
                    "stop": g.stop,
                    "verdict": g.verdict,
                    "members": g.members,
                    // Whole, where the plan drawing draws a count: an
                    // abridgement is a thing done for a reader, and a parser
                    // is not one.
                    "landed": landed_json(&g.landed),
                })).collect::<Vec<_>>(),
                "at": pos.at,
                // The third state, for the reader that cannot see the `at`
                // line. `waiting_on` being empty is the same fact only if you
                // already know `at` stops at an unpassed barrier, which is the
                // knowledge every reader of this object was missing.
                "barrier": pos.at_barrier(),
                "waiting_on": pos.holding().iter().map(|s| json!({
                    "id": s.id,
                    "status": s.settlement.word(),
                })).collect::<Vec<_>>(),
                // Carried in the settled reading's object because the settled
                // reading is where it was invisible: this object said `at`,
                // `dangling`, `groups`, `waiting_on` and `worklist`, and a
                // member the floor stepped over is in none of those.
                "behind": behind_json(&pos.slipped),
                "unwritten": pos.unwritten,
                "dangling": dangling,
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    println!("{}  {}", p.bold(&w.id), p.dim(&w.title));
    println!();
    println!("{}  {}", p.dim(&util::pad("status", 8)), w.status().as_str());
    // Where it is up to, and — on a list nobody has started — the fact that
    // this is where it *would* start. A plan has no barriers behind its groups
    // and its position says where the run would begin, which is a different
    // sentence from where a run has got to; the suffix is what keeps the two
    // apart on one line.
    //
    // The middle arm is the third state, and it is why this row used to lie.
    // A group with nothing holding it and no verdict was drawn as a group the
    // run had gone past, so `show` said `at group 3 of 3` while `next` was
    // asking for a sentence about group 2. It says **settled** and not
    // *finished* on purpose: this is the free reading, which is a task's
    // status and arrives before the commit is on the trunk, and only the
    // barrier's own reading may promise that it will open.
    let draft = w.status() == WorklistStatus::Draft;
    println!(
        "{}  {}",
        p.dim(&util::pad("at", 8)),
        match pos.at {
            Some(n) if draft => format!("group {n} of {} · nothing started", pos.of),
            Some(n) if pos.at_barrier() => format!(
                "group {n} of {} · every member settled — its barrier has not been passed",
                pos.of
            ),
            Some(n) => format!("group {n} of {}", pos.of),
            None if pos.of == 0 => "no groups yet".to_string(),
            None => format!("every one of {} finished", pos.of),
        }
    );
    // The window, printed unasked. It is the answer to the question the next
    // command is going to ask, and one line here is cheaper than a refusal
    // after somebody has typed the edit.
    let win = window(store, &w);
    let first = win.first_open();
    println!(
        "{}  {}",
        p.dim(&util::pad("edit", 8)),
        match () {
            _ if pos.of == 0 => "anything — there is nothing in it yet".to_string(),
            _ if first == 1 => "every group — it has not been started".to_string(),
            _ if first > pos.of => "nothing but a new group at the end".to_string(),
            _ if first == pos.of => format!("group {}", pos.of),
            _ => format!("groups {first} to {}", pos.of),
        }
    );

    if !groups.is_empty() {
        println!();
        let tasks_now = if args.has("verdicts") { store.tasks() } else { Vec::new() };
        let behind_at = flagged_in(&pos);
        let w_ord = groups.len().to_string().chars().count();
        // Where the ids begin: the mark, the ordinal and the cap column, each
        // with its two spaces. Everything written under a group hangs off this,
        // so a stop condition and a member's status word both read as belonging
        // to the ids above them rather than to the margin.
        let members_at = w_ord + 9;
        for (i, g) in groups.iter().enumerate() {
            let ordinal = i + 1;
            let mark = group_mark(&p, pos.at, ordinal, &behind_at);
            let cap = match g.cap {
                Some(n) => format!("x{n}"),
                None => String::new(),
            };
            println!(
                "{mark}  {}  {}  {}",
                util::pad(&ordinal.to_string(), w_ord),
                util::pad(&cap, 2),
                member_ids(&p, g)
            );
            // Only the group being waited on gets its members named again with
            // a word each, because that is the only group anybody is standing
            // in front of. Doing it for all of them is 26 lines of a list read
            // several times a session.
            if pos.at == Some(ordinal) {
                for s in pos.holding() {
                    println!("{}{}  {}", " ".repeat(members_at), s.id, p.dim(s.settlement.word()));
                }
            }
            let indent = " ".repeat(members_at);
            // Only where it was written: a group from before `wsp-134` has no
            // line, and saying `manual` under each of twenty of them is noise.
            if !g.agent.trim().is_empty() {
                println!("{indent}{}", p.dim(&agent_line(g)));
            }
            // Wrapped against the column it starts in, so the whole block
            // sits inside 80 however deep the ordinals go.
            let width = prose_width(groups.len());
            for line in stop_lines(&p, g, &w.id, pos.at, ordinal, width, args.has("stops")) {
                println!("{indent}{line}");
            }
            // The verdict under the stop condition it answers. A group with a
            // stop condition and no verdict under it is a barrier that has not
            // been passed, which is a thing worth being able to see in the plan
            // rather than only at `next`.
            for line in verdict_lines(&p, g, &w.id, width, args.has("verdicts")) {
                println!("{indent}{line}");
            }
            // And each member's own, under `--verdicts`: the newest pass on the
            // member's row, which is what the barrier is gated on (`wsp-188`).
            if args.has("verdicts") {
                for line in pass_lines(&p, &tasks_now, g) {
                    println!("{indent}{line}");
                }
            }
            for line in landed_lines(&p, g) {
                println!("{indent}{line}");
            }
        }
    }

    if !dangling.is_empty() {
        // Named, never removed. A machine quietly editing the membership is the
        // stale-plan failure with nobody left to notice it, so the record keeps
        // saying the id it was given and this line says nothing answers to it.
        println!();
        println!("{}  {}", p.dim(&util::pad("gone", 8)), dangling.join("  "));
        println!("{}  {}", util::pad("", 8), p.dim("no task answers to these — nothing here removes them"));
    }

    // Beside `gone`, in the same column and for the same reason: both are about
    // a member the run will never mention again, and neither is anything the
    // position line above says. The `!` marks in the plan say which groups;
    // this says which members, because a mark with no legend is not evidence.
    if !pos.slipped.is_empty() {
        println!();
        for line in behind_lines(&p, &pos.slipped, &p.dim(&util::pad("behind", 8)), 10) {
            println!("{line}");
        }
    }

    // And the barrier half of the same receipt, off `Position::unwritten`: a
    // group below the floor carrying no verdict drew an ordinary passed tick
    // here while `ls` drew `!` on the row — the two surfaces disagreeing about
    // one group. The mark on it now has its legend.
    if !pos.unwritten.is_empty() {
        println!();
        for line in unwritten_lines(&p, &pos.unwritten, &p.dim(&util::pad("unwritten", 8)), 10) {
            println!("{line}");
        }
    }

    // The prose, minus the two sections that are not prose: `Groups` is drawn
    // above and `Log` grows for ever, so it is named and counted rather than
    // printed into every reading of the plan.
    let mut rest = crate::model::localise_dates(&w.body);
    crate::model::set_section_in(&mut rest, "Groups", "");
    let log = crate::model::section_of(&rest, "Log").unwrap_or_default();
    if !args.has("log") {
        crate::model::set_section_in(&mut rest, "Log", "");
    }
    if !rest.trim().is_empty() {
        println!("\n{}", rest.trim());
    }
    if !args.has("log") && !log.trim().is_empty() {
        println!(
            "\n{}",
            p.dim(&format!("log  {} entries · wsp worklist show {} --log", log.trim().lines().count(), w.id))
        );
    }
    0
}

// ---- running it: the barrier ------------------------------------------
//
// `next`, `go`, `hold`, `done`. Everything above this line composes a plan;
// everything below it runs one, and the line between the two is the barrier.

/// Which barrier is shut, when one is.
///
/// There is one in front of every group, including the first, and a run is the
/// sequence of them being passed. What differs is where the prose to read at it
/// lives and what passing it means.
#[derive(Debug)]
enum Gate {
    /// **Barrier zero.** The list has not been started, and the prose is its own
    /// `## Overview` — per `wsp-092`, a start condition on group N is stop
    /// prose on group N−1, and group 1 has no group before it, so the list
    /// carries it.
    ///
    /// This is the start condition nobody is made to read, because there is no
    /// barrier in front of it — `worklist-004` found that and named it rather
    /// than inventing a verb outside its brief. It is a barrier here, which is
    /// what makes the overview load-bearing rather than decorative.
    ///
    /// It needs no verdict field: the decision to pass it is the status moving
    /// off `draft`, which is already written. One written fact per barrier and
    /// no barrier with two.
    Start,
    /// The barrier behind group `n`, which has finished and has not been
    /// passed. [`Group::verdict`] is what says whether it has.
    After(usize),
    /// Somebody said stop. Nothing more starts until a `go` reopens it, and the
    /// prose is the sentence they wrote.
    Held,
    /// A person paused the list. Nothing starts, no barrier is checked, and
    /// the only way past it is `resume`, which lands on whatever gate the
    /// position stands at — never through it. The prose is their reason.
    Parked,
}

/// The front of the run: what may be started now, and what is already going.
#[derive(Debug)]
struct Front {
    /// The members `wsp spawn` may be run on, in the order the group names
    /// them, with the cap already applied.
    ready: Vec<String>,
    /// The members something is already doing, and the one thing worth saying
    /// about each — who is holding it, or what git says is outstanding.
    waiting: Vec<(Standing, String)>,
    /// How many more would be ready but for the cap. Reported because a
    /// governor told "2 may start now" about a group of five otherwise goes
    /// looking for the three that are missing.
    capped: usize,
    cap: Option<usize>,
}

/// What `next` has to say, which is one of four things and never a fifth.
///
/// The whole design of this verb is that a governor runs it on repeat and
/// each answer names the one command to run next: spawn, wait, `go`/`hold`,
/// `done`. Everything an agent runs repeatedly is paid for in context on every
/// request of every session, so a state that does not fit on two lines is a
/// state that has to earn it.
#[derive(Debug)]
enum State {
    /// Members may start. `wsp spawn <id>` each.
    Ready(Front),
    /// Nothing may start and the group is not finished. Wait.
    Waiting(Front),
    /// A barrier that has not been passed. `wsp worklist go` or `hold`.
    ///
    /// `flight` is how many members are still going despite it, which is only
    /// ever non-zero for [`Gate::Held`]: holding stops the queue handing out
    /// work and leaves what is already in a tree to land, and a reader of a
    /// held list has to be told that rather than left to assume it stopped.
    Shut { gate: Gate, prose: String, flight: usize },
    /// Every group finished. `wsp worklist done`.
    Nothing,
}

/// The mark a held list's reason is logged under, and the thing that reads it
/// back.
///
/// Archaeology over the log, and it is worth saying why rather than hiding it.
/// `hold` writes a sentence and `next` has to be able to show it — somebody
/// walking up to a stopped list wants to know why it stopped, and the
/// alternative is a second place for the same sentence to live on a record
/// whose entire design is that nothing is written twice. The log is where the
/// sentence already goes, one function writes the mark, and a line nobody can
/// parse costs the reason and nothing else.
const HELD: &str = "held —";

/// The same, for a pause. Its own mark, so a hold written before a park reads
/// back as the hold and not as the reason somebody paused.
const PARKED: &str = "parked —";

fn last_logged(w: &Worklist, mark: &str) -> Option<String> {
    let log = w.section("Log").unwrap_or_default();
    log.lines()
        .rev()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest.trim()))
        .find_map(|rest| rest.strip_prefix(mark).map(|s| s.trim().to_string()))
}

/// The column a group's prose is wrapped to in [`show`], off the widest
/// ordinal there is.
///
/// A function rather than a local because `go` needs the same answer: what it
/// tells the writer about their verdict is a count of the lines `show` will
/// draw, and a count taken at a different width is a number about nothing.
fn prose_width(groups: usize) -> usize {
    72usize.saturating_sub(groups.to_string().chars().count() + 9)
}

/// How much of a group's prose [`show`] draws before it counts it instead — a
/// verdict's lines, and a stop condition's once it is behind the position.
///
/// Six, and the corpus chose it rather than taste: of the nineteen verdicts
/// written in this store, six run to 1–4 lines and thirteen to 13–54, with
/// nothing at all in between. So the cap sits in an empty band — every
/// verdict is comfortably one side of it or the other, and no barrier's prose
/// is near enough the line for the number to be arguable.
///
/// One number for both blocks because both are drawn on one page: two caps is
/// two numbers a reader must learn, for no block either of them fits.
const PROSE_LINES: usize = 6;

/// The lines a group's verdict draws under it, most of them usually not drawn.
///
/// Split out to be asserted on, for [`crate::cmd_project::decision_lines`]'s
/// reason: what this block does with a long verdict is the whole point of it,
/// and printing straight to stdout would have left that untestable.
///
/// **Counted, never cut**, and the difference from the decisions block two
/// files over is the argument. A decision is written the way a commit message
/// is — the rule first, the argument after — so an index of first sentences is
/// a true index of them. **A verdict is written to no such convention**, and
/// `worklist-046` is what assuming one costs: phase four's group-2 verdict
/// opens *"THIS IS THE G2 VERDICT. The barrier passed without one"* — a
/// sentence about where the record had to be filed — and any reading that
/// stops there has the group's own record saying the group has none. There is
/// no length of lead that is right when the writer was never told the lead was
/// load-bearing, so what is drawn is the two facts the plan reading wants and
/// can stand behind: that the barrier was passed, and when.
///
/// **The cap is the same idea `## Log` is held to one screen above** — that
/// section is named and counted rather than printed into every reading,
/// because it grows for ever. Verdicts grow for ever in exactly the same way,
/// one per barrier and unbounded prose each, and the rule had not reached
/// them: `wsp worklist show phase-four` was 278 lines, 157 of them last
/// night's four verdicts, and every later barrier paid for all of it. Across
/// the five lists here it is 380 lines of history drawn as 22.
///
/// Under the cap the block is what it always was. A `went: <date>  passed`
/// announcing itself as one line held somewhere else would be a round trip
/// bought for nothing, which is how a reader learns to type the escape flag
/// past every cap it meets — [`crate::cmd_task::log_lines`] declines the same
/// trade in the same words.
fn verdict_lines(p: &Paint, g: &Group, id: &str, width: usize, full: bool) -> Vec<String> {
    if g.verdict.trim().is_empty() {
        return Vec::new();
    }
    let (when, said) = verdict_parts(&g.verdict);
    let lead = match when.is_empty() {
        true => "went: ".to_string(),
        false => format!("went: {when}  "),
    };
    let body = util::wrap(said, width);
    // The count is of lines and the escape is named on the same line, so a
    // reader who wants the prose never has to work out where it went. `--log`
    // holds it too, in the entry `go` wrote; `--verdicts` is the one that puts
    // it back where it is being read from.
    if !full && body.len() > PROSE_LINES {
        return vec![p.dim(&format!(
            "{lead}{} lines · wsp worklist show {id} --verdicts",
            body.len()
        ))];
    }
    body.iter()
        .enumerate()
        .map(|(n, line)| {
            let margin = " ".repeat(lead.chars().count());
            p.dim(&format!("{}{line}", if n == 0 { lead.as_str() } else { margin.as_str() }))
        })
        .collect()
}

/// Each member's newest verification, one line apiece: the state, the commit
/// it read, and the first line of what it said. The whole of it is on the
/// member under `## Verification`, which the line points at.
fn pass_lines(p: &Paint, tasks: &[crate::model::Task], g: &Group) -> Vec<String> {
    g.members
        .iter()
        .filter_map(|m| crate::verification::latest(tasks, m).map(|v| (m, v)))
        .map(|(m, v)| {
            let word = match v.state {
                crate::verification::State::Holds => p.green(v.state.as_str()),
                crate::verification::State::Blocks => p.red(v.state.as_str()),
                _ => p.dim(v.state.as_str()),
            };
            let read = v.read.as_deref().map(|r| format!(" at {r}")).unwrap_or_default();
            let said = v.text.lines().next().map(|l| format!(" · {}", util::truncate(l, 60))).unwrap_or_default();
            format!("{m}  {word}{}", p.dim(&format!("{read}{said}")))
        })
        .collect()
}

/// A verdict as stored — `<instant> <sentence>` — taken apart for printing.
fn verdict_parts(v: &str) -> (String, &str) {
    match v.trim().split_once(' ') {
        Some((stamp, rest)) if stamp.len() == 20 && util::is_stamp(stamp) => {
            (util::local_ymd(stamp), rest.trim())
        }
        _ => (String::new(), v.trim()),
    }
}

/// What [`show`] draws of what a passed group's members landed: **a count,
/// never the mapping.** The rule is `worklist-046`'s taken one step further —
/// no lead of this record can stand in for it, so the only things drawn are
/// the two numbers that cannot mislead: how many members were placed, of how
/// many, and how many files between them. A member recorded as unplaced keeps
/// the placed count below the membership, which is the honest shape of "we
/// could not look". The mapping itself is on the group in the file and in
/// `--json` — not an abridgement of prose drawn elsewhere, but a record that
/// is drawn only as its size.
fn landed_lines(p: &Paint, g: &Group) -> Vec<String> {
    if g.landed.is_empty() {
        return Vec::new();
    }
    let placed = g.landed.iter().filter(|e| e.commit.is_some()).count();
    let files: usize = g.landed.iter().map(|e| e.files.len()).sum();
    vec![p.dim(&format!(
        "landed: {} of {} member{} · {} file{}",
        placed,
        g.landed.len(),
        if g.landed.len() == 1 { "" } else { "s" },
        files,
        if files == 1 { "" } else { "s" },
    ))]
}

/// What [`show`] draws of a group's stop condition: whole in front of the
/// position, counted behind it.
///
/// **The split is by position, never by length.** The condition on the group
/// at the position is the text a governor weighs before `go` passes the
/// barrier, and an abridgement there has them pass on a summary of the thing
/// they were supposed to read — so it draws whole however long it runs, the
/// length being part of what was put at the barrier. Ahead of the position it
/// is the only prose on the page nobody has read yet, which makes it the most
/// useful text there rather than the least. Only behind the position is a
/// stop condition history — its barrier was passed, and `at` moves forward
/// only, so no reading will stand in front of it again — and there the rule
/// is [`verdict_lines`]': over the cap, one line naming the weight and the
/// command that prints it whole.
///
/// Under the cap the block is what it always was, behind the position or not:
/// the count line costs as much as a short condition and carries none of it,
/// so announcing a two-line stop would be a round trip bought for nothing —
/// [`verdict_lines`] declines the same trade in the same words.
///
/// No writer-side notice, unlike a verdict's [`verdict_notice`]: a condition
/// is composed ahead of the run, where this draws it whole, so the cut only
/// ever reaches prose after the reader it was written for has had all of it.
/// `--stops` puts every block back, in the place each was counted.
fn stop_lines(
    p: &Paint,
    g: &Group,
    id: &str,
    at: Option<usize>,
    ordinal: usize,
    width: usize,
    full: bool,
) -> Vec<String> {
    if g.stop.trim().is_empty() {
        return Vec::new();
    }
    // A run with nowhere to stand is past every barrier, so every block here
    // is history; a draft stands in front of group 1, so none of them is.
    let behind = match at {
        Some(a) => ordinal < a,
        None => true,
    };
    let body = util::wrap(g.stop.trim(), width);
    if behind && !full && body.len() > PROSE_LINES {
        return vec![p.dim(&format!(
            "stop: {} lines · wsp worklist show {id} --stops",
            body.len()
        ))];
    }
    body.iter()
        .enumerate()
        .map(|(n, line)| p.dim(&format!("{}{line}", if n == 0 { "stop: " } else { "      " })))
        .collect()
}

/// What `go` tells the writer about the verdict it has just recorded, when
/// there is anything to tell.
///
/// **The writer is the one reader who never finds out that a verdict is
/// abridged.** They read it back whole — out of `--log`, out of the file, out
/// of the shell they composed it in — and go on writing twenty lines into a
/// surface that draws a count of them. That is `worklist-046`'s second half:
/// the cut is invisible from the writing end, so nothing ever tells the person
/// choosing the words how many of them will be read.
///
/// It reports [`verdict_lines`]'s rule rather than restating it — same cap,
/// same width, same count — because two sentences about the same abridgement
/// that can disagree is worse than neither. Once per barrier, and nothing at
/// all under the cap, where what was written is what is drawn.
fn verdict_notice(id: &str, said: &str, groups: usize) -> Option<String> {
    let n = util::wrap(said.trim(), prose_width(groups)).len();
    (n > PROSE_LINES).then(|| {
        format!("went  {n} lines · the plan reading shows this line instead · wsp worklist show {id} --verdicts")
    })
}

/// A verdict as it is written: the instant, then the sentence.
///
/// Dated because a plan's history is worth having and this is the one entry in
/// it that is a judgement — "the barrier after group 2 was passed on the 19th,
/// and here is what was said" is the sentence somebody reconstructs a night
/// from. `passed` where no sentence was asked for, because the field being
/// non-empty is what says the barrier is behind us, and an empty string would
/// make an unanswered barrier and an answered one look the same.
fn verdict_of(said: &str) -> String {
    let text = said.trim();
    format!("{} {}", util::now_iso(), if text.is_empty() { "passed" } else { text })
}

/// The list a running verb is about, and the words after it.
///
/// **The first positional is a slug when it names a list and the start of the
/// sentence when it does not.** `wsp worklist go batch` and `wsp worklist go
/// "the three landed clean"` are both what somebody types, and a list is named
/// by one word while a verdict is a sentence, so the ambiguity is real and
/// narrow: a one-word verdict that happens to be a worklist slug. That case
/// answers "there is no such list" in the only direction that loses nothing —
/// the sentence is refused, not silently filed against the wrong list.
///
/// `-` as the whole of the words reads the sentence from a stream and
/// `--from FILE` reads it out of a file, for the reason `--stop` takes both: a
/// verdict is written in the vocabulary of the work, which means backticks and
/// identifiers, and every backtick inside the double quotes a shell needs for a
/// paragraph runs a command. See [`words`].
fn list_and_words(store: &Store, args: &Args) -> Result<(Worklist, String, bool), i32> {
    let first = args.rest.get(1).cloned().unwrap_or_default();
    if !first.is_empty() {
        if let Some(w) = find(store, &first) {
            return Ok((w, words(args, 2)?, false));
        }
    }
    match seated(store, args) {
        Some(w) => Ok((w, words(args, 1)?, true)),
        None => {
            if first.is_empty() {
                eprintln!("wsp: no worklist named, and this workspace is the seat for none");
                eprintln!("     wsp worklist ls names them · wsp govern <slug> --take takes the seat");
            } else {
                eprintln!("{}", worklist_or_why(store, &first).err().unwrap_or_default());
            }
            Err(1)
        }
    }
}

/// The sentence: the words typed, the stream a lone `-` names, or the file
/// `--from` names.
///
/// The file form is `worklist-047`, and it is here for the reason the stream
/// form is — a verdict is written in the vocabulary of the work, so it is full
/// of backticks and identifiers, and every backtick inside the double quotes a
/// shell needs for a paragraph runs a command — plus one the stream does not
/// cover. A verdict at a barrier is composed before it is given, often while
/// the last member is still landing, and where it is composed is a file. `-`
/// alone makes naming that file a `cat` into a pipe, and that friction is what
/// ends with the sentence going between double quotes after all. `note`,
/// `flag`, `tell`, `ask` and `answer` all take both spellings; these two verbs
/// took one.
///
/// A sentence and `--from` together is refused rather than resolved. Two
/// verdicts arrived for one barrier and only one of them is going on the
/// record; choosing is the same silent loss `cmd_agent::from_source` refuses.
fn words(args: &Args, from: usize) -> Result<String, i32> {
    let typed = args.text(from);
    let (src, named) = match args.get("from") {
        Some(path) => {
            if !typed.trim().is_empty() && typed.trim() != "-" {
                eprintln!("wsp: a sentence and --from are two verdicts — give one");
                return Err(2);
            }
            match path.as_str() {
                // A lone `--from`, or `--from -`: a dash is not a path, it is
                // the conventional name for the stream.
                "true" | "-" => ("-".to_string(), "stdin".to_string()),
                _ => {
                    let named = util::contract(&util::expand(&path));
                    (path, named)
                }
            }
        }
        None if typed.trim() == "-" => ("-".to_string(), "stdin".to_string()),
        // The typed form, and the check every other typed intake makes. See
        // [`typed_stop`], whose argument is this one: what a shell substituted
        // arrives fluent, so the caller cannot see it. Before the fold, because
        // a carriage return is whitespace and folding is what destroys the
        // evidence.
        None => {
            if let Some(why) = util::terminal_output(&typed) {
                eprintln!("wsp: {why}");
                eprintln!("     a backtick inside double quotes runs a command. `--from FILE` reads the sentence out of a file, where a shell never sees it");
                return Err(2);
            }
            return Ok(fold(&typed));
        }
    };
    if src == "-" && util::stdin_is_tty() {
        eprintln!("wsp: nothing is piped in — `-` reads the sentence from a stream");
        return Err(2);
    }
    match crate::cmd_task::read_source(&src) {
        Ok(text) => {
            let text = fold(&text);
            if text.is_empty() {
                // No words at all is a legal thing to say to `go` — a barrier
                // with no prose at it asks for no judgement — but a *named*
                // source that turned out to hold nothing is a caller whose
                // verdict went nowhere, which is the whole of `worklist-036`.
                // The two read the same and want different answers.
                eprintln!("wsp: nothing on {named} — nothing recorded");
                return Err(2);
            }
            Ok(text)
        }
        Err(e) => {
            eprintln!("wsp: cannot read {named}: {e}");
            Err(1)
        }
    }
}

/// The worklist this workspace is the seat for, if it is the seat for one.
///
/// This is what makes the governor's loop three words. `wsp govern <slug> --take` is
/// how a workspace comes to hold a worklist seat, and one key space means the
/// scope it holds is either a project or a list with no ambiguity to settle.
fn seated(store: &Store, args: &Args) -> Option<Worklist> {
    // `-w` names a room this process is not standing in, so its own pane says
    // nothing about who is sitting there — the same rule `wsp govern` applies
    // to the pane it stamps on the record. With no `-w`, off `my_pane()` and
    // not `HERDR_WORKSPACE_ID`/`HERDR_PANE_ID` (`compound-105`): a
    // compound-hosted seat has neither, only the one string `my_pane()`
    // already resolves either way.
    let who = match args.get("workspace") {
        Some(ws) => crate::place::Seat::new(ws),
        None => crate::place::Seat::new(crate::cmd_agent::my_pane()?),
    };
    let scope = crate::cmd_govern::governs(&store.governors(), &who)?;
    store.worklist(&scope)
}

/// Where the run is up to, and which of the four things is true of it.
///
/// The order of the questions is the order they override each other in. A held
/// list opens no barrier whatever its tasks say; a list that has not started
/// has passed nothing; and a barrier behind the position is shut until a
/// verdict is written on the group behind it.
fn state(store: &Store, w: &Worklist, p: &Position) -> State {
    let groups = w.groups();
    if w.status() == WorklistStatus::Parked {
        let flight = front(store, groups.get(p.at.unwrap_or(1) - 1), &p.members).waiting.len();
        return State::Shut {
            gate: Gate::Parked,
            prose: last_logged(w, PARKED).unwrap_or_default(),
            flight,
        };
    }
    if w.status() == WorklistStatus::Held {
        let flight = front(store, groups.get(p.at.unwrap_or(1) - 1), &p.members).waiting.len();
        return State::Shut {
            gate: Gate::Held,
            prose: last_logged(w, HELD).unwrap_or_default(),
            flight,
        };
    }
    let draft = w.status() == WorklistStatus::Draft;
    if draft {
        let overview = w.section("Overview").unwrap_or_default();
        if !overview.trim().is_empty() {
            return State::Shut { gate: Gate::Start, prose: fold(&overview), flight: 0 };
        }
    }

    // The barrier this run has reached: the one behind the group the position
    // is standing at, shut exactly when nothing is holding that group any more.
    //
    // It reads no verdict of its own and it no longer arithmetics its way back
    // a group. `Position::at` is the first group nobody has written a verdict
    // on, so the barrier in question is always the one behind `at` — including
    // when `at` is the last group there is, which is the case the old form
    // reached through `unwrap_or(p.of)` and which is not a formality:
    // `worklist-008`'s stop prose is the gate on phase two, and a run that fell
    // straight through to "nothing left" would pass the one barrier the whole
    // exercise exists for.
    //
    // **Every barrier is shut until `go`, and not only the ones carrying
    // prose.** The design says a group with no stop condition passes on landing
    // alone, and that is about the *judgement*: nothing is demanded of a reader
    // there, and `go` at such a barrier is one word with no argument. It is
    // still `go` that runs, because passing a barrier is three other things as
    // well — the at-most-one-running check, the sweep of the trees behind it,
    // and the report of which members touched the same file — and the whole
    // argument for the sweep being automatic is that a step nobody is made to
    // run is a step that happened zero times in two nights and left 18
    // worktrees. A `next` that named the next group here would put that step
    // back on the honour system it has already failed.
    if let Some(at) = p.at.filter(|_| !draft && p.at_barrier()) {
        return State::Shut {
            gate: Gate::After(at),
            prose: groups.get(at - 1).map(|g| g.stop.trim().to_string()).unwrap_or_default(),
            flight: 0,
        };
    }

    match p.at {
        None => State::Nothing,
        Some(at) => {
            let f = front(store, groups.get(at - 1), &p.members);
            match f.ready.is_empty() {
                true => State::Waiting(f),
                false => State::Ready(f),
            }
        }
    }
}

/// Split the group being run into what may start and what is already going.
///
/// **The claim decides, and it decides first.** Who is standing on a task is
/// the claim's to say — `crate::worklist` deliberately never asks — and it is
/// what separates the two answers that otherwise look identical: a member with
/// no branch has either never been started or has an agent on it that has not
/// committed yet, and spawning the second one twice is two agents in one tree.
/// Everything git can say comes after that.
fn front(store: &Store, g: Option<&Group>, members: &[Standing]) -> Front {
    let claims = store.claims();
    let mut ready: Vec<String> = Vec::new();
    let mut waiting: Vec<(Standing, String)> = Vec::new();
    for s in members.iter().filter(|s| !s.finished()) {
        if let Some(c) = claims.get(&s.id) {
            // Except that a member already reviewed is waiting on its landing,
            // not on its agent, and naming where the agent was sent a governor
            // after the claim when the work was sitting unlanded (`wsp-142`).
            let why = match s.settlement.settled() {
                true => s.note(),
                false => crate::cmd_agent::claim_where(c),
            };
            waiting.push((s.clone(), why));
        } else if matches!(
            s.landing,
            None | Some(Landing::Nothing | Landing::NoBranch | Landing::NoRepo)
        ) {
            // No branch and nobody holding it: nothing has run for this member.
            // A member with no repository to look in — design-only work — is
            // the same answer for a different reason, and it is the reason
            // `Landing::NoRepo` is not an error.
            //
            // `Nothing` is here because an empty branch is evidentially a
            // missing one — see `worklist::Landing::Nothing` — and it is the
            // ordinary state of a member whose tree was made and whose agent
            // then died before committing: the branch is a leftover of `wsp
            // checkout` and there is nobody in it. It is the claim above that
            // keeps this honest while somebody *is* in it, and this is the
            // line that makes the comment on this function true: a member the
            // predicate called finished never reached it at all.
            ready.push(s.id.clone());
        } else {
            waiting.push((s.clone(), s.note()));
        }
    }

    // The machine's half of the cap is `None` and that is not an oversight: it
    // is set per executor and the seat — the machine all of this actually runs
    // on — has no record to carry one (see `Machine::agents`). It goes through
    // `parallelism` rather than being ignored so that the day the seat gets a
    // record, this line is the only one that changes and the rule stays where
    // the rule is.
    let cap = g.and_then(|g| g.parallelism(None));
    let room = cap.map(|n| n.saturating_sub(waiting.len())).unwrap_or(ready.len()).min(ready.len());
    let capped = ready.len() - room;
    ready.truncate(room);
    Front { ready, waiting, capped, cap }
}

// ---- next -------------------------------------------------------------

/// `wsp worklist next [<slug>]` — what may start now, or the prose.
///
/// **The verb the whole concept turns on, and the one a governor runs on
/// repeat**, which is why it says one of four things and says each of them in
/// two lines. Ids and no titles: the caller is about to run `wsp spawn <id>`,
/// which prints the title itself, and seven titles here is seven lines on every
/// barrier check instead of one. `--json` carries the rest.
///
/// The one exception to two lines is a shut barrier, which carries the evidence
/// block [`barrier_evidence`] reads — the report of what the group behind it
/// put on the trunk, printed here while the verdict is being composed rather
/// than only by [`go`] after it has been given. The argument for that, and for
/// nowhere else, is on that function.
///
/// [`Reading::Landed`], and this is the one caller that has to pay for it. A
/// group is finished when every member's branch is on the trunk, because both
/// cheap signals are wrong: `done` never arrives, and `review` arrives before
/// the commit does — which is the `batch`'s costliest failure with a barrier's
/// authority behind it.
///
/// It reads and it never writes. A governor polls this; a verb that appended to
/// the log every time it was asked would leave a log made of its own polling.
/// The dangling members it finds are therefore printed here and written to the
/// log by [`go`], which happens once per barrier.
pub fn next(store: &Store, args: &Args) -> i32 {
    let (w, seat) = match named_list(store, args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let pos = worklist::position(store, &w, Reading::Landed);
    // A list somebody has finished with is not a run, and `next` is a running
    // verb. It answers rather than reporting a barrier that is nobody's to
    // pass: the four states are about a run, and this one is over.
    //
    // **It still says what is behind it**, and that is why the reading is paid
    // for above this and not below it. Marking a list `done` used to make a
    // member the floor stepped over invisible on every surface at once, since
    // this was the verb that named it — a status somebody sets is not a reason
    // to stop saying that a group was passed without one of its members. The
    // walk is the same one `next` pays for anywhere else and nothing polls a
    // finished list: `done` is where a governor's loop ends.
    if w.status() == WorklistStatus::Done {
        let p = Paint::new();
        if args.json() {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "worklist": w.id,
                    "status": "done",
                    "state": "done",
                    "at": pos.at,
                    "of": pos.of,
                    "behind": behind_json(&pos.slipped),
                    "unwritten": pos.unwritten,
                }))
                .unwrap_or_default()
            );
            return 0;
        }
        for line in behind_lines(&p, &pos.slipped, &p.bold("behind"), 8) {
            println!("{line}");
        }
        for line in unwritten_lines(&p, &pos.unwritten, &p.bold("unwritten"), 8) {
            println!("{line}");
        }
        println!("{} {}", w.id, p.dim("is done — nothing left to want from it"));
        return 0;
    }
    let st = state(store, &w, &pos);
    let gone = worklist::dangling(store, &w);
    // Read once, here, for both halves of the verb: the lines `report` draws
    // and the keys `next_json` sets are the same walk's two answers.
    let touched = barrier_evidence(store, &w, &pos, &st);

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&next_json(&w, &pos, &st, &gone, touched.as_ref()))
                .unwrap_or_default()
        );
        return 0;
    }
    report(&w, &pos, &st, &gone, seat, touched.as_ref());
    0
}

/// The list a reading verb is about: the one named, or the one this workspace
/// is the seat for.
fn named_list(store: &Store, args: &Args) -> Result<(Worklist, bool), i32> {
    match args.rest.get(1) {
        Some(needle) => match worklist_or_why(store, needle) {
            Ok(w) => Ok((w, false)),
            Err(why) => {
                eprintln!("{why}");
                Err(1)
            }
        },
        None => match seated(store, args) {
            Some(w) => Ok((w, true)),
            None => {
                eprintln!("wsp: no worklist named, and this workspace is the seat for none");
                eprintln!("     wsp worklist ls names them · wsp govern <slug> --take takes the seat");
                Err(1)
            }
        },
    }
}

/// `wsp worklist go`, with the slug where the caller needs one.
///
/// Left off when the workspace holds the seat, because that is the form the
/// governor's loop actually types and this line is read on every barrier of
/// every run. Named when it does not, because a command that will not work as
/// printed is worse than a longer one.
fn how(verb: &str, w: &Worklist, seat: bool) -> String {
    match seat {
        true => format!("wsp worklist {verb}"),
        false => format!("wsp worklist {verb} {}", w.id),
    }
}

/// Four states, and the line each of them ends on is the command to run next.
///
/// The shut-barrier state carries the evidence block when one was read — see
/// [`barrier_evidence`] for when that is — drawn between the prose being judged
/// and the verbs, because it is part of what the verdict rests on and not a
/// hint about what to run.
fn report(
    w: &Worklist,
    pos: &Position,
    st: &State,
    gone: &[String],
    seat: bool,
    touched: Option<&worklist::Touched>,
) {
    let p = Paint::new();
    let of = pos.of;

    // Printed in every state and above everything, which is what "a line that
    // cannot be missed" means. A member no task answers to has been archived or
    // deleted under a plan that still names it: it never holds the barrier, so
    // nothing else here will ever mention it again, and the moment to put
    // something back is while there are still groups ahead.
    if !gone.is_empty() {
        println!("{}  {}", p.bold("gone"), gone.join("  "));
        println!("      {}", p.dim("no task answers to these — nothing here removes them"));
    }
    // The other line that cannot be missed, and it is rarer and stranger. The
    // argument is above `behind_lines`; this is the caller that always had it.
    for line in behind_lines(&p, &pos.slipped, &p.bold("behind"), 8) {
        println!("{line}");
    }
    // Its counterpart: a barrier crossed with nothing written at it is a
    // reading nobody ever made, and this is where somebody standing at the
    // next barrier learns it.
    for line in unwritten_lines(&p, &pos.unwritten, &p.bold("unwritten"), 8) {
        println!("{line}");
    }

    match st {
        State::Ready(f) => {
            let mut tail = String::new();
            if !f.waiting.is_empty() {
                tail.push_str(&format!(" · {} in flight", f.waiting.len()));
            }
            if f.capped > 0 {
                if let Some(cap) = f.cap {
                    tail.push_str(&format!(" · x{cap} holds {} back", f.capped));
                }
            }
            // A list nobody has started: the ids are what group 1 *is*, and the
            // command is `go` and not `spawn`. Spawning here would leave the
            // run at `draft`, where routing does not find it and no barrier
            // exists to wait at.
            if w.status() == WorklistStatus::Draft {
                tail.push_str(&format!(" · {} starts the list", how("go", w, seat)));
            }
            println!(
                "group {} of {of} — {} may start now{tail}",
                pos.at.unwrap_or(0),
                f.ready.len()
            );
            println!("{}", f.ready.join("  "));
        }
        State::Waiting(f) => {
            let tail = match (f.capped, f.cap) {
                (0, _) | (_, None) => String::new(),
                (n, Some(cap)) => format!(" · x{cap} holds {n} back"),
            };
            println!("group {} of {of} — waiting on {}{tail}", pos.at.unwrap_or(0), f.waiting.len());
            let w_id = f.waiting.iter().map(|(s, _)| s.id.chars().count()).max().unwrap_or(8);
            for (s, note) in &f.waiting {
                println!(
                    "{}  {}  {}",
                    util::pad(&s.id, w_id),
                    util::pad(s.settlement.word(), 7),
                    p.dim(note)
                );
            }
        }
        State::Shut { gate, prose, flight } => {
            let (head, verbs) = match gate {
                Gate::Start => (
                    format!("not started — {of} groups · read this before starting it"),
                    format!(
                        "{} \"…\"  starts it · {} \"…\"  shelves it",
                        how("go", w, seat),
                        how("hold", w, seat)
                    ),
                ),
                Gate::Held => (
                    match flight {
                        0 => "held — nothing more starts".to_string(),
                        n => format!("held — nothing more starts · {n} still in flight, and finishing"),
                    },
                    format!("{} \"…\"  starts it again", how("go", w, seat)),
                ),
                Gate::Parked => (
                    match flight {
                        0 => "parked — nothing starts and no barrier is checked".to_string(),
                        n => format!("parked — nothing starts and no barrier is checked · {n} still in flight"),
                    },
                    format!("{}  {}", how("resume", w, seat), resumes_to(pos)),
                ),
                Gate::After(n) if prose.trim().is_empty() => (
                    // No prose is no judgement asked for, and it is still a
                    // barrier: the sweep behind it and the same-file report are
                    // `go`'s, and neither of them is anybody's to remember.
                    format!(
                        "group {n} of {of} finished — {}  passes the barrier",
                        how("go", w, seat)
                    ),
                    String::new(),
                ),
                Gate::After(n) => (
                    format!("group {n} of {of} finished — read this before going on"),
                    format!(
                        "{} \"…\"  to go on · {} \"…\"  to stop",
                        how("go", w, seat),
                        how("hold", w, seat)
                    ),
                ),
            };
            println!("{head}");
            if !prose.trim().is_empty() {
                println!();
                for line in util::wrap(prose.trim(), 72) {
                    println!("  {line}");
                }
                println!();
            }
            // The same block `go` prints once the barrier is behind it, read
            // while it stands shut — one walk, [`touched_lines`], both verbs.
            if let Some(t) = touched {
                for line in touched_lines(&p, t) {
                    println!("{line}");
                }
            }
            if !verbs.is_empty() {
                println!("{}", p.dim(&verbs));
            }
            // The rotation, taught where the decision it belongs to is made.
            //
            // A barrier is where a custodian's run is most expensive and most
            // replaceable: the verdict it is composing goes into the store
            // through `go`, and everything else it knows that is worth keeping
            // should already be in a task log or a decision. So this line says
            // what `core-049` made the default — pass the barrier, seat a fresh
            // custodian, end — instead of leaving the thread to grow across a
            // night of them. Only on a group's own barrier: there is nothing to
            // succeed at barrier zero, and none left to sequence behind the
            // last. The command is spelled rather than described, because an
            // agent improvising around a noun is how the old habit survived;
            // and since `core-050` it names the one verb rather than a three-
            // step composition whose last step could be skipped — `rotate`
            // confirms the successor's first turn before anything is ended,
            // which was the half a separate "then end your session" never did.
            if let Gate::After(n) = gate {
                if *n < of {
                    println!(
                        "{}",
                        p.dim(&format!(
                            "then rotate - one verb seats your successor, confirms its first \
                             turn, and arranges your ending: wsp govern {} --rotate",
                            w.id
                        ))
                    );
                }
            }
        }
        State::Nothing => {
            println!(
                "nothing left — {of} group{}, all finished · wsp worklist done {}",
                if of == 1 { "" } else { "s" },
                w.id
            );
        }
    }
}

/// The evidence at a shut barrier: the group behind it, read off the trunk.
///
/// Phase five's stop conditions tell the reader to consult this report — *"`go`
/// reports which members of a landed group touched one file; read that report
/// rather than assuming they stayed apart"* — and until now only `go` printed
/// it, after the verdict was written and the sweep had run. **Reading the
/// evidence required passing the barrier first.** A governor composes the
/// verdict standing in front of it, polling this verb; the report belongs
/// there, beside the prose being read, where the next group is being composed.
/// `--json` carries it to the reader least likely to go and open a second verb
/// for it.
///
/// Only there. `next` runs on repeat, and the walk under the report is real
/// git — one reflog per repository and one diff per land, see
/// [`worklist::touched`] — so a barrier whose group has not finished, or a hold
/// taken in the middle of one, reads nothing at all. And where nothing was
/// read, [`next_json`] omits the keys outright instead of carrying empty
/// arrays: an absent walk must not be machine-readable as a clean one, which is
/// the absence-as-evidence this report exists to refuse, one indirection over.
///
/// Not on a draft either, whatever its members look like: a plan's finished
/// groups were finished before the list existed, `go` stamps them *already
/// finished when the list started* and reports nothing when it starts. This is
/// that same decision read from the other side of barrier zero.
///
/// The position walk above is `Reading::Landed` work too, and it stays its own
/// walk: it asks whether anything is outstanding, the reflog asks what each
/// land added, and no git question answers both. Within the report itself the
/// reflog is read once per repository however many members share it — the cost
/// `worklist::touched` was priced at, and the reason polling it at a barrier is
/// affordable.
fn barrier_evidence(store: &Store, w: &Worklist, pos: &Position, st: &State) -> Option<worklist::Touched> {
    match st {
        State::Shut { .. } => {}
        _ => return None,
    }
    // Held mid-group shuts no barrier over finished work; a draft has none
    // behind any group. What is left is a gate standing in front of a group
    // whose work is over — the one state the walk has an answer for.
    // Nor on a parked one: nobody is deciding that barrier while it is paused,
    // and `next` polled on a pause would walk git for a reader who is not there.
    if matches!(w.status(), WorklistStatus::Draft | WorklistStatus::Parked) || !pos.at_barrier() {
        return None;
    }
    let members = w.groups().get(pos.at? - 1)?.members.clone();
    Some(worklist::touched(store, &members))
}

fn next_json(w: &Worklist, pos: &Position, st: &State, gone: &[String], touched: Option<&worklist::Touched>) -> serde_json::Value {
    let mut v = json!({
        "worklist": w.id,
        "status": w.status().as_str(),
        "at": pos.at,
        "of": pos.of,
        // `state` already says `barrier`, but it says it for `start` and `held`
        // too, and a governor polling this wants to know that the *work* of the
        // group at `at` is over without matching on the gate string.
        "barrier": pos.at_barrier(),
        // What `report` prints and this object did not, which made the machine
        // half of the same verb the quieter one — and a governor polling
        // `--json` is the reader least likely to go and look.
        "behind": behind_json(&pos.slipped),
        "unwritten": pos.unwritten,
        "dangling": gone,
    });
    match st {
        State::Ready(f) | State::Waiting(f) => {
            v["state"] = json!(if f.ready.is_empty() { "waiting" } else { "ready" });
            v["start"] = json!(f.ready);
            v["cap"] = json!(f.cap);
            v["held_back"] = json!(f.capped);
            v["waiting"] = json!(f
                .waiting
                .iter()
                .map(|(s, note)| json!({ "id": s.id, "status": s.settlement.word(), "note": note }))
                .collect::<Vec<_>>());
        }
        State::Shut { gate, prose, flight } => {
            v["state"] = json!("barrier");
            v["gate"] = json!(match gate {
                Gate::Start => "start".to_string(),
                Gate::Held => "held".to_string(),
                Gate::Parked => "parked".to_string(),
                Gate::After(n) => format!("after {n}"),
            });
            v["prose"] = json!(prose);
            v["in_flight"] = json!(flight);
            // The evidence, where a group stands behind the gate to have
            // produced any — [`barrier_evidence`] names the states those are.
            // Key names are `go --json`'s, so one parser reads either verb.
            // Omitted rather than emptied elsewhere: present-but-empty on
            // every poll would read as "checked and clean" about walks that
            // never ran.
            if let Some(t) = touched {
                v["same_file"] = json!(t
                    .overlap
                    .shared
                    .iter()
                    .map(|(f, who)| json!({ "file": f, "members": who }))
                    .collect::<Vec<_>>());
                v["unread"] = json!(t.overlap.unread);
                v["landed"] = landed_json(&t.landed);
            }
            // The rotation, as the command rather than a boolean — this key is
            // the machine reader's copy of the dim line the text prints, and
            // like every line here it names what to run. Absent at barrier zero
            // and behind the last group, where the text says nothing either.
            // Additive: a parser that has never heard of it reads a barrier
            // exactly as before (`core-049`). The key keeps its name while the
            // value became `govern --rotate` (`core-050`), which seats the
            // successor, confirms its first turn, then moves the seat — a
            // renamed key would break a reader for the benefit of a word.
            if let Gate::After(n) = gate {
                if *n < w.groups().len() {
                    v["reseat"] = json!(format!("wsp govern {} --rotate", w.id));
                }
            }
        }
        State::Nothing => v["state"] = json!("finished"),
    }
    v
}

// ---- go ---------------------------------------------------------------

/// `wsp worklist go [<slug>] ["the verdict"]` — start a list, or pass a
/// barrier.
///
/// Four things happen here and only the first is about prose.
///
/// 1. **The verdict is recorded**, where the barrier asked for one. wsp does
///    not make the judgement: `fork`'s real rule was *"if any of the three goes
///    badly, flag and stop rather than push through"*, which no boolean
///    expresses. What wsp contributes is the **obligation** to make one and the
///    record that one was made — which is the thing a handbook nobody is made
///    to re-read could not contribute. A barrier with no stop prose asks for
///    nothing and is passed with three words.
/// 2. **The at-most-one-running constraint is checked**, because this is the
///    moment a person can act on it. A task may be in one *running* worklist
///    and in any number of drafts, held lists and finished ones.
/// 3. **The trees of every group behind the barrier are swept**, unless
///    `--keep`. See [`worklist::sweep`] — and note that `--keep` *defers*
///    rather than opts out, which is why [`worklist::Sweep::earlier`] is
///    printed here and is not optional.
/// 4. **The members of the group that just landed are reported where two of
///    them touched the same file.** The `batch`'s evidence for the composition
///    rule was an absence, which cannot be acted on; this is an observation,
///    and it arrives exactly when the next group is being composed.
///
/// Nothing spawns. `next` names what may start and the governor runs `wsp
/// spawn` per member: the moment the queue spawns it needs a spawn policy —
/// kind, tier, machine, mandate — and every one of those was a judgement in all
/// three real runs.
///
/// Two flags. `--keep` leaves the trees standing for one barrier — and says so,
/// because it defers rather than opts out. `-n` is a dry run of the whole verb:
/// what it would record, what it would sweep, and what the group that landed
/// touched, with nothing written and nothing removed.
pub fn go(store: &Store, args: &Args) -> i32 {
    let (w, said, seat) = match list_and_words(store, args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    go_with(store, args, w, said, seat)
}

/// `go` once the list and the sentence are in hand. [`followup`]'s non-blocking
/// form passes the barrier through here, with a verdict it composes itself.
fn go_with(store: &Store, args: &Args, mut w: Worklist, said: String, seat: bool) -> i32 {
    if w.status() == WorklistStatus::Done {
        eprintln!("wsp: `{}` is done — nothing in it is waiting to start", w.id);
        return 1;
    }
    // `wsp-173`: before anything else reads the list, because this is the
    // refusal the status exists for. A `go` here would be read as "carry on",
    // and at a barrier `go` is the pass — wsp-process group 3 was passed
    // unchecked on 2026-10-05 by a governor who meant to resume.
    if w.status() == WorklistStatus::Parked {
        eprintln!("wsp: `{}` is parked — `go` passes barriers, and a pause is not one", w.id);
        eprintln!("     {}  returns it to where it stood, with any barrier there still owed", how("resume", &w, seat));
        return 1;
    }
    let pos = worklist::position(store, &w, Reading::Landed);
    let st = state(store, &w, &pos);

    // Checked at every `go` and not only at the first, because a group ahead of
    // the work can be edited by hand between two barriers, and this is the last
    // moment before those members are named as startable.
    if let Some(code) = only_one_running(store, &w, &pos) {
        return code;
    }

    // Starting is not conditional on a barrier being shut. A list with nothing
    // written in its `## Overview` has no prose at barrier zero and so reads as
    // `Ready`, and `go` is still what starts it: spawning members of a draft
    // list leaves the run at `draft`, where routing does not find it and there
    // is no barrier to wait at.
    let starting = w.status() == WorklistStatus::Draft;
    let shut = match &st {
        State::Shut { gate, prose, .. } => Some((gate, prose.clone())),
        _ => None,
    };
    if !starting && shut.is_none() {
        // Not an error: a governor that types `go` twice has done nothing
        // wrong and wants to know where it is, which is what `next` says. No
        // evidence block here: no barrier is shut, so no group stands behind
        // one to have touched anything — `next` is where that is read.
        //
        // **Except when a member is what is holding it.** `wsp-163`: a
        // governor told a member's pane that more was owed, the row moved to
        // `doing`, and the barrier it had already opened went on reading as shut
        // — so `go` refused, correctly, with a sentence that named nothing and
        // read as "you already did this". `at_barrier` was the right predicate
        // all along; the message is what did not say so. A barrier that has
        // already opened is named here too, because that is the case where the
        // governor most needs to know a check is running on a group that moved.
        let held: Vec<&Standing> = pos.holding();
        if held.is_empty() {
            println!("{}", Paint::new().dim("no barrier is shut — nothing to pass"));
        } else {
            println!(
                "{}",
                Paint::new().yellow(&format!(
                    "nothing to pass: {} is still holding group {}",
                    held.iter().map(|s| format!("{} ({})", s.id, s.settlement.word())).collect::<Vec<_>>().join(", "),
                    pos.at.unwrap_or(1)
                ))
            );
            for s in &held {
                println!("  {}  {}", s.id, Paint::new().dim(&s.note()));
            }
            if let Some(at) = pos.at {
                let tasks = store.tasks();
                let open: Vec<&str> = tasks
                    .iter()
                    .filter(|t| crate::cycle::is_barrier(t, &w.id, at))
                    .filter(|t| matches!(t.status(), crate::model::Status::Doing))
                    .map(|t| t.id.as_str())
                    .collect();
                if !open.is_empty() {
                    println!(
                        "  {}",
                        Paint::new().dim(&format!(
                            "{} opened before this — its agent has been told what changed",
                            open.join(" ")
                        ))
                    );
                }
            }
            println!(
                "{}",
                Paint::new().dim(&format!(
                    "`wsp reopen <id> \"what is owed\"` is how work is sent back; it moves the row and tells the pane"
                ))
            );
        }
        report(&w, &pos, &st, &worklist::dangling(store, &w), seat, None);
        return 0;
    }
    let prose = shut.as_ref().map(|(_, prose)| prose.clone()).unwrap_or_default();

    // **The design's one piece of machinery around judgement.** Where there is
    // prose, the next group is not named until somebody has written a sentence
    // about it, and the sentence is dated into the record.
    if !prose.trim().is_empty() && said.trim().is_empty() {
        eprintln!("wsp: this barrier has something written at it, and passing it takes a sentence");
        eprintln!();
        for line in util::wrap(prose.trim(), 72) {
            eprintln!("  {line}");
        }
        eprintln!();
        eprintln!("     {} \"what you decided\"   · `-` reads it from a stream", how("go", &w, seat));
        eprintln!("     {} \"why\"                stops instead", how("hold", &w, seat));
        return 1;
    }

    let mut groups = w.groups();
    let resuming = matches!(shut, Some((Gate::Held, _)));

    // Which barrier was crossed, taken from the gate rather than from the
    // ordinal. Starting a list crosses none: the groups a freshly-started list
    // has already "passed" are work that was finished before it existed, and
    // taking their trees would be a blast radius nobody at this barrier asked
    // for. Resuming a held list crosses the barrier in front of it only if that
    // barrier is still shut — a `hold` taken in the middle of a group has none,
    // and holding does not move the position, so the barrier a resume crosses
    // is simply the one the position is standing at.
    let crossed = match (&shut, starting) {
        (_, true) => None,
        (Some((Gate::After(n), _)), _) => Some(*n),
        (Some((Gate::Held, _)), _) => pos.at.filter(|_| pos.at_barrier()),
        _ => None,
    };

    // How much of the list was already finished before anybody started it.
    // A draft has no barriers behind its groups, so its position is the first
    // group not finished and everything before it is work that was done before
    // the list existed. Nothing is removed on this: `crossed` is `None` when
    // starting, so no barrier is passed and no tree is swept.
    let already = match starting {
        true => pos.at.map(|at| at - 1).unwrap_or(pos.of),
        false => 0,
    };

    // Read before anything is removed, though nothing now depends on that
    // order: the report and the per-member record are both off the trunk's
    // reflog, which outlives the trees and the branches the sweep takes. See
    // `cmd_checkout::Landings`.
    let touched = match crossed {
        Some(n) => Some(worklist::touched(store, &groups[n - 1].members)),
        None => None,
    };

    // `-n` is a dry run of the **whole verb**, not of the sweep alone. Half a
    // dry run — the verdict written for real, the trees only imagined — is a
    // state nobody typing `-n` is asking for, and it is one that leaves the
    // barrier passed with the cleanup still to do.
    let dry = args.has("dry-run");
    let swept = match (crossed, args.has("keep")) {
        // The very position the barrier was read off, handed on rather than
        // recomputed: two walks under one `rm -rf` is two answers that can
        // disagree, and the one holding the removal is the wrong one to be
        // second. That is `worklist::sweep`'s own seam and its argument.
        (Some(_), false) => match worklist::sweep(store, &pos, dry) {
            Ok(s) => Some(s),
            Err(why) => {
                eprintln!("wsp: the sweep was refused: {why}");
                None
            }
        },
        _ => None,
    };

    // The record. A verdict goes on the group the barrier is behind, so it
    // travels with the group rather than with an ordinal that is rewritten on
    // every write.
    match starting {
        // Groups already finished when the list started were passed by nobody,
        // and marking them says so rather than leaving a run that has to be
        // walked through barriers for work it never did.
        true => {
            for g in groups.iter_mut().take(already) {
                if g.verdict.trim().is_empty() {
                    g.verdict = format!("{} already finished when the list started", util::now_iso());
                }
            }
        }
        false => {
            if let Some(n) = crossed {
                groups[n - 1].verdict = verdict_of(&said);
                // The association between member and commits, written while it
                // is still in hand: the sweep this call just ran deletes the
                // branch that was the only other place it lived, and what
                // outlives that is a machine-local reflog expiring at gc.
                if let Some(t) = &touched {
                    groups[n - 1].landed = t.landed.clone();
                }
            }
        }
    }
    if starting || resuming {
        w.set_status(WorklistStatus::Running);
    }

    // The log names the members and not the ordinal, for the reason every other
    // line in it does: an ordinal is a position and is rewritten the moment a
    // group is inserted above it.
    let entry = match (starting, resuming, crossed) {
        (true, _, _) => format!("started · {}", said.trim()),
        (_, true, _) => format!("started again · {}", said.trim()),
        (_, _, Some(n)) => format!("passed {} · {}", groups[n - 1].members.join(" "), said.trim()),
        _ => format!("go · {}", said.trim()),
    };
    w.log(entry.trim_end_matches(" · ").trim_end());

    // Written here rather than by `next`, which a governor polls: once per
    // barrier is a record and once per poll is a log made of its own polling.
    let gone = worklist::dangling(store, &w);
    if !gone.is_empty() {
        w.log(&format!("no task answers to {}", gone.join(" ")));
    }

    let msg = format!("go {}", w.id);
    if !dry {
        let saved = save(store, &mut w, &groups, "go", &msg);
        if saved != 0 {
            return saved;
        }
        store.log_event(
            "worklist-go",
            json!({ "id": w.id, "passed": crossed, "started": starting, "verdict": said }),
        );
        crate::cycle::poke_list(store, &w.id, "go", crossed);
    }

    // Read here rather than at the top, because `go` is what puts the list
    // into `running` and a list is only asked ahead of a project chain once it
    // is running: this is the state the members about to be named will raise
    // their hands into.
    let list_seat = seat_on(store, &w);

    // The state on the far side of what this call just did, so the answer can
    // be given here instead of pointed at (`core-049`). Every turn an agent
    // spends polling is a whole context re-read, and `go` is run exactly when
    // "what may start now" changes — which is why the pointer stood here: it
    // was one poll per barrier, per group, for every run. A dry run shows
    // nothing, because nothing moved; answering with post-state would be a
    // lie wearing the same words.
    let ahead = match dry {
        true => None,
        false => {
            let pos = worklist::position(store, &w, Reading::Landed);
            let st = state(store, &w, &pos);
            Some((pos, st))
        }
    };

    if args.json() {
        let mut out = json!({
            "worklist": w.id,
            "dry_run": dry,
                // The routing answer and not the occupancy: `false` is a hand
                // raised on a member reaching whatever seat sits above its own
                // project. A caller driving this verb sees no output at all,
                // and that is the case this whole line exists for.
                "seated": matches!(list_seat, SeatOn::Held),
                "started": starting,
                "passed": crossed,
                "verdict": said,
                "same_file": touched.as_ref().map(|t| t.overlap.shared.iter().map(|(f, who)| json!({ "file": f, "members": who })).collect::<Vec<_>>()).unwrap_or_default(),
                "unread": touched.as_ref().map(|t| t.overlap.unread.clone()).unwrap_or_default(),
                // The record itself, whole: a parser is not the reader the
                // count-only plan drawing is for.
                "landed": touched.as_ref().map(|t| landed_json(&t.landed)).unwrap_or_default(),
                "swept": swept.as_ref().map(|s| json!({
                    "removed": s.swept.removed,
                    "branches": s.swept.branches,
                    "kept": s.swept.kept.iter().map(|(t, why)| json!({ "task": t, "why": why })).collect::<Vec<_>>(),
                    "earlier": s.earlier,
                })),
                "dangling": gone,
            });
        // The far side, under the key the `next` reader already parses — same
        // shape, same omissions (no evidence walk ran here, so `same_file` and
        // friends are absent from it rather than empty). A driver that has
        // never heard of the key reads this verb exactly as before.
        if let Some((pos, st)) = &ahead {
            out["next"] = next_json(&w, pos, st, &gone, None);
        }
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }

    let p = Paint::new();
    let would = if dry { "would be " } else { "" };
    match (starting, resuming, crossed) {
        (true, _, _) => println!("{} {}", p.bold(&w.id), p.dim(&format!("{would}started"))),
        (_, true, _) => println!("{} {}", p.bold(&w.id), p.dim(&format!("{would}started again"))),
        (_, _, Some(n)) => {
            println!("{} {}", p.bold(&w.id), p.dim(&format!("group {n} {would}passed")))
        }
        _ => println!("{}", p.bold(&w.id)),
    }
    if dry {
        println!("{}", p.dim("-n — nothing was written and no tree was removed"));
    }
    if crossed.is_some() {
        if let Some(line) = verdict_notice(&w.id, &said, groups.len()) {
            println!("{}", p.dim(&line));
        }
    }
    if !gone.is_empty() {
        println!("{}  {}", p.bold("gone"), gone.join("  "));
    }
    // Only where a barrier was crossed. Starting a list has no group behind it
    // to have touched anything, and "none" there would be an answer to a
    // question nobody asked.
    if let Some(t) = &touched {
        for line in touched_lines(&p, t) {
            println!("{line}");
        }
    }
    say_swept(&p, swept.as_ref(), crossed.is_some() && args.has("keep"), dry);
    // Beside the line naming what may start, because it is about those same
    // members: a hand raised on one of them reaches this list's seat, and with
    // nobody in it, the project chain each member happens to sit in. Referencing
    // a task across a project boundary rather than moving it — `wsp-088` d1 —
    // only has its benefit once the list has a seat; without one it has the cost
    // of moving and none of the gain, and it looks like it works.
    //
    // Said at every barrier and not only at the start, because a barrier is
    // where the next group's members are named as startable and that is what
    // makes new hands possible. It stops the moment somebody takes the seat.
    //
    // **Not a refusal, and `go` does not take the seat.** A list may legitimately
    // be run by somebody who has not taken it, and taking one stands the
    // workspace down from whatever it held before — a consequence nobody typing
    // `go` asked for.
    match &list_seat {
        SeatOn::Held => {}
        rest => {
            // `wsp-202`: a list wsp runs is seated by the daemon's next tick, as
            // a post nobody has filled, and until then what it is told waits on
            // its own scope. The project chain is a list run by hand's answer.
            // Not where somebody stood it down, or where it is held from another
            // machine: the reconciler leaves both alone, and so must the sentence.
            let stood = crate::cmd_govern::stood_at(&store.governors(), &w.id).is_some();
            let until = match (w.runs_itself(), rest) {
                (true, SeatOn::Nobody) if !stood => "wsp seats its own governor on the daemon's next tick".to_string(),
                (true, _) => format!("nothing is seated for it · wsp govern {} --take", w.id),
                (false, _) => format!("hands raised on its members reach their projects' seats · wsp govern {} --take", w.id),
            };
            println!("{}  {}", p.bold("no seat"), p.dim(&until));
            if let SeatOn::Elsewhere(host) = rest {
                println!(
                    "{}  {}",
                    util::pad("", 7),
                    p.dim(&format!("held from {host} — not a machine a hand raised here reaches"))
                );
            }
        }
    }
    // The answer where the pointer used to be: `report` is the same block
    // `next` draws — ready ids, waiting members, the barrier if the far side of
    // this barrier is already another one — and `gone` is left out because the
    // lines above already said it, and `touched` because it did too. A dry run
    // keeps the pointer, since nothing moved to report.
    match &ahead {
        Some((pos, st)) => report(&w, pos, st, &[], seat, None),
        None => println!("{}", p.dim(&format!("{}  what may start now", how("next", &w, seat)))),
    }
    0
}

/// A task is in one *running* worklist and in as many plans as anybody cares to
/// write.
///
/// Checked here because this is where somebody can act on it — the moment a
/// list starts, and every barrier after it, since a group ahead of the work can
/// be edited between two of them. What it protects is the routing step in front
/// of `cmd_govern::seat_for`, which has to have one answer: a hand raised at 3am
/// must reach whoever is running the list this task is in tonight, and two
/// running lists holding it is a question with no answer.
///
/// **Only members this list has not already passed.** A finished member sitting
/// in a second list is a fact about a plan and not about tonight, and refusing
/// a barrier over it would stop the sweep and the report as well as the naming
/// — a heavy answer to something nothing is going to act on.
fn only_one_running(store: &Store, w: &Worklist, pos: &Position) -> Option<i32> {
    let running = worklist::Running::read(store);
    let done: Vec<&str> = pos.passed.iter().map(|b| b.member.id.as_str()).collect();
    let clash: Vec<String> = w
        .groups()
        .iter()
        .flat_map(|g| g.members.clone())
        .filter(|m| !done.contains(&m.as_str()))
        .filter_map(|m| {
            running.list_of(&m).filter(|l| *l != w.id).map(|l| format!("{m} is in `{l}`"))
        })
        .collect();
    if clash.is_empty() {
        return None;
    }
    eprintln!("wsp: `{}` cannot run — a task runs in one worklist at a time", w.id);
    for line in clash.iter().take(6) {
        eprintln!("     {line}");
    }
    if clash.len() > 6 {
        eprintln!("     …and {} more", clash.len() - 6);
    }
    // `park` and not `hold`: setting one list aside for another is a person's
    // pause, and a hold's way back is `go`, which passes whatever barrier the
    // held list was standing at. `wsp-173`.
    eprintln!("     take them out of one of the two, or park the other — wsp worklist park <slug> \"why\"");
    Some(1)
}

/// Whether anybody answers for this list, read from this machine.
enum SeatOn {
    /// Somebody is in it, here.
    Held,
    /// No record at all, or a post standing empty.
    Nobody,
    /// Filled from another machine, carried so the line can say which rather
    /// than claim the list has never had a seat.
    Elsewhere(String),
}

/// The first step of [`crate::cmd_govern::seat_for`]'s walk, asked of the list
/// rather than of a task: a member's raised hand tries its list's seat and
/// falls through to the member's own project chain when nobody is in it.
///
/// Read through [`crate::cmd_govern::slots`] so this answer and routing's are
/// one answer. A record filled from another host is a seat nothing raised here
/// can reach, and `seat_of` already reads it as absent rather than as
/// somewhere — this must agree with it or the line would promise a seat that
/// no hand arrives at.
fn seat_on(store: &Store, w: &Worklist) -> SeatOn {
    let slot = crate::cmd_govern::slots(&store.governors()).into_iter().find(|s| s.scope == w.id);
    match slot {
        Some(s) if s.filled() => SeatOn::Held,
        Some(s) if s.elsewhere() => SeatOn::Elsewhere(s.host),
        _ => SeatOn::Nobody,
    }
}

/// What the group behind the barrier put on the trunk: the same-file report,
/// then one line naming what each member landed.
///
/// **One shape, two verbs.** [`go`] prints it once the barrier is behind it;
/// `next` prints it while the barrier stands shut — see
/// [`barrier_evidence`] for why both moments read it and why nowhere else
/// does. The lines are built rather than printed so a test can assert on
/// exactly what either verb says, which is the same split [`verdict_lines`]
/// and [`behind_lines`] make.
///
/// Silence is the answer nearly every time and it is the answer worth having:
/// the composition rule held. It says so in one line rather than printing
/// nothing, because "no output" is exactly the absence the `batch` could not
/// act on.
fn touched_lines(p: &Paint, t: &worklist::Touched) -> Vec<String> {
    let mut out = Vec::new();
    let o = &t.overlap;
    if o.shared.is_empty() && o.unread.is_empty() {
        out.push(p.dim("same file  none — no two members touched one file"));
    } else {
        for (file, who) in &o.shared {
            out.push(format!("{}  {}  {}", p.bold("same file"), file, p.dim(&who.join(" "))));
        }
        // The honest half. A member nobody could place contributes no overlaps,
        // so leaving it out would turn "we could not look" into "we looked and
        // it was clean" — the absence-as-evidence this report exists to replace.
        if !o.unread.is_empty() {
            out.push(format!(
                "{}  {}",
                p.dim(&util::pad("", 9)),
                p.dim(&format!("{} could not be read back off the trunk", o.unread.join(" ")))
            ));
        }
    }
    // One line, the commit shortened for reading, because this is a receipt and
    // not the record: the full hashes are on the group, where they are kept.
    // `@?` is a member nobody could place, said in kind rather than left out.
    let said: Vec<String> = t
        .landed
        .iter()
        .map(|e| {
            let at: String =
                e.commit.as_deref().map(|c| c.chars().take(7).collect()).unwrap_or_else(|| "?".into());
            format!("{}@{} {}", e.member, at, e.files.len())
        })
        .collect();
    out.push(format!("{}  {}", p.bold("landed"), p.dim(&said.join(" · "))));
    out
}

/// The record itself, whole, for a parser.
fn landed_json(landed: &[Landed]) -> serde_json::Value {
    json!(landed
        .iter()
        .map(|e| json!({ "member": e.member, "commit": e.commit, "files": e.files }))
        .collect::<Vec<_>>())
}

/// What the sweep did, and the one line of it that is an obligation.
fn say_swept(p: &Paint, swept: Option<&worklist::Sweep>, kept: bool, dry: bool) {
    if kept {
        // `--keep` from the other side: it is a deferral, and saying so here is
        // the same honesty `earlier` owes at the barrier that finally takes
        // them. Somebody who keeps a tree to go and look at it should know how
        // long they have.
        println!("{}", p.dim("kept — no trees swept · the next barrier takes them, --keep defers"));
        return;
    }
    let Some(s) = swept else { return };
    if s.swept.removed.is_empty() && s.swept.kept.is_empty() {
        return;
    }
    let what = if dry { "would sweep" } else { "swept" };
    // **The obligation.** `earlier` names the trees an earlier `--keep` spared,
    // and it has to arrive where the decision is being made: somebody who kept
    // a tree in order to go and look at it would otherwise lose it one barrier
    // later without being told, which is the failure `--keep` exists to
    // prevent, delayed by one group.
    let n = s.swept.removed.len();
    let trees = if n == 1 { "tree" } else { "trees" };
    match s.earlier.len() {
        0 => println!("{} {n} {trees}", p.dim(what)),
        earlier => {
            println!("{} {n} {trees}, {earlier} of them from groups passed earlier", p.dim(what));
            println!("      {}", p.dim(&format!("{} — an earlier --keep spared these", s.earlier.join("  "))));
        }
    }
    if !s.swept.branches.is_empty() {
        println!(
            "      {}",
            p.dim(&format!("{} — the branch outlived the tree, it holds commits the trunk has not", s.swept.branches.join("  ")))
        );
    }
    for (task, why) in &s.swept.kept {
        println!("{}  {}  {}", p.dim("kept"), task, p.dim(why));
    }
}

// ---- hold -------------------------------------------------------------

/// `wsp worklist hold [<slug>] "why"` — start nothing more.
///
/// **It means exactly that and nothing stronger.** Agents already running are
/// left to finish: work in flight cannot be unwound, and a verb that pretended
/// otherwise would be promising the one thing in this design that could not be
/// built. So this writes a decision and stops the queue handing out any more
/// work; what is already in a tree lands the way it would have landed.
///
/// The sentence is required. `held` is a state somebody walks up to hours
/// later, and a stop with no reason on it is the notification failure in its
/// purest form — a run that will not go on and nothing anywhere saying why.
pub fn hold(store: &Store, args: &Args) -> i32 {
    let (mut w, said, seat) = match list_and_words(store, args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if said.trim().is_empty() {
        eprintln!("wsp: {} \"why\" — a hold with no reason on it is a run nobody can restart", how("hold", &w, seat));
        eprintln!("     `--from FILE` reads the sentence out of a file and `-` off a stream, where a shell never sees it");
        return 2;
    }
    if w.status() == WorklistStatus::Done {
        eprintln!("wsp: `{}` is done — there is nothing left in it to stop", w.id);
        return 1;
    }
    if w.status() == WorklistStatus::Held {
        println!("{} {}", w.id, Paint::new().dim("is already held"));
        return 0;
    }
    // A paused run has no barrier being decided, so there is no "does not
    // pass" to record — and taking it would turn a person's pause into a
    // barrier's hold, whose way back is `go`, the pass. A barrier check still
    // in flight from before the pause is told this and can say so on its row.
    if w.status() == WorklistStatus::Parked {
        eprintln!("wsp: `{}` is parked — no barrier is being decided while it is paused", w.id);
        eprintln!("     {}  first, and the barrier it stands at is owed again", how("resume", &w, seat));
        return 1;
    }

    let pos = worklist::position(store, &w, Reading::Landed);
    let flight = front(store, w.groups().get(pos.at.unwrap_or(1) - 1), &pos.members);

    w.set_status(WorklistStatus::Held);
    w.log(&format!("{HELD} {}", said.trim()));
    let groups = w.groups();
    let msg = format!("hold {}", w.id);
    let code = save(store, &mut w, &groups, "hold", &msg);
    if code != 0 {
        return code;
    }
    store.log_event("worklist-held", json!({ "id": w.id, "why": said }));
    crate::cycle::poke_list(store, &w.id, "hold", None);
    // `wsp-158`: a `hold` stops nothing that is already in flight, and says so
    // above. What it must not leave behind is an agent with nothing left to do —
    // a verifier that has recorded its verdict, a member, a helper. `wsp
    // release` is refused for a seat, so the honest ending is the whole one, and
    // it is the seat that is told what happened. Not the barrier check: this is
    // usually its turn, and it ends at `review` on a tick (`wsp-209`).
    crate::cycle::end_what_the_run_opened(store, &crate::cycle::Fleet, &w.id, pos.at, crate::cycle::Closing::Held);

    if args.json() {
        println!(
            "{}",
            json!({
                "worklist": w.id,
                "status": w.status().as_str(),
                "why": said,
                "in_flight": flight.waiting.iter().map(|(s, _)| s.id.clone()).collect::<Vec<_>>(),
            })
        );
        return 0;
    }
    let p = Paint::new();
    println!("{} {}", p.bold(&w.id), p.dim("held — nothing more starts"));
    match flight.waiting.len() {
        0 => {}
        n => {
            // Said out loud rather than assumed: somebody who holds a run
            // expects it to stop, and what is in a tree is going to land anyway.
            println!(
                "{}",
                p.dim(&format!("{n} still in flight and left to finish — work in flight cannot be unwound"))
            );
            for (s, note) in &flight.waiting {
                println!("  {}  {}  {}", s.id, util::pad(s.settlement.word(), 7), p.dim(note));
            }
        }
    }
    println!("{}", p.dim(&format!("{} \"…\"  starts it again", how("go", &w, seat))));
    0
}

// ---- followup ---------------------------------------------------------

/// The mark a follow-up is logged under, and the one [`followups`] reads back.
const FOLLOWUP: &str = "follow-up by";
/// The mark a refused round is logged under. A different word on purpose:
/// [`followups`] must not read rows that went to the governor as rows the run
/// took.
const FOLLOWUP_REFUSED: &str = "follow-ups refused to";
/// How many rows one call may attach.
const FOLLOWUP_MAX: usize = 4;

/// `wsp worklist followup <slug> <task>… --blocking|--next --from FILE` — a
/// barrier check's verdict, with a short tail of rows attached to the run.
/// `wsp-210`.
///
/// **Why the barrier may extend its own run.** wsp-149's acceptance run turned
/// up small fixes, and they needed a third worklist to get worked on. Running
/// `wsp-process`, then `wsp-unattended`, then `wsp-acceptance-fixes` was one run
/// with a tail, and the agent that found the tail was the barrier check. It
/// could only write it into a verdict for a governor to compose by hand.
///
/// It files nothing. The rows already exist (`wsp add --parent`). This attaches
/// them and records the verdict, in one of two senses:
///
/// - **`--blocking`: "holds, pending follow-ups".** The group's own done-when is
///   not met. The rows join the group being run. This is the one exception to
///   the write-ahead-only window, and it is sanctioned because the barrier is
///   the reader the window protects. wsp spawns and verifies them like any
///   member. The check reviews its row, and a fresh check opens once they land.
///   The list stays `running`: a `hold` would stop the spawns the follow-ups
///   need.
/// - **`--next`: "passes, with follow-ups".** The rows become a group placed
///   straight after this one, ahead of anything planned. It inherits this
///   group's agent line, and its stop is built from the rows' done-whens. The
///   barrier is then passed through [`go_with`], so the pass is the ordinary
///   one: the sweep, the rotation, the next group started by the run.
///
/// **The limits keep it a tail and not a second plan.**
///
/// - **One round.** A group that already had follow-ups, either joined
///   (`--blocking`) or as the group itself (`--next`), is refused. Its rows
///   and verdict go to the governor as a decision, and the check still ends
///   with `go` or `hold`. Read off the log by member id, see [`followups`], so
///   renumbering cannot lose it.
/// - **[`FOLLOWUP_MAX`] rows.** Anything larger, or anything needing a design
///   call, is a `wsp ask` to the governor.
/// - **Only the barrier check, during its own barrier.** The caller's seat must
///   hold the open check row of the group being run. A member, a verifier and a
///   person at the CLI hold none. The governor still edits the list with the
///   composing verbs.
/// - **Logged**, naming the check row that added them.
pub fn followup(store: &Store, args: &Args) -> i32 {
    followup_by(store, args, &rows_here(store))
}

/// The rows this process's seat holds: the binding on its pane, and every claim
/// made by the agent sitting there. Two readings because a binding is derived
/// and free to be lost (`store`'s claims doc), while the claim outlives it.
fn rows_here(store: &Store) -> Vec<String> {
    let Some(pane) = crate::cmd_agent::my_pane() else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    if let Some(t) = store.bindings().get(&pane).and_then(|b| b.get("task_id")).and_then(|t| t.as_str()) {
        out.push(t.to_string());
    }
    if let Some(agent) = store.agent_in_seat(&pane) {
        out.extend(
            store
                .claims()
                .into_iter()
                .filter(|(_, c)| c.get("agent_id").and_then(|a| a.as_str()) == Some(agent.as_str()))
                .map(|(id, _)| id),
        );
    }
    out
}

/// [`followup`] with the caller's rows given, which is what makes the "only
/// the barrier check" rule testable without a seat.
pub(crate) fn followup_by(store: &Store, args: &Args, here: &[String]) -> i32 {
    const USAGE: &str = "usage: wsp worklist followup <slug> <task>… --blocking|--next --from FILE   (`--from -` reads stdin)";
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let blocking = match (args.has("blocking"), args.has("next")) {
        (true, false) => true,
        (false, true) => false,
        _ => {
            eprintln!("{USAGE}");
            eprintln!("       --blocking: the group holds until they land · --next: it passes, and they run next");
            return 2;
        }
    };
    let w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let typed: Vec<String> = args.rest.iter().skip(2).cloned().collect();
    if typed.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }

    // Who is asking, first: whatever else is wrong with the call, a caller
    // who is not this barrier's check is told that, and nothing about how a
    // check would have done it.
    let pos = worklist::position(store, &w, Reading::Landed);
    let tasks = store.tasks();
    let at = pos.at.filter(|_| w.status() == WorklistStatus::Running && pos.at_barrier());
    let row = at.and_then(|at| {
        tasks
            .iter()
            .filter(|t| crate::cycle::is_barrier(t, &w.id, at))
            .filter(|t| matches!(t.status(), crate::model::Status::Doing | crate::model::Status::Blocked))
            .find(|t| here.contains(&t.id))
            .map(|t| t.id.clone())
    });
    let (Some(at), Some(row)) = (at, row) else {
        match at {
            Some(at) => eprintln!("wsp: only the agent checking group {at}'s barrier may add follow-ups to `{}`", w.id),
            None => eprintln!("wsp: `{}` is not standing at a barrier being checked, so there is no verdict to attach follow-ups to", w.id),
        }
        eprintln!("     a governor composes the list with `wsp worklist add` and `group`; anybody else files the row with `wsp add --parent` and says so");
        return 1;
    };

    if typed.len() > FOLLOWUP_MAX {
        eprintln!("wsp: at most {FOLLOWUP_MAX} follow-ups in one call, and this names {}", typed.len());
        eprintln!("     a tail that size, or one that needs a design call, is the governor's: `wsp ask {row} -`");
        return 2;
    }
    let members = match resolve_members(store, &typed) {
        Ok(m) => m,
        Err(code) => return code,
    };
    let groups = w.groups();
    if let Some(code) = already_in(&groups, &members, &w.id) {
        return code;
    }
    // Rows wsp can start: `todo` and nobody's. A row somebody is already on
    // would sit in the group with nothing spawning it, and the barrier behind
    // it would wait on whoever that is.
    let claims = store.claims();
    let running = worklist::Running::read(store);
    for m in &members {
        let t = tasks.iter().find(|t| &t.id == m);
        let status = t.map(|t| t.status());
        if status != Some(crate::model::Status::Todo) || claims.contains_key(m) {
            let word = t.map(|t| t.status().as_str().to_string()).unwrap_or_default();
            eprintln!("wsp: {m} is {word}{} — a follow-up is a row wsp starts, so it has to be todo and unclaimed", if claims.contains_key(m) { " and claimed" } else { "" });
            return 1;
        }
        if let Some(other) = running.list_of(m).filter(|l| *l != w.id) {
            eprintln!("wsp: {m} is already in `{other}`, which is running — a task is in one running list");
            return 1;
        }
    }

    let verdict = match words(args, args.rest.len()) {
        Ok(v) if !v.trim().is_empty() => v,
        Ok(_) => {
            eprintln!("wsp: a follow-up records this barrier's verdict — `--from FILE`, or `--from -` for stdin");
            return 2;
        }
        Err(code) => return code,
    };

    // One round. Refused, but not dropped: the check found work, and what it
    // found reaches the governor as a decision.
    let g = &groups[at - 1];
    if let Some(prior) = spent_by(&w, &g.members) {
        return refuse_round(store, w, at, &row, &prior, &members, &verdict);
    }

    if blocking {
        followup_blocking(store, args, &w.id, at, &row, &members, &verdict)
    } else {
        followup_next(store, args, &w.id, at, &row, &members, &verdict)
    }
}

/// The rows join the group being run, and the barrier is checked again once
/// they land.
fn followup_blocking(store: &Store, args: &Args, id: &str, at: usize, row: &str, members: &[String], verdict: &str) -> i32 {
    // Under the lock, re-read: a `go` from elsewhere, or a second call by
    // this check, may have moved the list since it was first read.
    let outcome = store.locked(|| -> Result<Worklist, i32> {
        let mut w = store.worklist(id).ok_or(1)?;
        let mut groups = w.groups();
        if let Some(code) = still_this_barrier(store, &w, &groups, at, row) {
            return Err(code);
        }
        if let Some(code) = already_in(&groups, members, &w.id) {
            return Err(code);
        }
        groups[at - 1].members.extend(members.iter().cloned());
        w.log(&format!("{FOLLOWUP} {row} holds: {} · join group {at} · {verdict}", members.join(" ")));
        w.set_groups(&groups);
        if let Err(e) = store.save_worklist(&w) {
            eprintln!("wsp: write failed: {e}");
            return Err(1);
        }
        Ok(w)
    });
    let w = match outcome {
        Ok(w) => w,
        Err(code) => return code,
    };
    store.log_event(
        "worklist-followup",
        json!({ "id": w.id, "by": row, "blocking": true, "members": members, "verdict": verdict }),
    );
    store.git_commit(&format!("wsp: worklist {} group {at} holds pending follow-ups {}", w.id, members.join(" ")));
    crate::cycle::tell(
        store,
        &w,
        &format!(
            "Group {at}'s barrier in the {list} run holds, pending follow-ups: {ids}. {row} added them to the \
             group, and wsp starts and verifies them, then checks the barrier again once they land. Its verdict: {v}",
            list = w.id,
            ids = members.join(" "),
            v = util::truncate(verdict, 400),
        ),
    );
    crate::cycle::poke_list(store, &w.id, "followup", None);

    if args.json() {
        println!("{}", worklist_json(store, &w));
        return 0;
    }
    let p = Paint::new();
    println!("{} {}", p.bold(&w.id), p.dim(&format!("group {at} holds, pending follow-ups {}", members.join(" "))));
    println!(
        "  {}",
        p.dim(&format!(
            "wsp starts and verifies them, and a fresh check reads the group once they land — finish with `wsp review {row} -`"
        ))
    );
    0
}

/// The rows become the next group, and the barrier is passed.
fn followup_next(store: &Store, args: &Args, id: &str, at: usize, row: &str, members: &[String], verdict: &str) -> i32 {
    let stop = followup_stop(store, row, at, members);
    let outcome = store.locked(|| -> Result<Worklist, i32> {
        let mut w = store.worklist(id).ok_or(1)?;
        let mut groups = w.groups();
        if let Some(code) = still_this_barrier(store, &w, &groups, at, row) {
            return Err(code);
        }
        if let Some(code) = already_in(&groups, members, &w.id) {
            return Err(code);
        }
        // Straight after, never at the end: a tail found at barrier 2 is
        // about group 2's work, and a planned group 3 may depend on it.
        let planned = groups.len() - at;
        let agent = groups[at - 1].agent.clone();
        groups.insert(at, Group { members: members.to_vec(), agent, stop: stop.clone(), ..Group::default() });
        w.log(&format!(
            "{FOLLOWUP} {row} passes: {} · group {} (new), ahead of {planned} planned",
            members.join(" "),
            at + 1
        ));
        w.set_groups(&groups);
        if let Err(e) = store.save_worklist(&w) {
            eprintln!("wsp: write failed: {e}");
            return Err(1);
        }
        Ok(w)
    });
    let w = match outcome {
        Ok(w) => w,
        Err(code) => return code,
    };
    store.log_event(
        "worklist-followup",
        json!({ "id": w.id, "by": row, "blocking": false, "members": members, "verdict": verdict }),
    );
    store.git_commit(&format!("wsp: worklist {} follow-ups {} are group {}", w.id, members.join(" "), at + 1));

    // The pass is `go`'s own, so it is the ordinary one in every respect, and
    // the verdict on the group says the follow-ups were part of it.
    let said = format!("passes, with follow-ups {} as group {} — {verdict}", members.join(" "), at + 1);
    let code = go_with(store, args, w.clone(), said, false);
    if code != 0 {
        eprintln!(
            "wsp: {} are group {} of `{}`, but its barrier was not passed — `wsp worklist go {} --from FILE` passes it",
            members.join(" "),
            at + 1,
            w.id,
            w.id
        );
    }
    code
}

/// Whether the barrier a check is answering is still the one in front: its
/// row open, and no verdict on the group. Asked inside the lock.
fn still_this_barrier(store: &Store, w: &Worklist, groups: &[Group], at: usize, row: &str) -> Option<i32> {
    let open = store
        .find_task(row)
        .is_some_and(|t| matches!(t.status(), crate::model::Status::Doing | crate::model::Status::Blocked));
    let unpassed = groups.get(at - 1).is_some_and(|g| g.verdict.trim().is_empty());
    if open && unpassed && w.status() == WorklistStatus::Running {
        return None;
    }
    eprintln!("wsp: group {at}'s barrier in `{}` was settled while this was being read — nothing was attached", w.id);
    Some(1)
}

/// A group past its one round: the rows and the verdict go to the governor as
/// a decision, and the list's log says so.
fn refuse_round(store: &Store, w: Worklist, at: usize, row: &str, prior: &str, members: &[String], verdict: &str) -> i32 {
    let tasks = store.tasks();
    let named: Vec<String> = members
        .iter()
        .map(|m| match tasks.iter().find(|t| &t.id == m) {
            Some(t) => format!("{m} ({})", util::truncate(&t.title, 60)),
            None => m.clone(),
        })
        .collect();
    crate::cycle::tell(
        store,
        &w,
        &format!(
            "A decision for you in the {list} run: group {at} has had its one round of follow-ups ({prior} added \
             them), so its barrier check {row} found more and could not attach them: {rows}. Its verdict: {v} \
             Add them to the list yourself (`wsp worklist add {list} …`), or leave them in the backlog. The check \
             still ends with `go` or `hold`.",
            list = w.id,
            rows = named.join(", "),
            v = util::truncate(verdict, 400),
        ),
    );
    // Re-read under the lock: the copy in hand was read before the tell, and
    // writing it back whole would undo anything saved since.
    let saved = store.locked(|| -> Result<(), i32> {
        let mut fresh = store.worklist(&w.id).ok_or(1)?;
        fresh.log(&format!("{FOLLOWUP_REFUSED} {row}: {} · one round, spent by {prior} · sent to the governor", members.join(" ")));
        store.save_worklist(&fresh).map_err(|e| {
            eprintln!("wsp: write failed: {e}");
            1
        })
    });
    if let Err(code) = saved {
        return code;
    }
    store.git_commit(&format!("wsp: worklist {} follow-ups refused to {row}", w.id));
    eprintln!("wsp: group {at} of `{}` has had its follow-ups ({prior}) — one round, so these went to the governor as a decision", w.id);
    eprintln!("     finish the barrier with `wsp worklist go {} --from FILE` or `wsp worklist hold {} --from FILE`", w.id, w.id);
    1
}

/// Every follow-up the list's log records: the check row that added it, and
/// the rows it attached.
///
/// **The log and not a field on the group**, because the log is where the
/// verb's record has to be anyway (naming the row that added it), and a
/// second copy is a copy that can disagree. Read by member id rather than by
/// ordinal, so a group renumbered by an edit ahead of it is still found.
pub(crate) fn followups(w: &Worklist) -> Vec<(String, Vec<String>)> {
    let log = w.section("Log").unwrap_or_default();
    log.lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest.trim()))
        .filter_map(|rest| rest.strip_prefix(FOLLOWUP))
        .filter_map(|rest| {
            let (head, tail) = rest.split_once(':')?;
            let row = head.split_whitespace().next()?.to_string();
            let ids = tail.split(" · ").next()?.split_whitespace().map(str::to_string).collect();
            Some((row, ids))
        })
        .collect()
}

/// The check row whose follow-ups a group already holds, if any: the group's
/// one round, spent. Either the rows joined it, or it is made of them.
pub(crate) fn spent_by(w: &Worklist, members: &[String]) -> Option<String> {
    followups(w).into_iter().find(|(_, ids)| ids.iter().any(|i| members.contains(i))).map(|(row, _)| row)
}

/// A follow-up group's stop, built from its rows' done-whens: the barrier
/// after it reads each row against what that row said would finish it.
fn followup_stop(store: &Store, row: &str, at: usize, members: &[String]) -> String {
    let parts: Vec<String> = members
        .iter()
        .map(|m| {
            let t = store.find_task(m);
            let said = t.as_ref().and_then(done_when).or_else(|| t.map(|t| t.title)).unwrap_or_default();
            format!("{m}: {}", util::truncate(&said, 400))
        })
        .collect();
    format!("Follow-ups {row} added at group {at}'s barrier. Each is done when — {}", parts.join(" · "))
}

/// The paragraph of a task's overview that begins "Done when", without the
/// label: the line it starts on and every line after it up to a blank one.
/// `None` where the overview says no such thing, and the caller uses the title.
pub(crate) fn done_when(t: &crate::model::Task) -> Option<String> {
    let overview = t.section("Overview")?;
    let mut out = String::new();
    let mut on = false;
    for line in overview.lines() {
        let plain = line.trim().replace("**", "");
        if !on {
            // ASCII lowering keeps byte offsets, so the index is good on `plain`.
            let lower = plain.to_ascii_lowercase();
            let Some(i) = lower.find("done when").or_else(|| lower.find("done-when")) else { continue };
            on = true;
            let rest = plain[i + "done when".len()..].trim_start_matches(|c: char| c == ':' || c.is_whitespace());
            out.push_str(rest);
            continue;
        }
        if plain.is_empty() || plain.starts_with('#') {
            break;
        }
        out.push(' ');
        out.push_str(plain.trim_start_matches("- ").trim());
    }
    Some(fold(&out)).filter(|s| !s.is_empty())
}

// ---- park -------------------------------------------------------------

/// `wsp worklist park [<slug>] "why"` — a person's pause. `wsp-173`.
///
/// **Not `hold`, and the difference is the way back.** `hold` is a barrier's
/// "does not pass", and the verb that answers it is `go` — the pass. A pause
/// written as a hold can only be resumed by passing whatever barrier the list
/// was standing at, and on 2026-10-05 that is how wsp-process group 3 was
/// passed unchecked by a governor who meant to resume. So this has its own
/// status, its own reason in the log under its own mark, and its own way back
/// in [`resume`]; and `go` and `hold` both refuse a parked list.
///
/// **It records nothing on a group.** The position is derived from verdicts
/// and tasks, so a pause leaves the run standing exactly where it stood, and a
/// barrier owed before it is owed after it.
///
/// **It ends nothing**, which is where it parts from `hold` the second time.
/// `hold` ends what the run opened because the run is over at that barrier;
/// a pause is a run somebody means to come back to, and a member's agent ended
/// here is work its resume has to start again. What is in flight is said, and
/// left to land.
///
/// **A held list may be parked, and parking takes the hold back.** Every hold
/// written before this verb existed is a pause that could only be typed as a
/// verdict — native-window's own log says "a pause of the list, not a verdict
/// on its group 4 barrier" — so the honest reading is the one that errs toward
/// checking: resumed, it stands at its barrier with the check owed, and a
/// barrier that really did not pass is simply checked again.
pub fn park(store: &Store, args: &Args) -> i32 {
    let (mut w, said, seat) = match list_and_words(store, args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if said.trim().is_empty() {
        eprintln!("wsp: {} \"why\" — a pause with no reason on it is a list nobody knows when to resume", how("park", &w, seat));
        eprintln!("     `--from FILE` reads the sentence out of a file and `-` off a stream, where a shell never sees it");
        return 2;
    }
    match w.status() {
        WorklistStatus::Done => {
            eprintln!("wsp: `{}` is done — there is nothing left in it to pause", w.id);
            return 1;
        }
        WorklistStatus::Draft => {
            eprintln!("wsp: `{}` has not started — a draft starts nothing until `go`, so there is nothing to pause", w.id);
            return 1;
        }
        WorklistStatus::Parked => {
            println!("{} {}", w.id, Paint::new().dim("is already parked"));
            return 0;
        }
        WorklistStatus::Running | WorklistStatus::Held => {}
    }
    let was_held = w.status() == WorklistStatus::Held;
    let pos = worklist::position(store, &w, Reading::Landed);
    let flight = front(store, w.groups().get(pos.at.unwrap_or(1) - 1), &pos.members);

    w.set_status(WorklistStatus::Parked);
    w.log(&format!("{PARKED} {}", said.trim()));
    if was_held {
        w.log(&format!("the hold before this is taken back — {}", resumes_to(&pos)));
    }
    let groups = w.groups();
    let msg = format!("park {}", w.id);
    let code = save(store, &mut w, &groups, "park", &msg);
    if code != 0 {
        return code;
    }
    store.log_event("worklist-parked", json!({ "id": w.id, "why": said, "was": if was_held { "held" } else { "running" } }));
    crate::cycle::poke_list(store, &w.id, "park", None);

    if args.json() {
        println!(
            "{}",
            json!({
                "worklist": w.id,
                "status": w.status().as_str(),
                "why": said,
                "at": pos.at,
                "barrier_owed": pos.at.is_some() && pos.at_barrier(),
                "took_back_hold": was_held,
                "in_flight": flight.waiting.iter().map(|(s, _)| s.id.clone()).collect::<Vec<_>>(),
            })
        );
        return 0;
    }
    let p = Paint::new();
    println!("{} {}", p.bold(&w.id), p.dim("parked — nothing starts, and no barrier is checked"));
    if was_held {
        println!("{}", p.dim("it was held: the hold is taken back, and no verdict was recorded either way"));
    }
    if !flight.waiting.is_empty() {
        println!("{}", p.dim(&format!("{} still in flight and left to land — a pause ends nothing", flight.waiting.len())));
        for (s, note) in &flight.waiting {
            println!("  {}  {}  {}", s.id, util::pad(s.settlement.word(), 7), p.dim(note));
        }
    }
    println!("{}", p.dim(&format!("{}  {}", how("resume", &w, seat), resumes_to(&pos))));
    0
}

/// Where a resume lands, said before anybody runs it — on the pause, on `next`,
/// and in the log — because the one thing a resume must not look like is a pass.
fn resumes_to(pos: &Position) -> String {
    match pos.at {
        None => "returns it with every group finished".to_string(),
        Some(at) if pos.at_barrier() => format!("returns it to group {at}'s barrier, still owed"),
        Some(at) => format!("returns it to group {at}"),
    }
}

/// `wsp worklist resume [<slug>] ["…"]` — the pause taken back, and nothing else.
///
/// **`running` again, and that is the whole of the write.** No verdict, no
/// sweep, no barrier crossed: the position was never moved, so the gate the
/// list stood at before the pause is the gate it stands at now, and if it was
/// a barrier the check is owed exactly as it was. The step that follows is the
/// cycle's ordinary one, which opens that check if nothing has. A sentence is
/// optional — the reason worth having was written at the pause.
pub fn resume(store: &Store, args: &Args) -> i32 {
    let (mut w, said, seat) = match list_and_words(store, args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    match w.status() {
        WorklistStatus::Parked => {}
        WorklistStatus::Running => {
            println!("{} {}", w.id, Paint::new().dim("is not parked — it is running"));
            return 0;
        }
        // Refused by name, because this is the confusion the verb exists to
        // end from the other side: a hold is a barrier's, and only `go` — the
        // pass — answers it.
        WorklistStatus::Held => {
            eprintln!("wsp: `{}` is held at a barrier, not parked — `go` passes it, `done` closes it", w.id);
            return 1;
        }
        WorklistStatus::Draft => {
            eprintln!("wsp: `{}` has not started — {} \"…\" starts it", w.id, how("go", &w, seat));
            return 1;
        }
        WorklistStatus::Done => {
            eprintln!("wsp: `{}` is done — nothing in it is waiting to start", w.id);
            return 1;
        }
    }
    let pos = worklist::position(store, &w, Reading::Landed);
    // Asked again here because a parked list is not running, so while it was
    // paused another list could have started with one of its members in it.
    if let Some(code) = only_one_running(store, &w, &pos) {
        return code;
    }

    w.set_status(WorklistStatus::Running);
    let entry = format!("resumed · {}", said.trim());
    w.log(entry.trim_end_matches(" · ").trim_end());
    let groups = w.groups();
    let msg = format!("resume {}", w.id);
    let code = save(store, &mut w, &groups, "resume", &msg);
    if code != 0 {
        return code;
    }
    store.log_event("worklist-resumed", json!({ "id": w.id, "at": pos.at, "said": said }));
    crate::cycle::poke_list(store, &w.id, "resume", None);

    let st = state(store, &w, &pos);
    let gone = worklist::dangling(store, &w);
    if args.json() {
        let mut out = json!({ "worklist": w.id, "status": w.status().as_str(), "at": pos.at });
        out["next"] = next_json(&w, &pos, &st, &gone, None);
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }
    let p = Paint::new();
    println!("{} {}", p.bold(&w.id), p.dim(&format!("resumed — {}", resumes_to(&pos).trim_start_matches("returns it "))));
    report(&w, &pos, &st, &gone, seat, None);
    0
}

// ---- done -------------------------------------------------------------

/// `wsp worklist done <slug>` — there is nothing left to want from this list.
///
/// The slug is required and is not optional the way `next`, `go` and `hold`
/// take it from the seat. This is the one verb here that is final, it is run
/// once at the end of a night, and typing the name is the whole of the
/// confirmation it needs.
///
/// It does not check that the list finished. `done` is *somebody's decision*
/// that there is nothing left to want, and a run abandoned two groups from the
/// end is a real and ordinary thing to be finished with — but it says what it
/// is closing over, because a plan closed with work still open in it is worth
/// one line at the moment it happens rather than a surprise in `worklist ls`.
pub fn done(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp worklist done <slug>");
        eprintln!("       (named rather than taken from the seat: this one is final)");
        return 2;
    };
    let w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    close(store, args, w, crate::cycle::Closing::Done)
}

/// [`done`] once the list is found, and the whole of it: the record, and
/// everything the run opened ended with it.
///
/// **Split out for `wsp-208`**: a run wsp takes past its last barrier is
/// closed by wsp, with [`crate::cycle::Closing::Passed`] — the same closing a
/// person types, except that the check whose `go` this is is left to finish.
/// A hand verb at the end of an unattended run was the defect, and a second
/// copy of what `done` writes would be one that drifts from it.
pub(crate) fn close(store: &Store, args: &Args, mut w: Worklist, how: crate::cycle::Closing) -> i32 {
    if w.status() == WorklistStatus::Done {
        println!("{} {}", w.id, Paint::new().dim("is already done"));
        return 0;
    }
    let pos = worklist::position(store, &w, Reading::Settled);
    let left: Vec<String> = pos.holding().iter().map(|s| s.id.clone()).collect();
    // The other half of what is being closed over, and the half that is easy to
    // close over without noticing: a member of a group this run already passed
    // that does not read as finished. It is not "still open" — nothing here is
    // waiting on it and no barrier will ever mention it again — which is
    // exactly why the decision is the last moment it can be written down.
    let behind: Vec<String> = pos.slipped.iter().map(|s| s.id.clone()).collect();
    // And the third thing being closed over: a barrier somebody crossed
    // without writing at it. Nothing below this line will ever ask about it
    // again, so the decision is its last chance to be on the record.
    let unwritten: Vec<String> = pos.unwritten.iter().map(|n| n.to_string()).collect();
    let mut tail = String::new();
    if !behind.is_empty() {
        tail.push_str(&format!(" · behind {}", behind.join(" ")));
    }
    if !unwritten.is_empty() {
        tail.push_str(&format!(" · unwritten {}", unwritten.join(" ")));
    }

    w.set_status(WorklistStatus::Done);
    w.log(&match pos.at {
        None => format!("done — every barrier passed{tail}"),
        // Nothing open in the group and the run still standing at it: what was
        // being closed over is the barrier itself, and saying so is the point
        // of writing this line at the moment the decision is taken. The old
        // form ended on a dash with nothing after it.
        Some(at) if left.is_empty() => {
            format!("done at group {at} of {} — its barrier was never passed{tail}", pos.of)
        }
        Some(at) => format!("done at group {at} of {} — {}{tail}", pos.of, left.join(" ")),
    });
    let groups = w.groups();
    let msg = format!("done {}", w.id);
    let code = save(store, &mut w, &groups, "done", &msg);
    if code != 0 {
        return code;
    }
    store.log_event(
        "worklist-done",
        json!({ "id": w.id, "at": pos.at, "open": left, "behind": behind, "unwritten": pos.unwritten }),
    );
    // `wsp-158`: a `done` is a barrier closing like any other, so everything the
    // run opened goes with it. Said rather than done quietly, because `done` can
    // be typed on a list with work still open — that is its documented meaning —
    // and ending a member's agent on a list somebody meant to come back to is
    // not something to do without saying.
    crate::cycle::end_what_the_run_opened(store, &crate::cycle::Fleet, &w.id, pos.at, how);

    if args.json() {
        println!(
            "{}",
            json!({ "worklist": w.id, "status": "done", "at": pos.at, "open": left, "behind": behind, "unwritten": pos.unwritten })
        );
        return 0;
    }
    let p = Paint::new();
    println!("{} {}", p.bold(&w.id), p.dim("done"));
    if !pos.finished() {
        println!(
            "{}",
            p.dim(&format!(
                "closed at group {} of {} · {}",
                pos.at.unwrap_or(0),
                pos.of,
                match left.is_empty() {
                    // The work of that group was over and its barrier was not
                    // crossed, which is the closing this line exists to record:
                    // "nothing still open in it" said the opposite of what
                    // happened.
                    true => "its barrier was never passed".to_string(),
                    false => format!("{} still open in it", left.join(" ")),
                }
            ))
        );
    }
    for line in behind_lines(&p, &pos.slipped, &p.bold("behind"), 8) {
        println!("{line}");
    }
    for line in unwritten_lines(&p, &pos.unwritten, &p.bold("unwritten"), 8) {
        println!("{line}");
    }
    0
}

// ---- edit -------------------------------------------------------------

/// `wsp worklist edit <slug> --overview -` — the prose around the queue.
///
/// **Built, and the reason is the barrier rather than convenience.** Per
/// `wsp-092`, a start condition on group N is stop prose on group N−1, except
/// for group 1, whose start condition is the worklist's own `## Overview` —
/// and nothing wrote that section, which `worklist-004` found and correctly
/// named rather than inventing a verb outside its brief. The gap matters more
/// than it sounds: **the first group's start condition is the one nobody is
/// made to read, because there is no barrier in front of it.**
///
/// It is the barrier's to close. [`Gate::Start`] makes the overview the prose
/// read at barrier zero, on exactly the machinery every other barrier uses, so
/// a list that says something about starting itself now refuses to start until
/// somebody has answered it. A section nothing can write would have made that
/// barrier permanently empty, which is a verb missing from the one place a
/// missing verb is load-bearing.
///
/// `## Groups` is deliberately not in [`WORKLIST_PROSE`]: the queue has verbs
/// of its own and a window they may edit it in, and an editor that could
/// rewrite it would be a way around both. What this reaches is the prose the
/// structure sits in.
pub fn edit(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp worklist edit <slug> [--overview | --decisions] [-]");
        return 2;
    };
    let w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    crate::cmd_task::edit_prose(
        store,
        args,
        crate::cmd_task::Prose {
            what: "worklist",
            id: w.id.clone(),
            body: w.body.clone(),
            path: store.worklist_path(&w.id),
            sections: &crate::model::WORKLIST_PROSE,
        },
    )
}

/// One worklist, for `--json`: what it is, and where it is up to.
fn worklist_json(store: &Store, w: &Worklist) -> serde_json::Value {
    worklist_json_at(w, &worklist::position(store, w, Reading::Settled))
}

/// The same object where the position is already in hand.
///
/// Split out for `ls`, which reads a position per list through
/// `worklist::listing` and must not ask for a second one — two walks of the
/// same groups is how a `--json` row comes to disagree with the table printed
/// beside it, and it is the position that both of them are about.
fn worklist_json_at(w: &Worklist, at: &Position) -> serde_json::Value {
    json!({
        "id": w.id,
        "title": w.title,
        "status": w.status().as_str(),
        "created": w.created,
        "groups": at.of,
        "at": at.at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;
    use std::path::PathBuf;

    /// A store of its own, with no environment touched: nothing in these verbs
    /// reads the ambient store, so the tests can run beside each other.
    fn scratch(tag: &str) -> Store {
        let root: PathBuf =
            std::env::temp_dir().join(format!("wsp-wl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    fn task(store: &Store, id: &str, status: &str) {
        let mut t = Task::new(id, id);
        t.status_raw = status.into();
        store.save_task(&t).unwrap();
    }

    fn run(store: &Store, argv: &[&str]) -> i32 {
        let rest: Vec<&str> = argv.to_vec();
        dispatch(store, &Args::synth("worklist", &rest, &[]))
    }

    /// `wsp-134`, Ed: configured when the group is created, with a sensible
    /// default. Said, it is taken; unsaid, the group before is inherited, so a
    /// list on opencode stays on opencode; with nothing before, `claude`.
    #[test]
    fn a_new_group_is_given_a_policy_said_inherited_or_the_default() {
        let store = scratch("policy");
        for id in ["p-1", "p-2", "p-3"] {
            task(&store, id, "todo");
        }
        assert_eq!(run(&store, &["new", "pol", "Policy"]), 0);
        assert_eq!(run(&store, &["add", "pol", "p-1"]), 0);
        assert_eq!(flagged(&store, &["add", "pol", "p-2"], &[("agent", "opencode  m/x ")]), 0);
        assert_eq!(run(&store, &["add", "pol", "p-3"]), 0);
        let g = store.worklist("pol").unwrap().groups();
        assert_eq!(g[0].agent, DEFAULT_POLICY);
        assert_eq!(g[1].agent, "opencode m/x", "said, and tidied");
        assert_eq!(g[2].agent, "opencode m/x", "inherited from the group before");
        assert_eq!(flagged(&store, &["group", "pol", "3"], &[("agent", MANUAL)]), 0);
        assert!(store.worklist("pol").unwrap().groups()[2].policy().is_none(), "and turned off");
    }

    /// A list from before groups had a line stays run by hand when it grows,
    /// because the rules it runs under live in its prose, not here.
    #[test]
    fn a_group_added_to_a_list_from_before_policies_stays_manual() {
        let store = scratch("legacy");
        for id in ["l-1", "l-2"] {
            task(&store, id, "todo");
        }
        let mut w = Worklist::new("old", "Old");
        w.set_groups(&[Group { members: vec!["l-1".into()], ..Group::default() }]);
        store.save_worklist(&w).unwrap();
        assert_eq!(run(&store, &["add", "old", "l-2"]), 0);
        assert_eq!(store.worklist("old").unwrap().groups()[1].agent, "", "no line, as before");

        // And in front of all of them: a new first group has nothing before
        // it, and still is not handed `claude` on a list from before this.
        assert_eq!(flagged(&store, &["mv", "old", "l-2"], &[("after", "0")]), 0);
        let g = store.worklist("old").unwrap().groups();
        assert_eq!(g[0].members, vec!["l-2"], "it moved to the front");
        assert_eq!(g[0].agent, "", "and inherits manual from the list it joined");
    }

    fn flagged(store: &Store, argv: &[&str], flags: &[(&str, &str)]) -> i32 {
        dispatch(store, &Args::synth("worklist", argv, flags))
    }

    fn groups_of(store: &Store, id: &str) -> Vec<Group> {
        store.worklist(id).expect("the list").groups()
    }

    fn started(store: &Store, id: &str) {
        let mut w = store.worklist(id).expect("the list");
        w.set_status(WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
    }

    /// A barrier somebody passed, written the way `go` writes it.
    ///
    /// The position is the first group nobody has done this to, so a test that
    /// wants a run to be standing *at* group 2 has to say that a person took it
    /// past group 1. Members finishing does not do it and must not.
    fn crossed(store: &Store, id: &str, n: usize) {
        let mut w = store.worklist(id).expect("the list");
        let mut groups = w.groups();
        groups[n - 1].verdict = "2026-08-20T09:00:00Z passed".into();
        w.set_groups(&groups);
        store.save_worklist(&w).unwrap();
    }

    /// The shape the composing path actually takes: a list, then one call per
    /// group, in the order the groups run. Each call is a group of its own,
    /// because that is what "one group, then the next" means when you are
    /// typing it — and a member is stored under the id the store resolved it
    /// to, never the suffix somebody typed.
    #[test]
    fn each_add_is_a_group_and_a_member_is_stored_as_the_id_it_resolved_to() {
        let store = scratch("compose");
        for id in ["wl-001", "wl-002", "wl-003", "wl-004"] {
            task(&store, id, "todo");
        }
        assert_eq!(run(&store, &["new", "batch", "Overnight batch"]), 0);
        assert_eq!(run(&store, &["add", "batch", "001"]), 0);
        assert_eq!(run(&store, &["add", "batch", "wl-002", "wl-003"]), 0);

        let gs = groups_of(&store, "batch");
        assert_eq!(gs.len(), 2, "one call, one group");
        assert_eq!(gs[0].members, ["wl-001"], "a suffix is stored as the id it names");
        assert_eq!(gs[1].members, ["wl-002", "wl-003"], "in the order they were typed");

        assert_eq!(flagged(&store, &["add", "batch", "wl-004"], &[("group", "2")]), 0);
        assert_eq!(groups_of(&store, "batch")[1].members, ["wl-002", "wl-003", "wl-004"]);
    }

    /// The window is what a *run* imposes, and a draft has no run. A plan
    /// composed out of the backlog legitimately holds work already at `review`
    /// — design-only tasks reach it before anything is spawned — and refusing
    /// the edit because that group "is finished" would be a refusal on the
    /// reading pass the whole design exists to invite.
    #[test]
    fn a_draft_is_editable_all_the_way_down_however_its_members_stand() {
        let store = scratch("draft");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "done");
        task(&store, "wl-003", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);

        assert_eq!(run(&store, &["rm", "batch", "wl-001"]), 0, "nothing has started, so nothing is behind");
        assert_eq!(groups_of(&store, "batch").len(), 2, "and the emptied group went with it");
    }

    /// `worklist-051`. The promise every dry run in wsp makes: what `-n` says
    /// is what dropping it then does, asserted by doing exactly that.
    ///
    /// The consequence being previewed is the one nothing else prints. Removing
    /// the only member of group 2 drops the group, which **renumbers 3 into 2**
    /// — and a group number is what the reader has to type next, at `worklist
    /// mv --group N` and `worklist group <slug> N`. `wsp worklist show` can be
    /// read before and after and the renumbering worked out; it cannot be
    /// asked. So the preview names the group by the position it has *now*,
    /// which is the one on the screen the reader is looking at.
    ///
    /// And the store is untouched, which here is the whole verb: `rm` mutates a
    /// copy of the groups and saves once at the end, so the dry run is that
    /// function with the save skipped rather than a second account of it.
    #[test]
    fn what_a_dry_run_of_worklist_rm_says_is_what_the_removal_then_does() {
        let store = scratch("wl-foresee");
        for id in ["wl-001", "wl-002", "wl-003"] {
            task(&store, id, "todo");
        }
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);
        let before = groups_of(&store, "batch");
        let logged = store.worklist("batch").expect("the list").body.clone();

        assert_eq!(flagged(&store, &["rm", "batch", "wl-002"], &[("dry-run", "true")]), 0);
        assert_eq!(
            groups_of(&store, "batch").iter().map(|g| g.members.clone()).collect::<Vec<_>>(),
            before.iter().map(|g| g.members.clone()).collect::<Vec<_>>(),
            "a dry run took a member out"
        );
        assert_eq!(
            store.worklist("batch").expect("the list").body,
            logged,
            "a dry run wrote a line into the list's log"
        );

        // The same call without the word, and it has to agree.
        assert_eq!(run(&store, &["rm", "batch", "wl-002"]), 0);
        let after = groups_of(&store, "batch");
        assert_eq!(after.len(), 2, "the emptied group was not dropped");
        assert_eq!(after[1].members, ["wl-003"], "group 3 did not become group 2");
    }

    /// A dry run that reported a removal the verb would have refused would be
    /// lying in the one direction that costs work, so `-n` is read *after* both
    /// refusals rather than before them — the arrangement `checkout --rm -n`
    /// settled on, arriving here for the same reason.
    ///
    /// Both refusals: a member the list does not hold, and the frozen window.
    /// Neither is reachable by looking at the list, because the window is a
    /// fact about the *run* — where it is up to — rather than about the
    /// membership.
    #[test]
    fn a_dry_run_of_worklist_rm_refuses_everything_the_removal_would_refuse() {
        let store = scratch("wl-foresee-refuse");
        for id in ["wl-001", "wl-002", "wl-003"] {
            task(&store, id, "todo");
        }
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);
        started(&store, "batch");
        crossed(&store, "batch", 1);

        let dry = &[("dry-run", "true")];
        assert_eq!(flagged(&store, &["rm", "batch", "wl-001"], dry), 1, "behind the position: history");
        assert_eq!(flagged(&store, &["rm", "batch", "wl-002"], dry), 1, "at the position: the barrier");
        assert_eq!(flagged(&store, &["rm", "batch", "wl-404"], dry), 1, "not a member of this list");
        assert_eq!(flagged(&store, &["rm", "batch", "wl-003"], dry), 0, "ahead of the work, and open");
        assert_eq!(groups_of(&store, "batch").len(), 3, "a refused dry run moved something");
    }

    /// `wsp-150`: inside the group being run, a member's own line may change
    /// until that member starts — and only until. The run is a real one here:
    /// group 1 is wsp's, one member is at `doing` and another is claimed while
    /// still at `todo`, and the third has not been touched.
    #[test]
    fn a_member_not_yet_started_takes_a_new_agent_line_in_the_running_group_and_one_that_has_started_is_refused() {
        let (_env, store) = running("member-line");
        for (id, status) in [("ml-1", "doing"), ("ml-2", "todo"), ("ml-3", "todo"), ("ml-4", "todo")] {
            task(&store, id, status);
        }
        run(&store, &["new", "mix", "m"]);
        assert_eq!(flagged(&store, &["add", "mix", "ml-1", "ml-2", "ml-3"], &[("agent", "opencode m-free")]), 0);
        run(&store, &["add", "mix", "ml-4"]);
        started(&store, "mix");
        store.set_claim("ml-2", json!({ "workspace_id": "cpd-9" }));

        // The group's own line is frozen with its membership, as before.
        assert_eq!(flagged(&store, &["group", "mix", "1"], &[("agent", "claude")]), 1);

        assert_eq!(flagged(&store, &["member", "mix", "ml-3"], &[("agent", "claude opus high")]), 0, "not started: open");
        assert_eq!(groups_of(&store, "mix")[0].member_agents.get("ml-3").map(String::as_str), Some("claude opus high"));
        assert_eq!(groups_of(&store, "mix")[0].policy_for("ml-3").unwrap().kind, "claude", "and it is what the spawn reads");

        assert_eq!(flagged(&store, &["member", "mix", "ml-1"], &[("agent", "claude")]), 1, "doing: refused");
        assert_eq!(member_started(&store, "ml-1").as_deref(), Some("doing"), "and the reason names how it started");
        assert_eq!(flagged(&store, &["member", "mix", "ml-2"], &[("agent", "claude")]), 1, "claimed at todo: refused");
        assert_eq!(member_started(&store, "ml-2").as_deref(), Some("todo · claimed in cpd-9"));
        let g = &groups_of(&store, "mix")[0];
        assert!(!g.member_agents.contains_key("ml-1") && !g.member_agents.contains_key("ml-2"), "a refusal writes nothing");

        assert_eq!(flagged(&store, &["member", "mix", "ml-3"], &[("agent", "none")]), 0, "none puts it back on the group's");
        assert!(groups_of(&store, "mix")[0].member_agents.is_empty());
        assert_eq!(flagged(&store, &["member", "mix", "ml-3"], &[("agent", MANUAL)]), 2, "a member is not run by hand alone");

        // A group ahead of the work takes a member's line too, and so does
        // `add` joining a group: there it is the joining members' own line.
        assert_eq!(flagged(&store, &["member", "mix", "ml-4"], &[("agent", "claude")]), 0);
        task(&store, "ml-5", "todo");
        assert_eq!(flagged(&store, &["add", "mix", "ml-5"], &[("group", "2"), ("agent", "opencode m-free")]), 0);
        let g = &groups_of(&store, "mix")[1];
        assert_eq!(g.agent, "opencode m-free", "inherited by the new group");
        assert_eq!(g.member_agents.get("ml-4").map(String::as_str), Some("claude"));
        assert!(!g.member_agents.contains_key("ml-5"), "the group's own line said again is no override");

        // And it travels with the member.
        assert_eq!(flagged(&store, &["mv", "mix", "ml-4"], &[("after", "2")]), 0);
        assert_eq!(groups_of(&store, "mix")[2].member_agents.get("ml-4").map(String::as_str), Some("claude"));
    }

    /// The rule the whole verb set is built on: a group at or behind the
    /// position is frozen. Editing what has run rewrites history; editing what
    /// is running changes the membership of a barrier already being waited on,
    /// which is the `batch` handbook failure with the disagreement inside one
    /// record.
    #[test]
    fn a_running_list_refuses_every_edit_at_or_behind_where_it_is_up_to() {
        let store = scratch("frozen");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "doing");
        task(&store, "wl-003", "todo");
        task(&store, "wl-004", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);
        started(&store, "batch");
        crossed(&store, "batch", 1);

        // Group 1 has landed and its barrier was passed, group 2 is being
        // waited on, group 3 is ahead.
        assert_eq!(run(&store, &["rm", "batch", "wl-001"]), 1, "behind the position: history");
        assert_eq!(run(&store, &["rm", "batch", "wl-002"]), 1, "at the position: the barrier");
        assert_eq!(
            flagged(&store, &["add", "batch", "wl-004"], &[("group", "2")]),
            1,
            "and adding to it is the same edit from the other side"
        );
        assert_eq!(
            flagged(&store, &["group", "batch", "2"], &[("parallel", "2")]),
            1,
            "a cap on the group being run is a change to what is in flight"
        );

        assert_eq!(run(&store, &["rm", "batch", "wl-003"]), 0, "ahead of the work, and open");
        assert_eq!(
            run(&store, &["add", "batch", "wl-004"]),
            0,
            "and a new group at the end can never disagree with anything"
        );
    }

    /// `wsp-206`: a verifier found the live group's stop asserting something
    /// false, and the only place a correction could go was a decision on one
    /// member that the barrier agent was trusted to find. The stop is the
    /// one field of the running group nothing has read as a barrier yet, so it
    /// may be corrected. The reason is required, because the edit changes a
    /// running plan, and it goes in the list's log.
    #[test]
    fn the_running_groups_stop_is_corrected_with_a_logged_reason_and_nothing_else_of_it_is() {
        let store = scratch("amend-stop");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "doing");
        task(&store, "wl-003", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);
        flagged(&store, &["group", "batch", "2"], &[("stop", "sonnet is priced 3/15")]);
        started(&store, "batch");
        crossed(&store, "batch", 1);

        assert_eq!(
            flagged(&store, &["group", "batch", "2"], &[("stop", "sonnet stays at 2/10")]),
            2,
            "a running plan is not changed without saying why"
        );
        assert_eq!(groups_of(&store, "batch")[1].stop, "sonnet is priced 3/15");

        let why = "the vendor cancelled the step";
        assert_eq!(
            flagged(&store, &["group", "batch", "2"], &[("stop", "sonnet stays at 2/10"), ("why", why)]),
            0
        );
        let w = store.worklist("batch").unwrap();
        assert_eq!(w.groups()[1].stop, "sonnet stays at 2/10", "the barrier is handed the correction");
        let log = w.section("Log").unwrap_or_default();
        assert!(log.contains(why) && log.contains("amended while it runs"), "and the reason is on the record: {log}");

        assert_eq!(
            flagged(&store, &["group", "batch", "2"], &[("parallel", "2"), ("why", why)]),
            1,
            "a cap is still what is in flight, reason or none"
        );
        assert_eq!(
            flagged(&store, &["group", "batch", "1"], &[("stop", "rewritten"), ("why", why)]),
            1,
            "a barrier already passed is history"
        );
    }

    /// The window closes when the barrier row opens, because the barrier
    /// agent's work order is composed with the stop at that moment. A `hold`
    /// settles the row, and the recheck is composed afresh, so the window
    /// opens again for it.
    #[test]
    fn the_running_groups_stop_is_refused_once_its_barrier_is_being_checked() {
        let store = scratch("amend-at-barrier");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "review");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        started(&store, "batch");
        crossed(&store, "batch", 1);

        let mut row = Task::new(&crate::cycle::barrier_title("batch", 2), "wl-900");
        row.tags = vec![crate::cycle::BARRIER_TAG.to_string()];
        row.status_raw = "doing".into();
        store.save_task(&row).unwrap();

        let amend = [("stop", "corrected"), ("why", "it was wrong")];
        assert_eq!(flagged(&store, &["group", "batch", "2"], &amend), 1, "its agent was handed the stop as it was");
        assert_eq!(groups_of(&store, "batch")[1].stop, "");

        row.status_raw = "review".into();
        store.save_task(&row).unwrap();
        assert_eq!(flagged(&store, &["group", "batch", "2"], &amend), 0, "held: the recheck reads it afresh");
        assert_eq!(groups_of(&store, "batch")[1].stop, "corrected");
    }

    // ---- followup (`wsp-210`) ----

    /// A run standing at group `at`'s barrier, with its check open on `row`.
    fn checking(store: &Store, list: &str, at: usize, row: &str) {
        let mut t = Task::new(&crate::cycle::barrier_title(list, at), row);
        t.tags = vec![crate::cycle::BARRIER_TAG.to_string()];
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();
    }

    /// A verdict in a file, the way a check writes one.
    fn verdict(tag: &str, text: &str) -> String {
        let path = std::env::temp_dir().join(format!("wsp-wl-verdict-{tag}-{}", std::process::id()));
        std::fs::write(&path, text).unwrap();
        path.display().to_string()
    }

    fn follow(store: &Store, argv: &[&str], flags: &[(&str, &str)], here: &[&str]) -> i32 {
        let here: Vec<String> = here.iter().map(|s| s.to_string()).collect();
        followup_by(store, &Args::synth("worklist", argv, flags), &here)
    }

    fn told() -> Vec<String> {
        crate::cycle::tests::TOLD.with(|t| t.borrow().clone())
    }

    /// A row with a done-when, as `wsp add --parent` files one.
    fn row_with(store: &Store, id: &str, overview: &str) {
        let mut t = Task::new(id, id);
        t.status_raw = "todo".into();
        crate::model::set_section_in(&mut t.body, "Overview", overview);
        store.save_task(&t).unwrap();
    }

    /// `--next`: the check passes the barrier, and the rows it found are the
    /// group straight after, **ahead of the group already planned**, never at
    /// the end. The new group runs on the line of the group that found it, and
    /// its barrier reads each row against that row's own done-when.
    #[test]
    fn a_non_blocking_follow_up_becomes_the_next_group_ahead_of_a_planned_one() {
        let store = scratch("followup-next");
        task(&store, "wl-001", "review");
        task(&store, "wl-003", "todo");
        row_with(&store, "wl-002", "Fix the thing.\n\n**Done when:** it prints hello\n- and exits 0\n\nLater prose.");
        run(&store, &["new", "run", "r"]);
        flagged(&store, &["add", "run", "wl-001"], &[("agent", "claude opus high")]);
        flagged(&store, &["add", "run", "wl-003"], &[("agent", "opencode m/x")]);
        started(&store, "run");
        checking(&store, "run", 1, "wl-900");

        let from = verdict("next", "group 1 holds; one small fix is owed after it");
        assert_eq!(follow(&store, &["followup", "run", "wl-002"], &[("next", "true"), ("from", &from)], &["wl-900"]), 0);

        let g = groups_of(&store, "run");
        assert_eq!(g.len(), 3);
        assert_eq!(g[1].members, vec!["wl-002"], "straight after the group that found it");
        assert_eq!(g[2].members, vec!["wl-003"], "and the planned group behind it");
        assert_eq!(g[1].agent, "claude opus high", "on the line of the group that found it, not the planned one");
        assert!(g[1].stop.contains("wl-002: it prints hello and exits 0"), "its stop is the row's done-when: {}", g[1].stop);
        assert!(!g[1].stop.contains("Later prose"), "and only the done-when: {}", g[1].stop);
        assert!(
            g[0].verdict.contains("passes, with follow-ups wl-002") && g[0].verdict.contains("one small fix is owed"),
            "the barrier is passed, and its verdict says how: {}",
            g[0].verdict
        );
        let log = store.worklist("run").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("follow-up by wl-900 passes: wl-002"), "the row that added it is on the record: {log}");
        let pos = worklist::position(&store, &store.worklist("run").unwrap(), Reading::Settled);
        assert_eq!(pos.at, Some(2), "and the run stands at the follow-up group");
    }

    /// One round. A follow-up group's own barrier finds more: nothing is
    /// attached, the rows and the verdict reach the governor as a decision,
    /// and the check is told it still ends with `go` or `hold`.
    #[test]
    fn a_follow_up_groups_barrier_is_refused_more_and_its_findings_reach_the_governor() {
        let store = scratch("followup-round");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        task(&store, "wl-004", "todo");
        run(&store, &["new", "run", "r"]);
        run(&store, &["add", "run", "wl-001"]);
        started(&store, "run");
        checking(&store, "run", 1, "wl-900");
        let from = verdict("round", "holds, with a tail");
        assert_eq!(follow(&store, &["followup", "run", "wl-002"], &[("next", "true"), ("from", &from)], &["wl-900"]), 0);

        let mut t = store.find_task("wl-002").unwrap();
        t.status_raw = "review".into();
        store.save_task(&t).unwrap();
        checking(&store, "run", 2, "wl-901");
        let from = verdict("round-2", "the follow-up holds, and found one more");
        for mode in ["next", "blocking"] {
            assert_eq!(
                follow(&store, &["followup", "run", "wl-004"], &[(mode, "true"), ("from", &from)], &["wl-901"]),
                1,
                "--{mode}: a follow-up group's barrier adds none of its own"
            );
        }
        let g = groups_of(&store, "run");
        assert_eq!(g.len(), 2, "nothing was attached");
        assert!(g.iter().all(|g| !g.members.contains(&"wl-004".to_string())));
        assert!(g[1].verdict.is_empty(), "nor was the barrier passed: the check still owes go or hold");
        let said = told();
        assert!(
            said.iter().any(|s| s.contains("A decision for you") && s.contains("wl-004") && s.contains("found one more")),
            "the governor is handed the rows and the verdict: {said:?}"
        );
        let log = store.worklist("run").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("follow-ups refused to wl-901: wl-004"), "{log}");
        assert_eq!(followups(&store.worklist("run").unwrap()).len(), 1, "and a refused round is not read as a round taken");
    }

    #[test]
    fn a_fifth_follow_up_is_refused() {
        let store = scratch("followup-five");
        task(&store, "wl-001", "review");
        for id in ["wl-011", "wl-012", "wl-013", "wl-014", "wl-015"] {
            task(&store, id, "todo");
        }
        run(&store, &["new", "run", "r"]);
        run(&store, &["add", "run", "wl-001"]);
        started(&store, "run");
        checking(&store, "run", 1, "wl-900");
        let from = verdict("five", "holds pending fixes");
        let five = ["followup", "run", "wl-011", "wl-012", "wl-013", "wl-014", "wl-015"];
        assert_eq!(follow(&store, &five, &[("blocking", "true"), ("from", &from)], &["wl-900"]), 2, "five is a plan, not a tail");
        assert_eq!(groups_of(&store, "run")[0].members, vec!["wl-001"], "and nothing joined");
        assert_eq!(follow(&store, &five[..6], &[("blocking", "true"), ("from", &from)], &["wl-900"]), 0, "four is a tail");
        assert_eq!(groups_of(&store, "run")[0].members.len(), 5);
    }

    /// Only the check, during its own barrier. A member, a verifier and a
    /// person at the CLI hold no open check row of the group being run, and a
    /// check that has already reviewed its row has given its verdict.
    #[test]
    fn a_caller_that_is_not_the_barrier_check_is_refused() {
        let store = scratch("followup-who");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "run", "r"]);
        run(&store, &["add", "run", "wl-001"]);
        started(&store, "run");
        let from = verdict("who", "holds");
        let argv = ["followup", "run", "wl-002"];
        let flags = [("blocking", "true"), ("from", from.as_str())];

        assert_eq!(follow(&store, &argv, &flags, &["wl-900"]), 1, "no barrier is being checked at all");
        checking(&store, "run", 1, "wl-900");
        assert_eq!(follow(&store, &argv, &flags, &[]), 1, "a person at the CLI holds no row");
        assert_eq!(follow(&store, &argv, &flags, &["wl-001"]), 1, "a member, or its verifier, holds the member");
        let mut t = store.find_task("wl-900").unwrap();
        t.status_raw = "review".into();
        store.save_task(&t).unwrap();
        assert_eq!(follow(&store, &argv, &flags, &["wl-900"]), 1, "a check that has reviewed has given its verdict");
        assert_eq!(groups_of(&store, "run")[0].members, vec!["wl-001"], "and nothing joined");
        assert!(told().is_empty(), "nor was anybody told anything");
    }

    #[test]
    fn a_done_when_is_read_off_the_overview_and_a_row_without_one_has_none() {
        let mut t = Task::new("t", "t-1");
        crate::model::set_section_in(&mut t.body, "Overview", "Intro.\n\n**Done when:** tests cover:\n- one\n- two\n\nLand it.");
        assert_eq!(done_when(&t).as_deref(), Some("tests cover: one two"));
        crate::model::set_section_in(&mut t.body, "Overview", "Nothing said about it.");
        assert_eq!(done_when(&t), None);
    }

    /// The window **at** a barrier, which is the moment the next group is
    /// composed.
    ///
    /// A group whose barrier nobody has passed is where the run is standing,
    /// and the group after it has not started: nothing is spawned for it,
    /// because `next` will not name it until somebody writes a verdict. So it
    /// is open — and that is the point rather than a leniency. The barrier is
    /// exactly when the same-file report arrives and the next group gets
    /// composed, and `go` re-checks the at-most-one-running rule at every
    /// barrier precisely because a group ahead of the work is hand-editable
    /// between two of them.
    ///
    /// It used to be shut, because the position had already walked past the
    /// unread barrier onto that group and frozen it there.
    #[test]
    fn the_group_after_a_standing_barrier_is_open_because_nothing_has_started_in_it() {
        let store = scratch("barrier-window");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        task(&store, "wl-003", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        started(&store, "batch");

        let w = store.worklist("batch").unwrap();
        let win = window(&store, &w);
        assert_eq!(win.at, Some(1), "group 1 is finished and nobody has passed its barrier");
        assert_eq!(win.first_open(), 2, "so group 2 — which nothing has run for — is editable");
        assert!(!win.allows(1), "and group 1, whose barrier is being read, is not");
        assert_eq!(
            flagged(&store, &["add", "batch", "wl-003"], &[("group", "2")]),
            0,
            "composing the next group is what a governor does standing at a barrier"
        );

        // Once the barrier is behind the run, group 2 is what the run is at and
        // shuts on the ordinary rule.
        crossed(&store, "batch", 1);
        let w = store.worklist("batch").unwrap();
        assert_eq!(window(&store, &w).first_open(), 3, "the verdict is what closes it");
    }

    /// A refusal that only says no sends somebody to edit the file by hand,
    /// which is the failure the window exists to prevent, arrived at from the
    /// other end. So it names the group, what is holding it, and what is open.
    #[test]
    fn a_refusal_names_the_group_being_run_and_what_may_be_edited_instead() {
        let store = scratch("refusal");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "doing");
        task(&store, "wl-003", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);
        started(&store, "batch");
        crossed(&store, "batch", 1);

        let w = store.worklist("batch").unwrap();
        let win = window(&store, &w);
        assert_eq!(win.first_open(), 3, "the position is 2, so 3 is the first open group");
        assert!(!win.allows(1) && !win.allows(2) && win.allows(3));
        assert_eq!(
            win.members.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            ["wl-002"],
            "the refusal has what is holding it already in hand, and asks the store no second time"
        );
    }

    /// **`worklist-047`.** The house rule printed in every brief is *give a
    /// wsp verb its prose through a file*, and this field took a quoted string
    /// or a stream and nothing else — so the documented spelling meant `cat`ing
    /// the file into a pipe in order to name it, and that friction is what ends
    /// with the sentence going between double quotes after all. A stop
    /// condition is the case that makes it worst: it is composed *ahead* of the
    /// run, in a file, by whoever is composing the list.
    ///
    /// Three readings are asserted and they are one token apart. `--from` after
    /// `--stop` is the file. `--from` with no `--stop` names no field on a verb
    /// that sets two, and is answered with the shape. `--from` beside a typed
    /// sentence is two stop conditions for one barrier, and choosing between
    /// them is the silent loss in a smaller hat.
    #[test]
    fn a_stop_condition_comes_out_of_the_file_from_names() {
        let store = scratch("stopfile");
        task(&store, "wl-001", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);

        let file = store.root.join("g1.md");
        let path = file.to_str().expect("a utf-8 temp dir").to_string();
        std::fs::write(&file, "If any of the three goes badly,\n\nflag and stop rather\nthan push through.\n")
            .unwrap();

        assert_eq!(flagged(&store, &["group", "batch", "1"], &[("stop", "true"), ("from", &path)]), 0);
        assert_eq!(
            groups_of(&store, "batch")[0].stop,
            "If any of the three goes badly, flag and stop rather than push through.",
            "a `stop:` block is one line, and the paragraph was not folded onto it"
        );

        assert_eq!(
            flagged(&store, &["group", "batch", "1"], &[("from", &path)]),
            2,
            "a --from naming no field, on a verb that sets two"
        );
        assert_eq!(
            flagged(&store, &["group", "batch", "1"], &[("stop", "typed"), ("from", &path)]),
            2,
            "two stop conditions for one barrier"
        );

        // An empty file reads exactly like `--stop none`, and the two want
        // different things done about them: one is a caller taking the
        // condition off, the other is a caller whose sentence went nowhere.
        std::fs::write(&file, "   \n\n").unwrap();
        assert_eq!(flagged(&store, &["group", "batch", "1"], &[("stop", "true"), ("from", &path)]), 2);
        assert!(
            groups_of(&store, "batch")[0].stop.starts_with("If any"),
            "a refused read took the condition off anyway"
        );
    }

    /// The check every other typed intake in the CLI makes — `add`, `rename`,
    /// `say`, `flag`'s row text, `project set`, `note` and its neighbours — and
    /// that these two did not. It is what catches the substitution the caller
    /// cannot see: the backticks a stop condition is written in ran before wsp
    /// was handed anything, and what arrives is fluent.
    ///
    /// The carriage return is the case that makes the ordering matter. It is
    /// whitespace, so `fold` destroys the evidence, which is why the check is
    /// made on the text before anything trims or folds it.
    #[test]
    fn a_stop_condition_that_is_captured_terminal_output_is_refused() {
        let store = scratch("stopctl");
        task(&store, "wl-001", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        assert_eq!(
            flagged(&store, &["group", "batch", "1"], &[("stop", "alone 87/745  \ralone 88/745")]),
            2
        );
        assert_eq!(groups_of(&store, "batch")[0].stop, "", "and it was not stored");
    }

    /// `--sub` is a resolution, not a rule. It puts in what was open when it
    /// was typed, and a sub-task filed under that parent tomorrow is not in the
    /// list — a group that grows under a governor at 3am is the stale-plan
    /// failure inverted.
    #[test]
    fn sub_resolves_when_it_is_typed_and_takes_only_what_is_open() {
        let store = scratch("sub");
        task(&store, "wl-010", "todo");
        for (id, status) in [("wl-011", "todo"), ("wl-012", "done"), ("wl-013", "review")] {
            let mut t = Task::new(id, id);
            t.status_raw = status.into();
            t.parent = Some("wl-010".into());
            store.save_task(&t).unwrap();
        }
        run(&store, &["new", "fork", "f"]);
        assert_eq!(flagged(&store, &["add", "fork", "wl-010"], &[("sub", "true")]), 0);

        let gs = groups_of(&store, "fork");
        assert_eq!(gs.len(), 1, "its sub-tasks are one group");
        assert_eq!(gs[0].members, ["wl-011", "wl-013"], "the open ones, and `done` is not one");

        // Filed after the fact, and deliberately not picked up.
        let mut later = Task::new("wl-014", "wl-014");
        later.parent = Some("wl-010".into());
        store.save_task(&later).unwrap();
        assert_eq!(groups_of(&store, "fork")[0].members.len(), 2, "resolved then, not live");
    }

    /// A task in two groups of one list holds two barriers, and the second
    /// could never open on work that landed for the first.
    #[test]
    fn a_task_cannot_be_in_two_groups_of_the_same_list() {
        let store = scratch("dup");
        task(&store, "wl-001", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        assert_eq!(run(&store, &["add", "batch", "wl-001"]), 1);
        assert_eq!(groups_of(&store, "batch").len(), 1, "and nothing was written");
    }

    /// Moving the only member of a group leaves a numbered blank that every
    /// reading of the queue would count as a group, and that a barrier would
    /// pass on the first look. It goes — and the destination it was named
    /// against moves up with it.
    #[test]
    fn a_group_emptied_by_a_move_is_dropped_and_the_destination_follows() {
        let store = scratch("mv");
        for id in ["wl-001", "wl-002", "wl-003"] {
            task(&store, id, "todo");
        }
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);

        assert_eq!(flagged(&store, &["mv", "batch", "wl-002"], &[("group", "3")]), 0);
        let gs = groups_of(&store, "batch");
        assert_eq!(gs.len(), 2, "the group it left was emptied");
        assert_eq!(gs[1].members, ["wl-003", "wl-002"], "and it landed in the one that was group 3");
    }

    /// `--after` is how a group is made between two that exist, which is the
    /// edit a plan wants when a piece of work turns out to need staging.
    #[test]
    fn after_makes_a_group_where_group_joins_one() {
        let store = scratch("after");
        for id in ["wl-001", "wl-002", "wl-003"] {
            task(&store, id, "todo");
        }
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);

        assert_eq!(flagged(&store, &["mv", "batch", "wl-002"], &[("after", "1")]), 0);
        let gs = groups_of(&store, "batch");
        assert_eq!(gs.len(), 3);
        assert_eq!(gs[0].members, ["wl-001"]);
        assert_eq!(gs[1].members, ["wl-002"], "a group of its own, between the two");
        assert_eq!(gs[2].members, ["wl-003"]);
    }

    /// The prose survives the file it is written into, wrapped and read back as
    /// the one paragraph it was — `## Groups` is parsed line by line, so this
    /// is the round trip that matters.
    #[test]
    fn a_stop_condition_and_a_cap_round_trip_through_the_record() {
        let store = scratch("stop");
        task(&store, "wl-001", "todo");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001", "wl-002"]);

        let prose = "if any of the three goes badly, flag and stop rather than push through — \
                     the whole night's spawning depends on it landing clean";
        assert_eq!(flagged(&store, &["group", "batch", "1"], &[("stop", prose), ("parallel", "2")]), 0);

        let g = groups_of(&store, "batch").remove(0);
        assert_eq!(g.cap, Some(2));
        assert_eq!(g.stop, prose, "wrapped on the way out and joined on the way back");

        assert_eq!(flagged(&store, &["group", "batch", "1"], &[("parallel", "none")]), 0);
        assert_eq!(groups_of(&store, "batch")[0].cap, None, "and a cap comes off again");
        assert_eq!(
            flagged(&store, &["group", "batch", "1"], &[("parallel", "0")]),
            2,
            "x0 has two readings, so it has none"
        );
    }

    /// One key space, because a governor seat is keyed on it: a hand raised at
    /// 3am must not route to whichever of a project and a worklist the map
    /// happened to hold.
    #[test]
    fn a_worklist_cannot_take_a_name_a_project_already_answers_to() {
        let store = scratch("scope");
        store.save_project(&crate::model::Project::new("render")).unwrap();
        assert_eq!(run(&store, &["new", "render", "clash"]), 1);
        assert!(store.worklist("render").is_none(), "and nothing was written");

        assert_eq!(run(&store, &["new", "Overnight Batch", "b"]), 0);
        assert!(store.worklist("overnight-batch").is_some(), "slugified, as a project id is");
        assert_eq!(run(&store, &["new", "overnight-batch", "again"]), 1, "and it holds its own name");
    }

    /// An archived task is the one member nothing else here may touch: the
    /// record keeps saying the id it was given, and `rm` is how a person takes
    /// it out. Resolving through the store first would have made it unremovable
    /// at exactly the moment somebody needs to remove it.
    #[test]
    fn a_member_the_store_has_forgotten_can_still_be_removed_by_hand() {
        let store = scratch("dangling");
        task(&store, "wl-001", "todo");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001", "wl-002"]);
        std::fs::remove_file(store.task_path("wl-002")).unwrap();

        assert_eq!(worklist::dangling(&store, &store.worklist("batch").unwrap()), ["wl-002"]);
        assert_eq!(run(&store, &["rm", "batch", "wl-002"]), 0);
        assert_eq!(groups_of(&store, "batch")[0].members, ["wl-001"]);
    }
    /// A store of its own **and** an environment of its own, which the verbs
    /// above do not need and the verbs below do.
    ///
    /// `go` reaches `cmd_checkout::Occupied::now`, which asks herdr who is
    /// standing in a tree — and on a machine where herdr is answering, that is
    /// a test talking to whoever is working today. `util::isolated` points the
    /// socket at nothing and the store at a directory of its own; it holds an
    /// environment lock, so these run one at a time, which is the price of the
    /// running verbs touching the world at all.
    fn running(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("wlrun-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        (env, store)
    }

    /// Where a run is, read the way the barrier reads it. `Settled` because
    /// these tests are about the queue and not about git — the landed reading
    /// has its own tests, in `worklist`, against real branches.
    fn at(store: &Store, id: &str) -> (Worklist, Position) {
        let w = store.worklist(id).expect("the list");
        let p = worklist::position(store, &w, Reading::Settled);
        (w, p)
    }

    /// A store of its own **and** a repository beside it, the project rooted
    /// there — the shape `worklist.rs`'s `scratch` builds, because the evidence
    /// block is the first thing in this file asked to read what a member put on
    /// a trunk. Members made against it carry `project: wsp`; [`task`] does not
    /// set one.
    ///
    /// Like `running`, this isolates the environment too: landing reads real
    /// worktrees under the repository, and the verbs around them ask herdr who
    /// is standing where.
    fn grounded(tag: &str) -> (util::Isolated, Store, std::path::PathBuf) {
        let env = util::isolated(&format!("wlground-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_run(&repo, &["init", "--quiet", "-b", "master"]);
        std::fs::write(repo.join("kept.txt"), "one\n").unwrap();
        git_run(&repo, &["add", "kept.txt"]);
        git_run(&repo, &["commit", "--quiet", "-m", "first"]);

        let mut p = crate::model::Project::new("wsp");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();

        (env, store, repo)
    }

    fn member(store: &Store, id: &str, status: &str) {
        let mut t = Task::new(id, id);
        t.project = Some("wsp".into());
        t.status_raw = status.into();
        store.save_task(&t).unwrap();
    }

    fn git_run(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A tree on a branch of the task's name with one commit in it, which is
    /// what an agent that has committed and not landed leaves behind.
    fn committed(repo: &std::path::Path, id: &str) -> std::path::PathBuf {
        let dir = repo.join(crate::cmd_checkout::WORKTREES).join(id);
        let from = crate::cmd_checkout::trunk_branch(repo).expect("the repository is on a branch");
        git_run(repo, &["worktree", "add", "--quiet", "-b", id, &dir.display().to_string(), &from]);
        std::fs::write(dir.join(format!("{id}.txt")), "mine\n").unwrap();
        git_run(&dir, &["add", "."]);
        git_run(&dir, &["commit", "--quiet", "-m", id]);
        dir
    }

    /// What `land` is underneath: rebase onto the trunk, fast-forward it. The
    /// reflog entry that names the branch is what the evidence walk reads.
    fn land(repo: &std::path::Path, id: &str) {
        git_run(repo, &["merge", "--ff-only", "--quiet", id]);
    }

    fn gate_of(store: &Store, id: &str) -> String {
        let (w, p) = at(store, id);
        match state(store, &w, &p) {
            State::Ready(f) => format!("ready {}", f.ready.join(" ")),
            State::Waiting(f) => format!("waiting {}", f.waiting.len()),
            State::Shut { gate: Gate::Start, prose, .. } => format!("start {prose}"),
            State::Shut { gate: Gate::Held, prose, .. } => format!("held {prose}"),
            State::Shut { gate: Gate::Parked, prose, .. } => format!("parked {prose}"),
            State::Shut { gate: Gate::After(n), prose, .. } => format!("after {n} {prose}"),
            State::Nothing => "nothing".to_string(),
        }
    }

    fn verdicts(store: &Store, id: &str) -> Vec<String> {
        groups_of(store, id).into_iter().map(|g| g.verdict).collect()
    }

    /// The real one, kept whole: phase four's group-2 verdict as it was
    /// written, whose opening sentence is about where the record had to be
    /// filed rather than about how the group went.
    const G2: &str = "2026-08-20T08:34:12Z **THIS IS THE G2 VERDICT.** The barrier passed \
                      without one; the verb would not amend a barrier already behind it, so \
                      it is recorded here. G2 was `worklist-041` alone, and it is the group \
                      that falsified the predicate the seat carried into it. **FOUR COMMITS, \
                      1106 GREEN, LANDED AT `8e8fd8f` AND CONFIRMED FROM THE TRUNK RATHER \
                      THAN FROM `land`'s OUTPUT.** Only the first was the row; the other \
                      three came out of driving it. The row specified `stopped && standing > \
                      0 && seat`. Driven against the live store first, it fires on every \
                      healthy seat on this machine — 51 of the 52 levels standing on the two \
                      of them are `review`, and a seat cannot take review down. So the \
                      predicate would have marked both working governors as permanently \
                      stalled. The obligation is named instead: an unsettled member of a \
                      running worklist.";

    fn verdict_group(verdict: &str) -> Group {
        Group { members: vec!["wl-001".into()], verdict: verdict.into(), ..Group::default() }
    }

    fn stop_group(stop: &str) -> Group {
        Group { members: vec!["wl-001".into()], stop: stop.into(), ..Group::default() }
    }

    /// Longer than the cap at any of the widths tested, and about the work —
    /// the way a real condition is written, not filler.
    fn long_stop() -> String {
        "every member lands inside its own module and nothing reaches \
         into a neighbour's; the trunk builds green before `go` is given; \
         any red flag on a member stops the group rather than the member. "
            .repeat(6)
    }

    /// `worklist-046`, and the reason the answer is a count and not a longer
    /// cut: **there is no first sentence of this verdict that is not a lie
    /// about it.** Read to its first sentence the group's own record says the
    /// group has no record, which is what the wsp seat reported off it. So the
    /// abridged form says the two things nothing can misread — that the
    /// barrier was passed, and when — and names what prints the rest.
    #[test]
    fn a_verdict_too_long_to_draw_is_counted_rather_than_cut_to_a_sentence_that_denies_it() {
        let p = Paint::new();
        let drawn = verdict_lines(&p, &verdict_group(G2), "phase-four", 62, false);
        assert_eq!(drawn.len(), 1, "one line, whatever the verdict runs to: {drawn:?}");
        let line = &drawn[0];
        assert!(line.contains("2026-08-20"), "the date the barrier was passed survives: {line}");
        assert!(line.contains("lines"), "and the weight of what is not drawn: {line}");
        assert!(
            line.contains("wsp worklist show phase-four --verdicts"),
            "with the command that prints it, so nothing has to be hunted for: {line}",
        );
        assert!(
            !line.contains("THIS IS THE G2 VERDICT"),
            "and no lead is promoted to standing for the whole: {line}",
        );
    }

    /// The other half of the cap, and it is not symmetry for its own sake: a
    /// `passed` announcing itself as one line held elsewhere is a round trip
    /// bought for nothing, and a cap that fires for no gain is the one a
    /// reader learns to type past every time.
    #[test]
    fn a_verdict_short_enough_to_draw_is_drawn_and_not_announced() {
        let p = Paint::new();
        let drawn = verdict_lines(&p, &verdict_group("2026-08-20T09:00:00Z passed"), "batch", 62, false);
        assert_eq!(drawn.len(), 1, "still one line");
        assert!(drawn[0].contains("passed"), "but it is the verdict, not a count of it: {:?}", drawn[0]);
        assert!(!drawn[0].contains("--verdicts"), "and nothing to go and fetch: {:?}", drawn[0]);
    }

    /// Nothing is lost, which is what makes the cap safe to take. `--verdicts`
    /// is the same block with the cap off, in the place it was abridged in —
    /// `--log` also holds the words, in the entry `go` wrote, but only here do
    /// they sit under the group they are a judgement on.
    #[test]
    fn the_flag_draws_the_verdict_whole_where_it_was_abridged() {
        let p = Paint::new();
        let drawn = verdict_lines(&p, &verdict_group(G2), "phase-four", 62, true);
        assert!(drawn.len() > PROSE_LINES, "the cap is off: {} lines", drawn.len());
        let whole = drawn.join(" ");
        assert!(whole.contains("THIS IS THE G2 VERDICT"), "the lead is there");
        assert!(whole.contains("falsified the predicate"), "and so is the middle of it");
    }

    // ---- stop conditions, split by position and never by length ----------

    /// `worklist-053`. Behind the position a stop condition is history — its
    /// barrier was passed, `at` moves forward only — so over the cap it draws
    /// as a count with the command that prints it whole, the verdict's rule
    /// one group later.
    #[test]
    fn a_stop_condition_behind_the_position_is_counted_rather_than_drawn() {
        let p = Paint::new();
        let long = long_stop();
        let drawn = stop_lines(&p, &stop_group(&long), "phase-five", Some(3), 1, 62, false);
        assert_eq!(drawn.len(), 1, "one line, whatever the condition runs to: {drawn:?}");
        let line = &drawn[0];
        assert!(
            line.contains("wsp worklist show phase-five --stops"),
            "with the command that prints it: {line}",
        );
        assert!(
            !line.contains("lands inside"),
            "and none of the prose is promoted to standing for the block: {line}",
        );
    }

    /// The rule this row exists to get right, asserted from both sides at
    /// once. The group AT the position is the live barrier: its condition is
    /// what a governor weighs before passing it, and a summary there has them
    /// pass on words nobody read. Same length that drew as a count one line
    /// up; here it draws whole.
    #[test]
    fn the_stop_at_the_live_barrier_reads_whole_however_long_it_runs() {
        let p = Paint::new();
        let long = long_stop();
        let counted = stop_lines(&p, &stop_group(&long), "phase-five", Some(2), 1, 62, false);
        assert_eq!(counted.len(), 1, "the same prose one barrier back draws as a count");
        let whole = stop_lines(&p, &stop_group(&long), "phase-five", Some(1), 1, 62, false);
        assert!(whole.len() > PROSE_LINES, "at the position the cap does not apply");
        assert!(
            whole.join(" ").contains("lands inside"),
            "and it is the prose itself, not a summary of it",
        );
    }

    /// Ahead of the position the prose is the only text on the page nobody
    /// has read yet — the most useful writing in the plan reading, not the
    /// least. Length is irrelevant there too, and this is the arm that makes
    /// the split positional rather than a second verdict cap.
    #[test]
    fn a_group_ahead_of_the_position_has_unread_prose_and_it_draws_whole() {
        let p = Paint::new();
        let long = long_stop();
        let drawn = stop_lines(&p, &stop_group(&long), "phase-six", Some(1), 2, 62, false);
        assert!(drawn.len() > PROSE_LINES, "no cap in front of the run");
        assert!(drawn.join(" ").contains("any red flag"), "and it is the prose");
    }

    /// The other half of the cap, on the verdicts' reasoning: announcing a
    /// short condition costs the line the condition itself would have cost,
    /// and carries none of it. A round trip bought for nothing teaches the
    /// reader to type past every cap they meet.
    #[test]
    fn a_short_stop_behind_the_position_still_draws_rather_than_announces() {
        let p = Paint::new();
        let drawn =
            stop_lines(&p, &stop_group("it has to land clean"), "batch", Some(4), 1, 62, false);
        assert_eq!(drawn.len(), 1, "one line either way");
        assert!(drawn[0].contains("land clean"), "but it is the condition: {:?}", drawn[0]);
        assert!(!drawn[0].contains("--stops"), "and nothing to go and fetch: {:?}", drawn[0]);
    }

    /// Nothing is lost behind the position either, which is what makes the
    /// positional cap safe to take: `--stops` is the block with the cap off,
    /// in the place it was counted.
    #[test]
    fn the_flag_draws_a_counted_stop_whole_where_it_was_counted() {
        let p = Paint::new();
        let long = long_stop();
        let drawn = stop_lines(&p, &stop_group(&long), "phase-five", Some(3), 1, 62, true);
        assert!(drawn.len() > PROSE_LINES, "the cap is off: {} lines", drawn.len());
        assert!(drawn.join(" ").contains("trunk builds green"), "and it is the prose");
    }

    /// A finished run stands past every barrier, so `at` is none and every
    /// condition on the page is history — the state phase-four and phase-five
    /// were actually read in, 88 lines of crossed prose between them.
    #[test]
    fn once_every_barrier_is_passed_every_condition_on_the_page_is_history() {
        let p = Paint::new();
        let long = long_stop();
        for ordinal in [1usize, 2, 7] {
            let drawn = stop_lines(&p, &stop_group(&long), "phase-four", None, ordinal, 62, false);
            assert_eq!(
                drawn.len(),
                1,
                "group {ordinal} of a finished list counts like any other behind"
            );
            assert!(drawn[0].contains("--stops"));
        }
    }

    /// And the state that forbids the cut entirely: a draft stands in front
    /// of group 1, nothing is behind anything, and no barrier on the page has
    /// been read by anybody.
    #[test]
    fn a_draft_has_no_history_so_no_condition_is_counted() {
        let p = Paint::new();
        let long = long_stop();
        let drawn = stop_lines(&p, &stop_group(&long), "fork-next", Some(1), 1, 62, false);
        assert!(drawn.len() > PROSE_LINES, "group 1 of a draft is the live position");
        let drawn = stop_lines(&p, &stop_group(&long), "fork-next", Some(1), 3, 62, false);
        assert!(drawn.len() > PROSE_LINES, "and so is everything after it");
    }

    /// The writer's end of the same fact. A verdict is composed by somebody who
    /// then reads it back whole and never learns that the surface everybody
    /// else reads draws a count of it — so `go` says so, once, and only when
    /// the two differ.
    #[test]
    fn the_writer_is_told_when_what_they_wrote_is_not_what_will_be_drawn() {
        let (_, said) = verdict_parts(G2);
        let notice = verdict_notice("phase-four", said, 4).expect("a verdict over the cap is reported");
        assert!(notice.contains("--verdicts"), "naming what draws it whole: {notice}");
        assert_eq!(
            verdict_notice("batch", "passed", 4),
            None,
            "and a verdict drawn as written is not remarked on at all",
        );
    }

    /// The two ends agree because they are one rule read twice. A count taken
    /// at a different width is a number about nothing, and the width follows
    /// the widest ordinal, so both sides ask `prose_width` rather than each
    /// wrapping to a constant of its own.
    #[test]
    fn what_go_counts_and_what_show_draws_are_the_same_lines() {
        let p = Paint::new();
        let (_, said) = verdict_parts(G2);
        for groups in [1usize, 9, 10, 100] {
            let width = prose_width(groups);
            let whole = verdict_lines(&p, &verdict_group(G2), "phase-four", width, true);
            let notice = verdict_notice("phase-four", said, groups).expect("over the cap at every width");
            assert!(
                notice.contains(&format!("{} lines", whole.len())),
                "{groups} groups: go says `{notice}` and show draws {} lines",
                whole.len(),
            );
        }
    }

    /// The design's one piece of machinery around judgement, and the whole of
    /// what wsp contributes to it. wsp does not make the call — `fork`'s real
    /// rule was *"if any of the three goes badly, flag and stop rather than
    /// push through"*, which no boolean expresses. What it contributes is the
    /// obligation to make one, and the record that one was made.
    #[test]
    fn a_barrier_with_prose_at_it_will_not_pass_until_somebody_writes_a_sentence() {
        let (_env, store) = running("verdict");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        flagged(&store, &["group", "batch", "1"], &[("stop", "it has to land clean")]);
        started(&store, "batch");

        assert_eq!(gate_of(&store, "batch"), "after 1 it has to land clean");
        assert_eq!(run(&store, &["go", "batch"]), 1, "no sentence, no passage");
        assert_eq!(verdicts(&store, "batch")[0], "", "and nothing was written");

        assert_eq!(run(&store, &["go", "batch", "it", "landed", "clean"]), 0);
        assert!(
            verdicts(&store, "batch")[0].ends_with("it landed clean"),
            "the sentence is on the group the barrier is behind, dated"
        );
        assert_eq!(gate_of(&store, "batch"), "ready wl-002", "and now the next group is named");
    }

    /// The other half of `worklist-047`: the sentence `go` and `hold` take.
    ///
    /// A verdict is composed before it is given — often while the last member
    /// is still landing — and where it is composed is a file, so the same
    /// argument the stop condition makes is made here by the two verbs that
    /// read one. And an empty file is refused rather than recorded: no words at
    /// all is a legal thing to say to `go`, since a barrier with no prose at it
    /// asks for no judgement, but a named source that turned out to hold
    /// nothing is `worklist-036` exactly — the message lost inside the record,
    /// under a success receipt nobody reads at three in the morning.
    #[test]
    fn a_verdict_and_a_hold_come_out_of_the_file_from_names() {
        let (_env, store) = running("verdictfile");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        flagged(&store, &["group", "batch", "1"], &[("stop", "it has to land clean")]);
        started(&store, "batch");

        let file = store.root.join("verdict.md");
        let path = file.to_str().expect("a utf-8 temp dir").to_string();
        std::fs::write(&file, "All three landed.\n\n`worklist-045` carries the fix.\n").unwrap();

        assert_eq!(
            flagged(&store, &["go", "batch", "the three landed"], &[("from", &path)]),
            2,
            "two verdicts arrived for one barrier"
        );
        assert_eq!(verdicts(&store, "batch")[0], "", "and neither of them was written");

        assert_eq!(flagged(&store, &["go", "batch"], &[("from", &path)]), 0);
        assert!(
            verdicts(&store, "batch")[0]
                .ends_with("All three landed. `worklist-045` carries the fix."),
            "the backticks did not survive a shell they never met, or it was not folded: {:?}",
            verdicts(&store, "batch")[0]
        );

        std::fs::write(&file, "  \n\n").unwrap();
        assert_eq!(flagged(&store, &["hold", "batch"], &[("from", &path)]), 2, "an empty file");
        assert_eq!(
            store.worklist("batch").unwrap().status(),
            WorklistStatus::Running,
            "a hold with no reason on it is a run nobody can restart, and it was taken"
        );

        std::fs::write(&file, "wl-002 is blocked on a decision\nthat is Ed's.\n").unwrap();
        assert_eq!(flagged(&store, &["hold", "batch"], &[("from", &path)]), 0);
        let w = store.worklist("batch").unwrap();
        assert_eq!(w.status(), WorklistStatus::Held);
        assert_eq!(
            gate_of(&store, "batch"),
            "held wl-002 is blocked on a decision that is Ed's.",
            "the reason somebody walking up hours later reads"
        );
    }

    /// `-n` is a dry run of the whole verb. Half a dry run — the verdict
    /// written for real and the trees only imagined — is a state nobody typing
    /// it is asking for, and it is one that leaves the barrier passed with the
    /// cleanup still to do.
    #[test]
    fn a_dry_run_passes_nothing_and_writes_nothing() {
        let (_env, store) = running("dry");
        task(&store, "wl-001", "todo");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);

        assert_eq!(flagged(&store, &["go", "batch"], &[("dry-run", "true")]), 0);
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Draft, "not started");

        run(&store, &["go", "batch"]);
        task(&store, "wl-001", "review");
        assert_eq!(gate_of(&store, "batch"), "after 1 ", "group 1 landed and the barrier is shut");
        assert_eq!(flagged(&store, &["go", "batch"], &[("dry-run", "true")]), 0);
        assert_eq!(verdicts(&store, "batch")[0], "", "and it is still shut");
        assert_eq!(gate_of(&store, "batch"), "after 1 ");
    }

    /// A group with no stop condition asks for no judgement — and it is still a
    /// barrier, because passing one is three other things as well: the
    /// at-most-one-running check, the sweep of the trees behind it, and the
    /// same-file report. The whole argument for the sweep being automatic is
    /// that a step nobody is made to run happened zero times in two nights and
    /// left 18 worktrees, so `next` must not walk past one.
    #[test]
    fn a_barrier_with_nothing_written_at_it_is_still_a_barrier_and_still_takes_go() {
        let (_env, store) = running("silent");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        started(&store, "batch");

        assert_eq!(gate_of(&store, "batch"), "after 1 ", "shut, with nothing to read at it");
        assert_eq!(run(&store, &["go", "batch"]), 0, "and three words pass it");
        assert_eq!(gate_of(&store, "batch"), "ready wl-002");
    }

    /// The far side of a barrier is answered where the barrier is passed, not
    /// pointed at (`core-049`). Every turn an agent spends polling is a whole
    /// context re-read, and `go` runs exactly when "what may start now"
    /// changes — so what this asserts is that the state `go` prints is the
    /// state `next` would have been polled for: same walk, same shape, computed
    /// after the write. The printing itself is `report`, shared unchanged; the
    /// dry run keeps its pointer because nothing moved.
    #[test]
    fn go_answers_what_may_start_instead_of_sending_the_reader_to_next() {
        let (_env, store) = running("answered");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        started(&store, "batch");

        // Through the flags parameter rather than the argv words: anything
        // typed after the slug is the verdict's prose by the time `go` sees
        // it, which is the whole point of that grammar.
        assert_eq!(dispatch(&store, &Args::synth("worklist", &["go", "batch"], &[("dry-run", "true")])), 0);
        let mut w = store.worklist("batch").unwrap();
        // A dry run moved nothing: the barrier still stands, so there is no
        // far side to print and the pointer is what remains.
        assert!(
            matches!(state(&store, &w, &worklist::position(&store, &w, Reading::Landed)), State::Shut { .. }),
            "the dry run left the barrier shut"
        );

        assert_eq!(run(&store, &["go", "batch"]), 0);
        w = store.worklist("batch").unwrap();
        let pos = worklist::position(&store, &w, Reading::Landed);
        let st = state(&store, &w, &pos);
        match st {
            State::Ready(f) => {
                assert_eq!(f.ready, vec!["wl-002".to_string()], "the answer go now carries");
                let v = next_json(&w, &pos, &State::Ready(f), &[], None);
                assert_eq!(v["start"], json!(["wl-002"]), "and the JSON key the next reader parses");
                assert_eq!(v["state"], json!("ready"));
            }
            other => panic!("the far side of the barrier is startable, got {other:?}"),
        }
    }

    /// Per `wsp-092`, a start condition on group N is stop prose on group N−1 —
    /// except for group 1, whose start condition is the worklist's own
    /// `## Overview`. That is the one nobody is made to read, because there is
    /// no barrier in front of it. This is that barrier, on exactly the
    /// machinery every other barrier uses, and it is what `worklist edit`
    /// exists to be able to write.
    #[test]
    fn the_overview_is_group_ones_start_condition_and_a_list_that_has_one_stops_at_it() {
        let (_env, store) = running("overview");
        task(&store, "wl-001", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);

        assert_eq!(gate_of(&store, "batch"), "ready wl-001", "nothing written, nothing to read");

        let mut w = store.worklist("batch").unwrap();
        crate::model::set_section_in(&mut w.body, "Overview", "wait for the tuning table");
        store.save_worklist(&w).unwrap();

        assert_eq!(gate_of(&store, "batch"), "start wait for the tuning table");
        assert_eq!(run(&store, &["go", "batch"]), 1, "a start condition is a sentence too");
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Draft);

        assert_eq!(run(&store, &["go", "batch", "the", "table", "is", "agreed"]), 0);
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Running);
        assert_eq!(gate_of(&store, "batch"), "ready wl-001");
    }

    /// A list with no seat routes its members' raised hands to their own
    /// projects' seats, and `go` is where somebody is present to hear it said.
    ///
    /// `robustness-088` is the first member ever referenced across a project
    /// boundary rather than moved, and its routing check arrived at the `wsp`
    /// seat four minutes after it was spawned. Every step of that walk was
    /// correct — its list had no seat, so `seat_for` fell through to the
    /// project chain. What is wrong is the silence: referencing rather than
    /// moving only has its benefit once the list has a seat, and without one it
    /// has the cost of moving with none of the gain while looking like it
    /// works.
    ///
    /// Said, never refused. A list may legitimately be run by somebody who has
    /// not taken the seat, and `go` does not take one either — taking a seat
    /// stands a workspace down from whatever it held before.
    #[test]
    fn a_list_with_no_seat_says_where_its_hands_go_and_still_passes_the_barrier() {
        let (_env, store) = running("seatless");
        task(&store, "wl-001", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);

        let w = store.worklist("batch").unwrap();
        assert!(matches!(seat_on(&store, &w), SeatOn::Nobody), "no record is nobody");
        assert_eq!(run(&store, &["go", "batch"]), 0, "and it is said rather than refused");
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Running);

        // A post standing empty answers the same, because what routing asks is
        // who is in it *now* — `cmd_govern::vacate` keeps the slot and drops
        // the occupancy.
        store.set_governor("batch", json!({ "since": util::now_iso() }));
        assert!(matches!(seat_on(&store, &w), SeatOn::Nobody), "an empty post is nobody");

        // And a seat held from another machine is not one a hand raised here
        // reaches: `seat_of` reads it as absent, so this must too, and it says
        // which machine rather than claiming the list never had a seat.
        store.set_governor("batch", json!({ "workspace": "w1", "host": "another-box" }));
        assert!(matches!(seat_on(&store, &w), SeatOn::Elsewhere(h) if h == "another-box"));

        store.set_governor("batch", json!({ "workspace": "w1", "host": util::hostname() }));
        assert!(matches!(seat_on(&store, &w), SeatOn::Held), "and this is the quiet case");
    }

    /// The constraint the routing step rests on: a hand raised at 3am reaches
    /// whoever is running the list this task is in tonight, and two running
    /// lists holding it is a question with no answer. Checked at `go`, which is
    /// the moment somebody can act on it, and named rather than counted.
    #[test]
    fn a_task_running_in_one_worklist_will_not_start_in_a_second() {
        let (_env, store) = running("exclusive");
        task(&store, "wl-001", "todo");
        run(&store, &["new", "night", "n"]);
        run(&store, &["add", "night", "wl-001"]);
        run(&store, &["new", "other", "o"]);
        run(&store, &["add", "other", "wl-001"]);

        assert_eq!(run(&store, &["go", "night"]), 0);
        assert_eq!(run(&store, &["go", "other"]), 1, "the same task, in a second run");
        assert_eq!(store.worklist("other").unwrap().status(), WorklistStatus::Draft);

        // A plan is not a run: holding the first frees the task for the second.
        assert_eq!(run(&store, &["hold", "night", "not", "tonight"]), 0);
        assert_eq!(run(&store, &["go", "other"]), 0);
    }

    /// `hold` means *start nothing more* and means nothing stronger. Work in
    /// flight cannot be unwound, and a verb that pretended otherwise would be
    /// promising the one part of this design that could not be built.
    #[test]
    fn holding_starts_nothing_more_and_takes_nothing_back() {
        let (_env, store) = running("hold");
        task(&store, "wl-001", "doing");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["go", "batch"]);

        assert_eq!(run(&store, &["hold", "batch"]), 2, "a stop with no reason on it");
        assert_eq!(run(&store, &["hold", "batch", "the", "table", "moved"]), 0);
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Held);
        assert_eq!(gate_of(&store, "batch"), "held the table moved", "and the reason is readable back");
        assert_eq!(
            store.task("wl-001").unwrap().status_raw,
            "doing",
            "what was already going is untouched — holding is about what starts next"
        );

        assert_eq!(run(&store, &["go", "batch", "settled", "again"]), 0);
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Running);
    }

    /// `wsp-173`'s done-when: a person's pause at a barrier, and the way back
    /// from it, leave that barrier exactly as owed as they found it.
    ///
    /// The failure it is written against is a resume that passes: on
    /// 2026-10-05 the only pause was `hold`, the only way back was `go`, and
    /// `go` at a barrier is the pass — wsp-process group 3 was passed
    /// unchecked by a governor who meant to carry on. So the verbs that could
    /// pass it are refused while it is parked, and the resume is checked for
    /// what it did *not* write as much as for what it did.
    #[test]
    fn a_list_parked_at_a_barrier_resumes_with_the_barrier_still_owed_and_no_verdict() {
        let (_env, store) = running("park");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        flagged(&store, &["group", "batch", "1"], &[("stop", "it has to land clean")]);
        started(&store, "batch");
        assert_eq!(gate_of(&store, "batch"), "after 1 it has to land clean", "standing at the barrier");

        assert_eq!(run(&store, &["park", "batch"]), 2, "a pause with no reason on it");
        assert_eq!(run(&store, &["park", "batch", "Ed", "is", "away"]), 0);
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Parked);
        assert_eq!(gate_of(&store, "batch"), "parked Ed is away", "and the reason reads back");

        // The two verbs that would decide the barrier, both refused, and
        // neither leaves a mark.
        assert_eq!(run(&store, &["go", "batch", "carry", "on"]), 1, "go passes barriers; a pause is not one");
        assert_eq!(run(&store, &["hold", "batch", "not", "yet"]), 1, "nobody is deciding a barrier while it is paused");
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Parked);
        assert_eq!(verdicts(&store, "batch")[0], "", "neither wrote at the barrier");

        assert_eq!(run(&store, &["resume", "batch"]), 0);
        let w = store.worklist("batch").unwrap();
        assert_eq!(w.status(), WorklistStatus::Running);
        assert_eq!(verdicts(&store, "batch")[0], "", "a resume records no verdict");
        assert_eq!(
            gate_of(&store, "batch"),
            "after 1 it has to land clean",
            "the same barrier, still owed and still asking its question"
        );
        let log = w.section("Log").unwrap_or_default();
        assert!(!log.contains("passed"), "nothing was passed: {log}");
        assert!(log.contains("resumed"), "and the resume is on the record: {log}");

        // Resuming what is not parked does nothing.
        assert_eq!(run(&store, &["resume", "batch"]), 0, "running already");
        assert_eq!(verdicts(&store, "batch")[0], "");
    }

    /// The stand-in case `wsp-173` names: native-window was *held* by a
    /// governor because `hold` was the only pause there was. Parking a held
    /// list takes the hold back, and resuming it lands on the barrier with the
    /// check owed — never on `go`'s side of it. And `resume` refuses a hold,
    /// whose way back is a verdict.
    #[test]
    fn a_held_list_parked_and_resumed_owes_its_barrier_rather_than_passing_it() {
        let (_env, store) = running("parkheld");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        flagged(&store, &["group", "batch", "1"], &[("stop", "it has to land clean")]);
        started(&store, "batch");

        assert_eq!(run(&store, &["hold", "batch", "a", "pause", "really"]), 0);
        assert_eq!(run(&store, &["resume", "batch"]), 1, "a hold is a barrier's, and `go` answers it");
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Held);

        assert_eq!(run(&store, &["park", "batch", "Ed", "said", "so"]), 0);
        assert_eq!(gate_of(&store, "batch"), "parked Ed said so", "the pause's reason, not the hold's");
        assert_eq!(run(&store, &["resume", "batch"]), 0);
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Running);
        assert_eq!(verdicts(&store, "batch")[0], "");
        assert_eq!(gate_of(&store, "batch"), "after 1 it has to land clean");
    }

    /// A draft has nothing running to pause, and a done list nothing left.
    #[test]
    fn only_a_list_that_has_started_and_not_finished_can_be_parked() {
        let (_env, store) = running("parkdraft");
        task(&store, "wl-001", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        assert_eq!(run(&store, &["park", "batch", "not", "yet"]), 1, "a draft starts nothing until go");
        assert_eq!(store.worklist("batch").unwrap().status(), WorklistStatus::Draft);
        assert_eq!(run(&store, &["done", "batch"]), 0);
        assert_eq!(run(&store, &["park", "batch", "too", "late"]), 1);
        assert_eq!(run(&store, &["resume", "batch"]), 1);
    }

    /// Passing a barrier sweeps the trees behind it, and sweeping a tree
    /// deletes its branch — so a member that landed but never reached `review`
    /// reads as *never started* the moment its tree goes, and the position
    /// slips back onto a group already passed. `next` would then offer to start
    /// work that is on the trunk: a second agent on landed work, caused by the
    /// barrier's own cleanup. The verdict is what stops it, and the member that
    /// caused it is named rather than covered up.
    #[test]
    fn the_run_does_not_go_back_past_a_barrier_somebody_passed_and_says_who_made_it_try() {
        let (_env, store) = running("floor");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["go", "batch"]);
        run(&store, &["go", "batch"]);
        assert_eq!(at(&store, "batch").1.at, Some(2), "group 1 is behind it");

        // The member of the passed group stops looking finished.
        task(&store, "wl-001", "doing");
        let (_, p) = at(&store, "batch");
        assert_eq!(p.at, Some(2), "the run stays where somebody put it");
        assert_eq!(
            p.slipped.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["wl-001"],
            "and the member that would have dragged it back is named"
        );
    }

    /// The record is as much the point of passing a barrier as the verdict is:
    /// `go` writes what each member put on the trunk onto the group it crossed
    /// — one entry per member, surviving the write back to the file — and a
    /// dry run writes nothing at all. Starting a list records nothing either:
    /// it crosses no barrier, so there is no group behind it to have landed.
    #[test]
    fn passing_a_barrier_records_what_its_members_landed_and_a_dry_run_writes_nothing() {
        let (_env, store) = running("record");
        task(&store, "wl-001", "todo");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);

        // Starting: no barrier behind anything, so no record anywhere.
        assert_eq!(run(&store, &["go", "batch"]), 0);
        assert!(groups_of(&store, "batch")[0].landed.is_empty(), "starting crossed nothing");

        // Group 1 finishes on the store — no repository here to land in, which
        // is the design-only shape — and its barrier shuts.
        task(&store, "wl-001", "review");

        assert_eq!(flagged(&store, &["go", "batch"], &[("dry-run", "true")]), 0);
        assert!(groups_of(&store, "batch")[0].landed.is_empty(), "-n wrote nothing");

        assert_eq!(run(&store, &["go", "batch"]), 0);
        let gs = groups_of(&store, "batch");
        assert_eq!(
            gs[0].landed.iter().map(|e| e.member.as_str()).collect::<Vec<_>>(),
            ["wl-001"],
            "one entry per member of the group whose barrier was crossed"
        );
        assert_eq!(
            gs[0].landed[0].commit, None,
            "no repository to look in here, and unknown is said rather than dropped"
        );
        assert!(gs[1].landed.is_empty(), "the group ahead of the barrier is untouched");
    }

    /// The report phase five's stop conditions tell the reader to consult, on
    /// the verb a governor polls **while composing the verdict** — not only on
    /// `go`, which printed it after the verdict was written and the sweep had
    /// run, so reading the evidence required passing the barrier first.
    ///
    /// Two members share a file (room left in it, so they rebase clean), an
    /// unrelated land sits between theirs, and the next group has not started:
    /// the arrangement the reflog walk exists for.
    #[test]
    fn a_shut_barrier_names_which_members_touched_one_file_while_it_stands_shut() {
        let (_env, store, repo) = grounded("evidence");
        std::fs::write(repo.join("shared.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
        git_run(&repo, &["add", "shared.txt"]);
        git_run(&repo, &["commit", "--quiet", "-m", "shared"]);

        for (id, line) in [("wl-001", 0usize), ("wl-002", 9)] {
            member(&store, id, "review");
            let dir = committed(&repo, id);
            let mut lines: Vec<String> =
                std::fs::read_to_string(dir.join("shared.txt")).unwrap().lines().map(String::from).collect();
            lines[line] = id.to_string();
            std::fs::write(dir.join("shared.txt"), lines.join("\n") + "\n").unwrap();
            git_run(&dir, &["commit", "--quiet", "--all", "--message", "shared"]);
            git_run(&dir, &["rebase", "--quiet", "master"]);
            land(&repo, id);
        }
        // The group ahead: never started, which is what makes this a barrier
        // somebody is standing in front of rather than a finished run.
        member(&store, "wl-003", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);
        started(&store, "batch");
        assert_eq!(gate_of(&store, "batch"), "after 1 ", "the work is over and nobody has passed it");

        let w = store.worklist("batch").unwrap();
        let pos = worklist::position(&store, &w, Reading::Landed);
        let st = state(&store, &w, &pos);
        let t = barrier_evidence(&store, &w, &pos, &st)
            .expect("the gate is shut over a finished group, so the evidence is read");
        let lines = touched_lines(&Paint::plain(), &t);
        assert!(
            lines.iter().any(|l| l.contains("same file") && l.contains("shared.txt")
                && l.contains("wl-001") && l.contains("wl-002")),
            "the file two of them shared, named before anybody judges the group: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("landed") && l.contains("wl-001") && l.contains("wl-002")),
            "and the receipts beside it — the whole block `go` prints, from one walk: {lines:?}"
        );

        // The verb itself runs to completion with all of it in view.
        assert_eq!(run(&store, &["next", "batch"]), 0);

        // And nowhere else. Once the barrier is passed there is no shut gate,
        // so a second poll reads nothing rather than reporting on a group
        // whose trees are already gone.
        run(&store, &["go", "batch", "clean"]);
        let w = store.worklist("batch").unwrap();
        let pos = worklist::position(&store, &w, Reading::Landed);
        let st = state(&store, &w, &pos);
        assert!(barrier_evidence(&store, &w, &pos, &st).is_none(), "no shut barrier, no walk");
    }

    /// The honest half of the report, on the surface the verdict is composed
    /// at. A member whose change cannot be read back contributes no overlaps,
    /// so a report that skipped it would turn *we could not look* into *we
    /// looked and it was clean* — here, in front of the person about to judge
    /// the group, not only afterwards in `go`'s receipt.
    #[test]
    fn a_member_nobody_can_read_back_is_named_at_the_barrier_rather_than_counted_clean() {
        let (_env, store, _repo) = grounded("unread");
        // At `review`, so both members are settled and the barrier stands
        // open-able; but nothing ever ran for either, so no reflog places
        // what either of them changed.
        member(&store, "wl-001", "review");
        member(&store, "wl-002", "review");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001", "wl-002"]);
        started(&store, "batch");

        let w = store.worklist("batch").unwrap();
        let pos = worklist::position(&store, &w, Reading::Landed);
        let st = state(&store, &w, &pos);
        let t =
            barrier_evidence(&store, &w, &pos, &st).expect("both settled, and the gate still shut");
        let lines = touched_lines(&Paint::plain(), &t);
        assert!(
            lines.iter().any(|l| l.contains("could not be read back") && l.contains("wl-001") && l.contains("wl-002")),
            "named where silence would read as a clean bill: {lines:?}"
        );
    }

    /// The machine half of the same evidence, under the key names `go --json`
    /// already established — present exactly where a walk ran, absent
    /// otherwise. A governor parsing this must not be able to read an empty
    /// array as *checked and clean* on a poll where nothing was checked.
    #[test]
    fn the_json_carries_the_evidence_at_the_barrier_and_omits_it_where_no_walk_ran() {
        let (_env, store) = running("json-evidence");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        started(&store, "batch");

        let w = store.worklist("batch").unwrap();
        let pos = worklist::position(&store, &w, Reading::Landed);
        let st = state(&store, &w, &pos);
        let t = barrier_evidence(&store, &w, &pos, &st).expect("shut over a settled group");
        let v = next_json(&w, &pos, &st, &[], Some(&t));
        assert_eq!(v["gate"], json!("after 1"));
        assert_eq!(v["same_file"], json!([]), "the walk ran and found nothing shared");
        assert_eq!(
            v["landed"][0],
            json!({ "member": "wl-001", "commit": null, "files": [] }),
            "design-only work is a plain entry, not an unreadable one"
        );
        assert_eq!(v["unread"], json!([]), "and unreadable means something narrower than no repository");

        task(&store, "wl-001", "doing");
        let pos = worklist::position(&store, &w, Reading::Landed);
        let st = state(&store, &w, &pos);
        let t = barrier_evidence(&store, &w, &pos, &st);
        assert!(t.is_none(), "the group is held again: nothing is read");
        let v = next_json(&w, &pos, &st, &[], t.as_ref());
        assert!(v.get("same_file").is_none(), "absent, not empty: {v}");
        assert!(v.get("unread").is_none() && v.get("landed").is_none(), "{v}");
    }

    /// The plan drawing draws a count and never the mapping — there is no lead
    /// of this record that can stand in for it, so the two numbers that cannot
    /// mislead are all that is drawn, and an unplaced member keeps the placed
    /// count below the membership instead of vanishing from it.
    #[test]
    fn the_plan_draws_a_count_of_what_landed_and_never_the_mapping() {
        let p = Paint::plain();
        let mut g = Group { members: vec!["wl-001".into(), "wl-002".into()], ..Group::default() };
        assert!(landed_lines(&p, &g).is_empty(), "nothing recorded, nothing drawn");

        g.landed = vec![
            Landed {
                member: "wl-001".into(),
                commit: Some("e3f1c2ade4b5960f8d21c7ba44f0e6d9a1b2c3d4".into()),
                files: (0..11).map(|i| format!("src/dir{i}/file{i}.rs")).collect(),
            },
            Landed { member: "wl-002".into(), commit: None, files: Vec::new() },
        ];

        let drawn = landed_lines(&p, &g);
        assert_eq!(drawn.len(), 1, "one line whatever the mapping holds: {drawn:?}");
        let line = &drawn[0];
        assert!(line.contains("1 of 2"), "placed, of members — the gap is visible: {line}");
        assert!(line.contains("11 files"), "and the weight of what landed: {line}");
        assert!(
            !line.contains("src/"),
            "never the mapping itself: {line}",
        );

        let solo = Group {
            landed: vec![Landed { member: "wl-001".into(), commit: Some("e3f1c2a".into()), files: vec!["one.rs".into()] }],
            ..Group::default()
        };
        let line = &landed_lines(&p, &solo)[0];
        assert!(line.contains("1 of 1 member") && line.contains("1 file"), "singular reads as one: {line}");
    }


    /// The receipt for the floor, and who is told about it.
    ///
    /// `report` was the only one of four callers that printed anything: `show`
    /// drew a passed group holding a member left behind with the same `✓` as
    /// one that genuinely landed, and neither `--json` object mentioned it. The
    /// mark and the words are one thing now, so a caller that prints neither is
    /// a caller that did not ask.
    #[test]
    fn a_member_the_floor_stepped_over_is_named_and_the_group_it_slipped_in_is_marked() {
        let (_env, store) = running("behind");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["go", "batch"]);
        run(&store, &["go", "batch"]);
        task(&store, "wl-001", "doing");

        let (_, p) = at(&store, "batch");
        let paint = Paint::plain();
        let marks: Vec<String> =
            (1..=p.of).map(|n| group_mark(&paint, p.at, n, &flagged_in(&p))).collect();
        assert_eq!(marks, ["!", "→"], "the group it slipped in is not a group that landed");

        let block = behind_lines(&paint, &p.slipped, "behind", 8).join("\n");
        assert!(block.starts_with("behind  wl-001"), "the member is named: {block}");
        assert!(block.contains("already passed"), "and what that means is said: {block}");
        assert!(
            block.lines().all(|l| l.chars().count() <= 80),
            "inside eighty however deep the column is: {block}"
        );

        assert_eq!(
            behind_json(&p.slipped),
            serde_json::json!([{ "id": "wl-001", "status": "doing" }]),
            "and the machine half carries the disagreement, not only the id"
        );
        assert!(behind_lines(&paint, &[], "behind", 8).is_empty(), "silent in an ordinary run");
    }

    /// The barrier half of the same receipt. `phase-two` group 2 was passed in
    /// silence and every surface drew it as an ordinary passed group until
    /// `ls` learned to draw `!` on the row — at which point the two surfaces
    /// disagreed, and none of them said *which* group or what was wrong with
    /// it. The plan's mark and this block come off one `Position` read, so
    /// they cannot part again.
    #[test]
    fn a_barrier_passed_without_a_verdict_is_marked_and_named_where_behind_is_named() {
        let (_env, store) = running("unwritten");
        for id in ["wl-001", "wl-002", "wl-003"] {
            task(&store, id, "review");
        }
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["add", "batch", "wl-003"]);
        started(&store, "batch");
        crossed(&store, "batch", 1);
        // Group 3 answered over group 2's silence: `phase-two`'s shape.
        crossed(&store, "batch", 3);

        let (_, p) = at(&store, "batch");
        assert_eq!(p.unwritten, [2], "the walk names the group nobody wrote at");

        let paint = Paint::plain();
        let marks: Vec<String> =
            (1..=p.of).map(|n| group_mark(&paint, p.at, n, &flagged_in(&p))).collect();
        assert_eq!(marks, ["✓", "!", "✓"], "group 2 is not a group that landed cleanly: {marks:?}");

        let block = unwritten_lines(&paint, &p.unwritten, "unwritten", 8).join("\n");
        assert!(block.starts_with("unwritten  group 2"), "which group: {block}");
        assert!(block.contains("crossed"), "and what is wrong with it: {block}");
        assert!(
            block.lines().all(|l| l.chars().count() <= 80),
            "inside eighty however deep the column is: {block}"
        );
        assert!(
            unwritten_lines(&paint, &[2, 5], "unwritten", 8)[0].contains("groups 2, 5"),
            "and more than one is said as a list: {:?}",
            unwritten_lines(&paint, &[2, 5], "unwritten", 8)[0]
        );

        // Closing over it says so, once, where the decision is written down.
        assert_eq!(run(&store, &["done", "batch"]), 0);
        let log = store.worklist("batch").unwrap().section("Log").unwrap_or_default();
        assert!(
            log.contains("every barrier passed · unwritten 2"),
            "\"every barrier passed\" without this line is how phase-two got its tick: {log}"
        );
    }

    /// `done` is a decision and it says what it is closing over. A member of a
    /// group already passed is the half of that nobody is waiting on — no
    /// barrier will mention it again, and marking the list finished used to be
    /// the moment it stopped being said anywhere — so the decision is the last
    /// place it can be written down.
    #[test]
    fn done_names_what_slipped_behind_it_and_not_only_what_was_still_open() {
        let (_env, store) = running("donebehind");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["go", "batch"]);
        run(&store, &["go", "batch"]);
        task(&store, "wl-001", "doing");

        assert_eq!(run(&store, &["done", "batch"]), 0);
        let log = store.worklist("batch").unwrap().section("Log").unwrap_or_default();
        assert!(
            log.contains("done at group 2 of 2 — wl-002 · behind wl-001"),
            "what was open and what was left behind are different facts: {log}"
        );
    }

    /// `xN` is a cap on the work — "only two of these at once, they sit near
    /// each other" — and the number that is held back is reported, because a
    /// governor told "1 may start now" about a group of three otherwise goes
    /// looking for the two that are missing.
    #[test]
    fn a_groups_cap_holds_back_what_it_will_not_run_at_once_and_says_how_many() {
        let (_env, store) = running("cap");
        for id in ["wl-001", "wl-002", "wl-003"] {
            task(&store, id, "todo");
        }
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001", "wl-002", "wl-003"]);
        flagged(&store, &["group", "batch", "1"], &[("parallel", "1")]);
        run(&store, &["go", "batch"]);

        let (w, p) = at(&store, "batch");
        let State::Ready(f) = state(&store, &w, &p) else { panic!("nothing has started") };
        assert_eq!(f.ready, ["wl-001"], "one at a time, in the order the group names them");
        assert_eq!(f.capped, 2, "and the other two are said to be held back, not lost");
    }

    /// `done` is somebody's decision that there is nothing left to want, not a
    /// check that the list finished — a run abandoned two groups from the end
    /// is an ordinary thing to be finished with. What it owes is saying what it
    /// closed over, at the moment it happens.
    #[test]
    fn done_is_a_decision_and_names_what_was_still_open_when_it_was_taken() {
        let (_env, store) = running("done");
        task(&store, "wl-001", "doing");
        task(&store, "wl-002", "todo");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        run(&store, &["go", "batch"]);

        assert_eq!(run(&store, &["done"]), 2, "and it is named rather than taken from the seat");
        assert_eq!(run(&store, &["done", "batch"]), 0);
        let w = store.worklist("batch").unwrap();
        assert_eq!(w.status(), WorklistStatus::Done);
        assert!(
            w.section("Log").unwrap_or_default().contains("done at group 1 of 2 — wl-001"),
            "the log says where it was closed and what was open in it"
        );
    }


    // ---- the list of lists ------------------------------------------------

    /// A run that is over: the status `wsp worklist done` records, set directly
    /// because the verb refuses where a barrier is still shut and these tests
    /// are about the reading rather than about the decision.
    fn finished(store: &Store, id: &str) {
        let mut w = store.worklist(id).expect("the list");
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
    }

    fn drawn(store: &Store, all: bool) -> Vec<String> {
        list_lines(&Paint::new(), &worklist::listing(store, store.worklists()), all)
    }

    fn row<'a>(lines: &'a [String], id: &str) -> Option<&'a String> {
        lines.iter().find(|l| l.starts_with(id))
    }

    /// The collapse, and the notice that keeps it honest. A closed segment
    /// taken out of the table reads exactly like a table that never had one —
    /// so it is counted, and the flag that draws it is on the same line, which
    /// is `cmd_project.rs`'s abridged-decision shape.
    ///
    /// **The count is what pays for this**: nothing removes a worklist, they
    /// arrive at roughly one a night, and this is the only place they are ever
    /// managed.
    #[test]
    fn the_closed_segment_is_counted_rather_than_quietly_dropped() {
        let store = scratch("segments");
        task(&store, "wl-001", "done");
        task(&store, "wl-002", "review");
        for (list, member) in [("shut", "wl-001"), ("open", "wl-002")] {
            run(&store, &["new", list, list]);
            run(&store, &["add", list, member]);
            crossed(&store, list, 1);
            finished(&store, list);
        }

        let lines = drawn(&store, false);
        assert!(row(&lines, "open").is_some(), "the unjudged list is drawn: {lines:?}");
        assert!(row(&lines, "shut").is_none(), "the closed one is not: {lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("⋯ 1 closed · wsp worklist ls --all")),
            "and the table says so, and says what draws it: {lines:?}",
        );
        assert!(
            lines.iter().any(|l| l.contains("unjudged — the run is over")),
            "the segment is named where a reader would otherwise read `done` as finished: {lines:?}",
        );

        let all = drawn(&store, true);
        assert!(row(&all, "shut").is_some(), "--all draws it: {all:?}");
        assert!(!all.iter().any(|l| l.contains("⋯")), "and has nothing left to announce: {all:?}");
    }

    /// `worklist-049` drew a tick on a barrier nobody had read, and this is the
    /// same tick one storey up: `phase-two` group 2 was passed in silence, and
    /// because the floor is the group after the *last* one carrying a verdict,
    /// `at` steps over the hole and the row drew `✓` — the same glyph as a run
    /// somebody read every barrier of. The design read it off `wsp worklist ls`
    /// and said one of the five ticks was not true.
    #[test]
    fn a_run_that_walked_past_a_barrier_in_silence_does_not_draw_a_tick() {
        let store = scratch("silent");
        task(&store, "wl-001", "done");
        task(&store, "wl-002", "done");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        crossed(&store, "batch", 2);
        finished(&store, "batch");

        let lines = drawn(&store, false);
        let line = row(&lines, "batch").expect("the row is drawn").clone();
        assert!(line.contains(" ! "), "the mark says a barrier went unread: {line}");
        assert!(!line.contains('✓'), "and not that every one was read: {line}");
        assert!(
            !lines.iter().any(|l| l.contains("closed")),
            "and it is not closed either, however done its rows are: {lines:?}",
        );
    }

    /// A member that has gone keeps a finished run out of `Closed` and adds
    /// nothing to the count of rows anybody can act on — so with everything
    /// else `done` the row would read `✓` and `·` and sit under a heading
    /// saying something is still somebody's, with nothing on it saying what.
    /// The mark is that sentence.
    #[test]
    fn a_finished_run_holding_a_member_that_has_gone_says_so_on_the_row() {
        let store = scratch("gone-row");
        task(&store, "wl-001", "done");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        // Named in the plan, and no task answers to it: the shape `wsp archive`
        // left behind on 2026-08-20 when 266 tasks went at once.
        let mut w = store.worklist("batch").unwrap();
        let mut groups = w.groups();
        groups[0].members.push("wl-404".into());
        w.set_groups(&groups);
        store.save_worklist(&w).unwrap();
        crossed(&store, "batch", 1);
        finished(&store, "batch");

        let lines = drawn(&store, false);
        let line = row(&lines, "batch").expect("the row is drawn").clone();
        assert!(line.contains(" ! "), "the mark stands for the member that is gone: {line}");
        assert!(line.contains(" · "), "and the count is honest: there is no row left to judge: {line}");
    }

    /// The number no surface showed: how many rows on a list are still
    /// somebody's. Zero is a mark rather than a `0`, because a column of counts
    /// with a nought in it is read as a small number when what is meant is that
    /// there is nothing there.
    #[test]
    fn the_row_carries_the_number_of_rows_still_standing_on_it() {
        let store = scratch("open-count");
        task(&store, "wl-001", "review");
        task(&store, "wl-002", "done");
        run(&store, &["new", "batch", "b"]);
        run(&store, &["add", "batch", "wl-001"]);
        run(&store, &["add", "batch", "wl-002"]);
        crossed(&store, "batch", 1);
        crossed(&store, "batch", 2);
        finished(&store, "batch");

        let lines = drawn(&store, false);
        assert!(lines[0].contains("OPEN"), "the column is named: {:?}", lines[0]);
        let line = row(&lines, "batch").expect("the row is drawn").clone();
        assert!(line.contains(" 1 "), "one row is still Ed's: {line}");

        task(&store, "wl-001", "done");
        let line = row(&drawn(&store, true), "batch").expect("still drawn").clone();
        assert!(line.contains(" · "), "and none is drawn as nothing, not as a nought: {line}");
    }
}
