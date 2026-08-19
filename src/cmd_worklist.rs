//! `wsp worklist` — composing a queue of groups, running it, and the barrier
//! between the two.
//!
//! The record is [`crate::model::Worklist`] and where it is up to is
//! [`crate::worklist`]; this is the noun in between, with the same shape as
//! `project` and `machine` — a subcommand, a slug, and `ls` when nothing else
//! is said. Two halves, and the line between them is the barrier: **composing**
//! is `new`, `add`, `rm`, `mv`, `group`, `edit` and the two reading verbs;
//! **running** is `next`, `go`, `hold`, `done`.
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

use crate::model::{Group, Worklist, WorklistStatus};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::worklist::{self, Landing, Position, Reading, Standing};
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
        "edit" => edit(store, args),
        "ls" | "list" => list(store, args),
        "show" | "get" => show(store, args),
        // Running it, which is the barrier. Everything above composes a plan.
        "next" => next(store, args),
        "go" | "start" => go(store, args),
        "hold" | "stop" => hold(store, args),
        "done" | "finish" => done(store, args),
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
                    eprintln!("     group {at} of {} is where it is up to", self.of);
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
    const USAGE: &str = "usage: wsp worklist add <slug> <task>… [--group N]\n       wsp worklist add <slug> <parent> --sub";
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
        groups.push(Group { members: members.clone(), ..Group::default() });
    } else {
        groups[ordinal - 1].members.extend(members.iter().cloned());
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
    }
    0
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

    let mut groups = w.groups();
    let win = window(store, &w);
    let mut gone: Vec<String> = Vec::new();
    let mut emptied = 0;
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
        // A group emptied by hand is a barrier that opens on the first look,
        // which is a group that no longer means anything. It goes, and it is
        // said so rather than left as a numbered blank.
        if groups[gi].members.is_empty() {
            groups.remove(gi);
            emptied += 1;
        }
        gone.push(id);
    }

    w.log(&format!("removed {}", gone.join(" ")));
    save(store, &mut w, &groups, "rm", &format!("rm {}", gone.join(" ")));

    if args.json() {
        println!("{}", worklist_json(store, &w));
    } else {
        let tail = match emptied {
            0 => String::new(),
            1 => " · one group left empty and dropped".to_string(),
            n => format!(" · {n} groups left empty and dropped"),
        };
        println!("removed {} from {}{tail}", gone.join("  "), w.id);
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
        groups.insert(at - 1, Group { members: vec![id.clone()], ..Group::default() });
    } else {
        groups[at - 1].members.push(id.clone());
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
    const USAGE: &str = "usage: wsp worklist group <slug> N [--parallel N|none] [--stop \"…\"|-]";
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
    if !args.has("parallel") && !args.has("stop") {
        eprintln!("{USAGE}");
        eprintln!("       --parallel caps the work; --stop is the prose read at the barrier after it");
        return 2;
    }

    let win = window(store, &w);
    if !win.allows(ordinal) {
        return win.refuse(ordinal, "will not change");
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

    w.log(&format!("group {ordinal} — {} · {}", groups[ordinal - 1].members.join(" "), said.join(", ")));
    save(store, &mut w, &groups, "group", &format!("group {ordinal} {}", said.join(", ")));

    if args.json() {
        println!("{}", worklist_json(store, &w));
    } else {
        println!("{} group {ordinal}: {}", w.id, said.join(" · "));
    }
    0
}

/// The prose read at a barrier: the text as typed, or the stream it names.
///
/// `-` because this is the sentence a shell is worst at carrying. Stop prose is
/// the reason a night stopped — *"if any of the three goes badly, flag and stop
/// rather than push through"* — and it is written in the vocabulary of the
/// work, which means backticks and identifiers, and every backtick inside
/// double quotes runs a command. `-` is the path that never meets a shell.
///
/// Folded to one paragraph, and that is not tidying. `## Groups` is a structure
/// parsed line by line: a blank line inside a `stop:` block ends it, so prose
/// stored with one would come back next read as prose that stops at the gap.
fn stop_prose(args: &Args) -> Result<String, i32> {
    let raw = args.get("stop").unwrap_or_default();
    let src = match raw.trim() {
        // `--stop` with nothing usable after it names the stream, on the same
        // reading `edit --from` gives it: a missing argument is a mistake worth
        // answering, not an editor session nobody asked for.
        "-" | "true" => "-",
        "" | "none" => return Ok(String::new()),
        _ => return Ok(fold(&raw)),
    };
    if util::stdin_is_tty() {
        eprintln!("wsp: nothing is piped in — `--stop -` reads the prose from a stream");
        return Err(2);
    }
    match crate::cmd_task::read_source(src) {
        Ok(text) => {
            let text = fold(&text);
            if text.is_empty() {
                // An empty stream reads exactly like "take the stop condition
                // off", and the two want different things done about them.
                eprintln!("wsp: nothing on stdin — `--stop none` is how a stop condition is taken off");
                return Err(2);
            }
            Ok(text)
        }
        Err(e) => {
            eprintln!("wsp: cannot read stdin: {e}");
            Err(1)
        }
    }
}

fn fold(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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

pub fn list(store: &Store, args: &Args) -> i32 {
    let all = store.worklists();

    if args.json() {
        let out: Vec<_> = all.iter().map(|w| worklist_json(store, w)).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }
    if all.is_empty() {
        println!("no worklists yet — wsp worklist new <slug> \"title\"");
        return 0;
    }

    let p = Paint::new();
    let w_id = all.iter().map(|w| w.id.chars().count()).max().unwrap_or(8).max(8);
    println!(
        "{}  {}  {}  {}  {}",
        p.dim(&util::pad("WORKLIST", w_id)),
        p.dim(&util::pad("STATUS", 7)),
        p.dim("GROUPS"),
        p.dim("AT"),
        p.dim("TITLE")
    );
    for l in &all {
        let pos = worklist::position(store, l, Reading::Settled);
        // The position, or the mark for having no position left to have. A
        // finished list reading `5` beside `5` groups would say it is on the
        // last one, which is the one thing it is not.
        let at = match (pos.at, pos.of) {
            (Some(n), _) => n.to_string(),
            (None, 0) => "·".to_string(),
            (None, _) => "✓".to_string(),
        };
        println!(
            "{}  {}  {}  {}  {}",
            util::pad(&l.id, w_id),
            util::pad(l.status().as_str(), 7),
            util::pad(&pos.of.to_string(), 6),
            util::pad(&at, 2),
            p.dim(&util::truncate(&l.title, 40))
        );
    }
    0
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

/// What being behind means, in the words a reader can act on.
const BEHIND: &str = "in a group already passed, and not finished — the run does not go back for them";

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

/// Which groups a member slipped in, for the mark above.
///
/// The ordinal is carried on `passed` and not on `slipped`, and the two are the
/// same walk: `position` puts every member of a group it walks past on `passed`
/// and the unfinished ones on `slipped`. Read from `passed` here so the mark
/// needs no second reading — and the block still prints off `slipped`, so a
/// member the two ever disagree about is named without a mark rather than lost.
fn slipped_in(pos: &Position) -> BTreeSet<usize> {
    pos.passed.iter().filter(|b| !b.member.finished()).map(|b| b.group).collect()
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
                    "stop": g.stop,
                    "verdict": g.verdict,
                    "members": g.members,
                })).collect::<Vec<_>>(),
                "at": pos.at,
                "waiting_on": pos.holding().iter().map(|s| json!({
                    "id": s.id,
                    "status": s.settlement.word(),
                })).collect::<Vec<_>>(),
                // Carried in the settled reading's object because the settled
                // reading is where it was invisible: this object said `at`,
                // `dangling`, `groups`, `waiting_on` and `worklist`, and a
                // member the floor stepped over is in none of those.
                "behind": behind_json(&pos.slipped),
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
    // this is where it *would* start. The position is derived off the members
    // whatever the status says, and a draft whose first group is already at
    // review would otherwise read as a run that has got somewhere.
    let draft = w.status() == WorklistStatus::Draft;
    println!(
        "{}  {}",
        p.dim(&util::pad("at", 8)),
        match pos.at {
            Some(n) if draft => format!("group {n} of {} · nothing started", pos.of),
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
        let behind_at = slipped_in(&pos);
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
                g.members.join("  ")
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
            // Wrapped against the column it starts in, so the whole block
            // sits inside 80 however deep the ordinals go.
            let width = 72usize.saturating_sub(members_at);
            if !g.stop.trim().is_empty() {
                for (n, line) in util::wrap(g.stop.trim(), width).iter().enumerate() {
                    println!("{indent}{}", p.dim(&format!("{}{line}", if n == 0 { "stop: " } else { "      " })));
                }
            }
            // The verdict under the stop condition it answers. A group with a
            // stop condition and no verdict under it is a barrier that has not
            // been passed, which is a thing worth being able to see in the plan
            // rather than only at `next`.
            if !g.verdict.trim().is_empty() {
                let (when, said) = verdict_parts(&g.verdict);
                let lead = match when.is_empty() {
                    true => "went: ".to_string(),
                    false => format!("went: {when}  "),
                };
                for (n, line) in util::wrap(said, width).iter().enumerate() {
                    println!(
                        "{indent}{}",
                        p.dim(&format!("{}{line}", if n == 0 { lead.clone() } else { " ".repeat(lead.chars().count()) }))
                    );
                }
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
}

/// The front of the run: what may be started now, and what is already going.
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

fn last_logged(w: &Worklist, mark: &str) -> Option<String> {
    let log = w.section("Log").unwrap_or_default();
    log.lines()
        .rev()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest.trim()))
        .find_map(|rest| rest.strip_prefix(mark).map(|s| s.trim().to_string()))
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
/// `-` as the whole of the words reads the sentence from a stream, for the
/// reason `--stop -` does: a verdict is written in the vocabulary of the work,
/// which means backticks and identifiers, and every backtick inside the double
/// quotes a shell needs for a paragraph runs a command.
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
                eprintln!("     wsp worklist ls names them · wsp govern <slug> takes the seat");
            } else {
                eprintln!("{}", worklist_or_why(store, &first).err().unwrap_or_default());
            }
            Err(1)
        }
    }
}

