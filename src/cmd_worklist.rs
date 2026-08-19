//! `wsp worklist` — composing a queue of groups, and the window a hand may
//! edit it in.
//!
//! The record is [`crate::model::Worklist`] and where it is up to is
//! [`crate::worklist`]; this is the noun in between, with the same shape as
//! `project` and `machine` — a subcommand, a slug, and `ls` when nothing else
//! is said. What it owns is **editing**: `new`, `add`, `rm`, `mv`, `group`, and
//! the two reading verbs. Running a list is the barrier's — `next`, `go`,
//! `hold`, `done` — and lands here after this.
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

use serde_json::json;

use crate::model::{Group, Worklist, WorklistStatus};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::worklist::{self, Position, Reading, Standing};
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
        "ls" | "list" => list(store, args),
        "show" | "get" => show(store, args),
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
    Err(format!("wsp: no worklist matching `{needle}` — there is {}", names.join(", ")))
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
        groups.push(Group { members: members.clone(), cap: None, stop: String::new() });
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
        groups.insert(at - 1, Group { members: vec![id.clone()], cap: None, stop: String::new() });
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
                    "members": g.members,
                })).collect::<Vec<_>>(),
                "at": pos.at,
                "waiting_on": pos.holding().iter().map(|s| json!({
                    "id": s.id,
                    "status": s.settlement.word(),
                })).collect::<Vec<_>>(),
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
        let w_ord = groups.len().to_string().chars().count();
        // Where the ids begin: the mark, the ordinal and the cap column, each
        // with its two spaces. Everything written under a group hangs off this,
        // so a stop condition and a member's status word both read as belonging
        // to the ids above them rather than to the margin.
        let members_at = w_ord + 9;
        for (i, g) in groups.iter().enumerate() {
            let ordinal = i + 1;
            let mark = match pos.at {
                Some(at) if at == ordinal => p.cyan("→"),
                Some(at) if at > ordinal => p.dim("✓"),
                None => p.dim("✓"),
                _ => " ".to_string(),
            };
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
            if !g.stop.trim().is_empty() {
                let indent = " ".repeat(members_at);
                // Wrapped against the column it starts in, so the whole block
                // sits inside 80 however deep the ordinals go.
                let width = 72usize.saturating_sub(members_at);
                for (n, line) in util::wrap(g.stop.trim(), width).iter().enumerate() {
                    println!("{indent}{}", p.dim(&format!("{}{line}", if n == 0 { "stop: " } else { "      " })));
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
}