/// The sentence, from the words typed or from the stream a lone `-` names.
fn words(args: &Args, from: usize) -> Result<String, i32> {
    let typed = args.text(from);
    if typed.trim() != "-" {
        return Ok(fold(&typed));
    }
    if util::stdin_is_tty() {
        eprintln!("wsp: nothing is piped in — `-` reads the sentence from a stream");
        return Err(2);
    }
    match crate::cmd_task::read_source("-") {
        Ok(text) => Ok(fold(&text)),
        Err(e) => {
            eprintln!("wsp: cannot read stdin: {e}");
            Err(1)
        }
    }
}

/// The worklist this workspace is the seat for, if it is the seat for one.
///
/// This is what makes the governor's loop three words. `wsp govern <slug>` is
/// how a workspace comes to hold a worklist seat, and one key space means the
/// scope it holds is either a project or a list with no ambiguity to settle.
fn seated(store: &Store, args: &Args) -> Option<Worklist> {
    let ws = args.get("workspace").or_else(|| crate::herdr::Env::read().workspace_id)?;
    let scope = crate::cmd_govern::governs(&store.governors(), &ws)?;
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

    // The barrier this run has reached: the one behind the group at the
    // position, or — where everything is finished — the one behind the last
    // group there is. That last one is not a formality: `worklist-008`'s stop
    // prose is the gate on phase two, and a run that fell straight through to
    // "nothing left" would pass the one barrier the whole exercise exists for.
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
    let crossed = p.at.map(|at| at - 1).unwrap_or(p.of);
    if !draft && crossed >= 1 {
        if let Some(g) = groups.get(crossed - 1).filter(|g| g.verdict.trim().is_empty()) {
            return State::Shut {
                gate: Gate::After(crossed),
                prose: g.stop.trim().to_string(),
                flight: 0,
            };
        }
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
            waiting.push((s.clone(), crate::cmd_agent::claim_where(c)));
        } else if matches!(s.landing, None | Some(Landing::NoBranch) | Some(Landing::NoRepo)) {
            // No branch and nobody holding it: nothing has run for this member.
            // A member with no repository to look in — design-only work — is
            // the same answer for a different reason, and it is the reason
            // `Landing::NoRepo` is not an error.
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
                }))
                .unwrap_or_default()
            );
            return 0;
        }
        for line in behind_lines(&p, &pos.slipped, &p.bold("behind"), 8) {
            println!("{line}");
        }
        println!("{} {}", w.id, p.dim("is done — nothing left to want from it"));
        return 0;
    }
    let st = state(store, &w, &pos);
    let gone = worklist::dangling(store, &w);

    if args.json() {
        println!("{}", serde_json::to_string_pretty(&next_json(&w, &pos, &st, &gone)).unwrap_or_default());
        return 0;
    }
    report(&w, &pos, &st, &gone, seat);
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
                eprintln!("     wsp worklist ls names them · wsp govern <slug> takes the seat");
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
fn report(w: &Worklist, pos: &Position, st: &State, gone: &[String], seat: bool) {
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
            if !verbs.is_empty() {
                println!("{}", p.dim(&verbs));
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

fn next_json(w: &Worklist, pos: &Position, st: &State, gone: &[String]) -> serde_json::Value {
    let mut v = json!({
        "worklist": w.id,
        "status": w.status().as_str(),
        "at": pos.at,
        "of": pos.of,
        // What `report` prints and this object did not, which made the machine
        // half of the same verb the quieter one — and a governor polling
        // `--json` is the reader least likely to go and look.
        "behind": behind_json(&pos.slipped),
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
                Gate::After(n) => format!("after {n}"),
            });
            v["prose"] = json!(prose);
            v["in_flight"] = json!(flight);
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
    let (mut w, said, seat) = match list_and_words(store, args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if w.status() == WorklistStatus::Done {
        eprintln!("wsp: `{}` is done — nothing in it is waiting to start", w.id);
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
        // wrong and wants to know where it is, which is what `next` says.
        println!("{}", Paint::new().dim("no barrier is shut — nothing to pass"));
        report(&w, &pos, &st, &worklist::dangling(store, &w), seat);
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
    let behind = pos.at.map(|at| at - 1).unwrap_or(pos.of);
    let resuming = matches!(shut, Some((Gate::Held, _)));

    // Which barrier was crossed, taken from the gate rather than from the
    // ordinal. Starting a list crosses none: the groups a freshly-started list
    // has already "passed" are work that was finished before it existed, and
    // taking their trees would be a blast radius nobody at this barrier asked
    // for. Resuming a held list crosses the barrier in front of it only if that
    // barrier is still shut — a `hold` taken in the middle of a group has none.
    let crossed = match (&shut, starting) {
        (_, true) => None,
        (Some((Gate::After(n), _)), _) => Some(*n),
        (Some((Gate::Held, _)), _) => (behind >= 1
            && groups.get(behind - 1).is_some_and(|g| g.verdict.trim().is_empty()))
        .then_some(behind),
        _ => None,
    };

    // Read before anything is removed, and that ordering is load-bearing: the
    // report finds a member by its branch tip and the sweep deletes the branch.
    let overlap = match crossed {
        Some(n) => worklist::overlaps(store, &groups[n - 1].members),
        None => worklist::Overlap::default(),
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
            for g in groups.iter_mut().take(behind) {
                if g.verdict.trim().is_empty() {
                    g.verdict = format!("{} already finished when the list started", util::now_iso());
                }
            }
        }
        false => {
            if let Some(n) = crossed {
                groups[n - 1].verdict = verdict_of(&said);
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
    }

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "worklist": w.id,
                "dry_run": dry,
                "started": starting,
                "passed": crossed,
                "verdict": said,
                "same_file": overlap.shared.iter().map(|(f, who)| json!({ "file": f, "members": who })).collect::<Vec<_>>(),
                "unread": overlap.unread,
                "swept": swept.as_ref().map(|s| json!({
                    "removed": s.swept.removed,
                    "branches": s.swept.branches,
                    "kept": s.swept.kept.iter().map(|(t, why)| json!({ "task": t, "why": why })).collect::<Vec<_>>(),
                    "earlier": s.earlier,
                })),
                "dangling": gone,
            }))
            .unwrap_or_default()
        );
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
    if !gone.is_empty() {
        println!("{}  {}", p.bold("gone"), gone.join("  "));
    }
    // Only where a barrier was crossed. Starting a list has no group behind it
    // to have touched anything, and "none" there would be an answer to a
    // question nobody asked.
    if crossed.is_some() {
        say_overlap(&p, &overlap);
    }
    say_swept(&p, swept.as_ref(), crossed.is_some() && args.has("keep"), dry);
    println!("{}", p.dim(&format!("{}  what may start now", how("next", &w, seat))));
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
    eprintln!("     take them out of one of the two, or hold the other — wsp worklist hold <slug> \"why\"");
    Some(1)
}

/// What the group that just landed touched, where two of them touched one file.
///
/// Silence is the answer nearly every time and it is the answer worth having:
/// the composition rule held. It says so in one line rather than printing
/// nothing, because "no output" is exactly the absence the `batch` could not
/// act on.
fn say_overlap(p: &Paint, o: &worklist::Overlap) {
    if o.shared.is_empty() && o.unread.is_empty() {
        println!("{}", p.dim("same file  none — no two members touched one file"));
        return;
    }
    for (file, who) in &o.shared {
        println!("{}  {}  {}", p.bold("same file"), file, p.dim(&who.join(" ")));
    }
    if !o.unread.is_empty() {
        // The honest half. A member nobody could place contributes no overlaps,
        // so leaving it out would turn "we could not look" into "we looked and
        // it was clean" — the absence-as-evidence this report exists to replace.
        println!(
            "{}  {}",
            p.dim(&util::pad("", 9)),
            p.dim(&format!("{} could not be read back off the trunk", o.unread.join(" ")))
        );
    }
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
        eprintln!("     `-` reads the sentence from a stream, where a shell never sees it");
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
    let mut w = match worklist_or_why(store, &needle) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
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
    let tail = match behind.is_empty() {
        true => String::new(),
        false => format!(" · behind {}", behind.join(" ")),
    };

    w.set_status(WorklistStatus::Done);
    w.log(&match pos.at {
        None => format!("done — every group finished{tail}"),
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
        json!({ "id": w.id, "at": pos.at, "open": left, "behind": behind }),
    );

    if args.json() {
        println!(
            "{}",
            json!({ "worklist": w.id, "status": "done", "at": pos.at, "open": left, "behind": behind })
        );
        return 0;
    }
    let p = Paint::new();
    println!("{} {}", p.bold(&w.id), p.dim("done"));
    if !pos.finished() {
        println!(
            "{}",
            p.dim(&format!(
                "closed at group {} of {} · {} still open in it",
                pos.at.unwrap_or(0),
                pos.of,
                match left.is_empty() {
                    true => "nothing".to_string(),
                    false => left.join(" "),
                }
            ))
        );
    }
    for line in behind_lines(&p, &pos.slipped, &p.bold("behind"), 8) {
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
    let pos = worklist::position(store, w, Reading::Settled);
    json!({
        "id": w.id,
        "title": w.title,
        "status": w.status().as_str(),
        "created": w.created,
        "groups": pos.of,
        "at": pos.at,
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

        // Group 1 has landed, group 2 is being waited on, group 3 is ahead.
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

    fn gate_of(store: &Store, id: &str) -> String {
        let (w, p) = at(store, id);
        match state(store, &w, &p) {
            State::Ready(f) => format!("ready {}", f.ready.join(" ")),
            State::Waiting(f) => format!("waiting {}", f.waiting.len()),
            State::Shut { gate: Gate::Start, prose, .. } => format!("start {prose}"),
            State::Shut { gate: Gate::Held, prose, .. } => format!("held {prose}"),
            State::Shut { gate: Gate::After(n), prose, .. } => format!("after {n} {prose}"),
            State::Nothing => "nothing".to_string(),
        }
    }

    fn verdicts(store: &Store, id: &str) -> Vec<String> {
        groups_of(store, id).into_iter().map(|g| g.verdict).collect()
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
            (1..=p.of).map(|n| group_mark(&paint, p.at, n, &slipped_in(&p))).collect();
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

}
