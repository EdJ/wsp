//! wsp — workspace and task control plane for herdr.
//!
//! Durable facts (projects, tags, tasks) live in `~/wsp` as Markdown + git.
//! Live facts (panes, agent status) come from herdr's socket. This binary
//! joins them, and pushes the join back into herdr's sidebar as metadata
//! tokens.

use std::collections::HashMap;

mod cmd_agent;
mod cmd_brief;
mod cmd_machine;
mod cmd_mandate;
mod cmd_project;
mod cmd_spawn;
mod cmd_task;
mod cmd_verify;
mod daemon;
mod detail;
mod fm;
mod herdr;
mod input;
mod kanban;
mod model;
mod overlap;
mod panel;
mod resolve;
mod story;
mod store;
mod sync;
mod tunnel;
mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Flags that never consume the following token.
const BOOL_FLAGS: &[&str] = &[
    "json", "all", "force", "top", "raw", "overview", "details", "decisions", "verbose", "quiet", "yes", "clear", "tree", "inbox", "open", "done",
    "help", "version", "no-commit", "closed", "here", "agent", "no-focus", "terse", "seen",
    // `verify` takes paths as positionals, so every flag it owns has to be
    // known here or `wsp verify --check src/main.rs` eats the path as a value.
    "release", "check", "rm",
];

pub struct Args {
    pub cmd: String,
    pub rest: Vec<String>,
    flags: HashMap<String, Vec<String>>,
}

impl Args {
    fn parse(argv: Vec<String>) -> Args {
        let mut positional: Vec<String> = Vec::new();
        let mut flags: HashMap<String, Vec<String>> = HashMap::new();
        let mut i = 0;
        while i < argv.len() {
            let a = argv[i].clone();
            if let Some(body) = a.strip_prefix("--") {
                if body.is_empty() {
                    // `--` ends flag parsing
                    positional.extend(argv[i + 1..].iter().cloned());
                    break;
                }
                let (name, inline) = match body.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (body.to_string(), None),
                };
                let entry = flags.entry(name.clone()).or_default();
                if let Some(v) = inline {
                    entry.push(v);
                } else if BOOL_FLAGS.contains(&name.as_str()) {
                    entry.push("true".into());
                } else if i + 1 < argv.len() && !argv[i + 1].starts_with('-') {
                    entry.push(argv[i + 1].clone());
                    i += 1;
                } else {
                    entry.push("true".into());
                }
            } else if a.len() >= 2 && a.starts_with('-') && !a[1..].starts_with(|c: char| c.is_ascii_digit()) {
                let name = expand_short(&a[1..]);
                let entry = flags.entry(name.clone()).or_default();
                if BOOL_FLAGS.contains(&name.as_str()) {
                    entry.push("true".into());
                } else if i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
                    entry.push(argv[i + 1].clone());
                    i += 1;
                } else {
                    entry.push("true".into());
                }
            } else {
                positional.push(a);
            }
            i += 1;
        }

        let cmd = if positional.is_empty() { String::new() } else { positional.remove(0) };
        Args { cmd, rest: positional, flags }
    }

    /// A command line one command builds for another, instead of shelling out
    /// to itself.
    ///
    /// `spawn` opens a workspace and then claims a task into the pane it made,
    /// and a claim is thirty lines of guards — done work reopened, a block
    /// walked past, work taken off a live agent — that must have exactly one
    /// implementation. The panel already refuses to keep a second copy of them
    /// and runs the CLI; inside the CLI the same rule means calling the same
    /// function, which needs the arguments it reads.
    pub fn synth(cmd: &str, rest: &[&str], flags: &[(&str, &str)]) -> Args {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in flags {
            map.entry((*k).to_string()).or_default().push((*v).to_string());
        }
        Args {
            cmd: cmd.to_string(),
            rest: rest.iter().map(|s| (*s).to_string()).collect(),
            flags: map,
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }
    pub fn get(&self, name: &str) -> Option<String> {
        self.flags.get(name).and_then(|v| v.first().cloned())
    }
    pub fn all(&self, name: &str) -> Vec<String> {
        self.flags
            .get(name)
            .map(|v| {
                v.iter()
                    .flat_map(|s| s.split(',').map(|x| x.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn json(&self) -> bool {
        self.has("json")
    }
    /// Leave out what the caller already has.
    ///
    /// Not a second rendering of everything and deliberately not a width dial:
    /// measured over 988 `wsp` calls in 221 sessions, what costs context is a
    /// handful of blocks that get re-read rather than the width of a row.
    /// `ls` and `show` are untouched — an `ls` row is 21 tokens of id, status
    /// and title with nothing to remove, and `show` is the task's own prose,
    /// which is the work in hand.
    ///
    /// Two commands honour it, and they are the two that get re-read: the rules
    /// in `brief` and the blocked list in `wip`. Both roughly halve, both are
    /// one command away in full, and both say the block is gone rather than
    /// going quietly. `project show` was the third candidate and is not one —
    /// see the note there.
    ///
    /// `WSP_TERSE` because the caller who wants this is an agent that decided
    /// once, at the top of a session, and should not have to remember a flag on
    /// every call after that. `0`, `false` and empty are off, so a variable
    /// exported by something else does not silently trim anybody's output.
    pub fn terse(&self) -> bool {
        if self.has("terse") {
            return true;
        }
        match std::env::var("WSP_TERSE") {
            Ok(v) => !matches!(v.trim(), "" | "0" | "false" | "no"),
            Err(_) => false,
        }
    }
    /// Every flag name given, for a command that would rather refuse one it
    /// does not know than guess at what was meant. `edit` is the case that
    /// forced this: it took an unrecognised `--<section>` for "no section
    /// given", which is the combined-buffer path, and wrote the payload over
    /// `Overview`. A typo cost prose and printed success.
    pub fn flag_names(&self) -> Vec<&str> {
        self.flags.keys().map(|s| s.as_str()).collect()
    }
    /// Remaining positionals joined — titles, notes, reasons.
    pub fn text(&self, from: usize) -> String {
        self.rest.iter().skip(from).cloned().collect::<Vec<_>>().join(" ")
    }
}

fn expand_short(s: &str) -> String {
    match s {
        "p" => "project".into(),
        "t" => "tag".into(),
        "s" => "status".into(),
        "a" => "all".into(),
        "v" => "verbose".into(),
        "j" => "json".into(),
        "w" => "workspace".into(),
        other => other.to_string(),
    }
}

/// Die quietly when whatever was reading us stops.
///
/// Rust's runtime sets `SIGPIPE` to `SIG_IGN` before `main`, which turns a
/// closed pipe into a write error, and `println!` panics on a write error. So
/// `wsp ls | head` printed a panic and a note about `RUST_BACKTRACE` to stderr
/// — for doing the most ordinary thing anyone does with a list. Worse for an
/// agent than for a person: the output looked right, and the failure was in a
/// stream it may not even be reading.
///
/// Putting the default disposition back is the whole fix. `head` closing the
/// pipe then kills us the way it kills `ls`, which is what every other tool in
/// the pipeline already does.
///
/// Declared here rather than taken from `libc`: this is two lines and one
/// constant, against a dependency the README promises not to add.
fn die_on_broken_pipe() {
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

fn main() {
    die_on_broken_pipe();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = Args::parse(argv);

    if args.has("version") || args.cmd == "version" {
        println!("wsp {VERSION}");
        return;
    }
    if args.cmd.is_empty() || args.cmd == "help" || args.has("help") {
        help();
        return;
    }

    if args.has("no-commit") {
        std::env::set_var("WSP_NO_COMMIT", "1");
    }

    let store = store::Store::open();
    if !store.exists() && args.cmd != "init" && args.cmd != "doctor" {
        eprintln!(
            "wsp: no store at {}. Run `wsp init` first.",
            util::contract(&store.root)
        );
        std::process::exit(2);
    }

    let code = match args.cmd.as_str() {
        "init" => cmd_project::init(&store, &args),

        "project" | "proj" | "p" => cmd_project::dispatch(&store, &args),
        "projects" => cmd_project::list(&store, &args),
        "tree" => cmd_project::tree(&store, &args),

        "add" | "new" => cmd_task::add(&store, &args),
        "ls" | "list" => cmd_task::list(&store, &args),
        "inbox" => cmd_task::inbox(&store, &args),
        "show" | "cat" => cmd_task::show(&store, &args),
        "decide" => cmd_task::decide(&store, &args),
        "note" => cmd_task::note(&store, &args),
        "start" | "doing" => cmd_task::set_status(&store, &args, model::Status::Doing),
        "done" | "close" => cmd_task::done(&store, &args),
        "block" => cmd_task::block(&store, &args),
        "review" => cmd_task::set_status(&store, &args, model::Status::Review),
        "reopen" | "todo" => cmd_task::set_status(&store, &args, model::Status::Todo),
        "mv" | "move" => cmd_task::mv(&store, &args),
        "tag" => cmd_task::tag(&store, &args),
        "prio" | "priority" => cmd_task::prio(&store, &args),
        "next" => cmd_task::next(&store, &args),
        "edit" => cmd_task::edit(&store, &args),
        "rename" => cmd_task::rename(&store, &args),
        "rm" | "remove" => cmd_task::rm(&store, &args),
        "archive" => cmd_task::archive(&store, &args),

        "brief" => cmd_brief::brief(&store, &args),
        "commit-help" => cmd_brief::commit_help(&store, &args),
        "verify" => cmd_verify::verify(&store, &args),
        "claim" => cmd_agent::claim(&store, &args),
        "spawn" => cmd_spawn::spawn(&store, &args),
        "machine" | "machines" => cmd_machine::dispatch(&store, &args),
        "mandate" => cmd_mandate::mandate(&store, &args),
        "release" => cmd_agent::release(&store, &args),
        "pin" => cmd_agent::pin(&store, &args),
        "unpin" => cmd_agent::unpin(&store, &args),
        "where" => cmd_agent::where_am_i(&store, &args),
        "wip" | "status" => cmd_agent::wip(&store, &args),
        "overlap" => cmd_agent::overlap(&store, &args),
        "peek" => cmd_agent::peek(&store, &args),
        "sync" => cmd_agent::sync_once(&store, &args),
        "hook" => cmd_agent::hook(&store, &args),
        "doctor" => cmd_agent::doctor(&store, &args),
        "adopt" => cmd_agent::adopt(&store, &args),
        "view" => detail::run(&store, &args),
        "kanban" | "board" => kanban::run(&store, &args),
        "say" => cmd_agent::say(&store, &args),
        "flag" => cmd_agent::flag(&store, &args),
        "reconcile" => {
            let r = cmd_agent::reconcile(&store, args.has("reap"));
            println!("reconciled {} binding(s) from claims", r.bound);
            println!("named {} pane(s) after the task they hold", r.named);
            if args.has("reap") {
                println!("ended {} claim(s) whose workspace is gone", r.reaped);
            }
            0
        }
        "daemon" => daemon::run(&store, args.has("verbose")),
        "panel" => match args.rest.first().map(|s| s.as_str()) {
            Some("install") => panel::install(&store, &args),
            Some("uninstall" | "remove") => panel::uninstall(&store, &args),
            Some("storyboard") => story::run(&args),
            // `--full` is the panel `Z` opens in a tab: the same panel, at the
            // width of the workspace, and quit rather than kept.
            _ => panel::run(&store, args.has("full")),
        },

        other => {
            eprintln!("wsp: unknown command `{other}`. Try `wsp help`.");
            2
        }
    };
    std::process::exit(code);
}

fn help() {
    let p = util::Paint::new();
    let h = |s: &str| p.bold(s);
    println!(
        r#"{name} {VERSION} — workspace and task control plane for herdr

{projects}
  wsp init                          create the store at ~/wsp
  wsp project add <slug> [--name N] [--parent P] [--tag T]… [--root PATH]…
  wsp project ls|projects [--tag T] list projects
  wsp tree                          hierarchy with open counts
  wsp project show <id> [--decisions]  brief, tags, roots, tasks, agents
  wsp project set <id> k=v…         name/parent/status/brief/tags/roots
  wsp project rm <id> [--force]     remove; --force orphans what it held

{tasks}
  wsp add "title" [-p proj] [-t tag]… [--prio high] [--ref PATH]
  wsp add "title" --parent <id>     a sub-task, filed where its parent is
  wsp ls [-p proj] [-t tag] [-s status] [--all]
  wsp inbox                         tasks with no project
  wsp show <id>                     full task, including notes
  wsp start|review|reopen <id>      move through the workflow
  wsp done <id> [--force]           complete; --force over open sub-tasks
  wsp block <id> "reason"           park it, and say why
  wsp decide <task|proj> "…"      record what was settled, and why
  wsp note <id> "text"              append to the log
  wsp edit <id> [--overview|--details|--decisions]  prose, in $EDITOR
  wsp edit <id> --overview --from F|-    …or from a file, or stdin
  wsp rename <id> "title"           retitle it; the old title goes in the log
  wsp mv <id> -p proj               reassign, sub-tree and all
  wsp mv <id> --parent <id>|none    re-parent it, or detach it
  wsp tag <id> +dsp -ui             adjust tags
  wsp prio <id> high|normal|low     what comes first inside its project
  wsp next [-p proj]                highest-priority actionable task
  wsp rm <id>                       retire it to the archive
  wsp archive [--all]               sweep done tasks older than 30d

{agents}
  wsp brief                         what this pane is for, and who else is working
  wsp commit-help                   how to commit in a tree somebody else is in
  wsp verify [<path>…] [--check] [--release] [--rm]
                                    build and test your change in a tree of your
                                    own, at HEAD — the only build whose result
                                    means anything while somebody else is here
  wsp claim <id>                    bind this pane to a task, leaving the last
  wsp spawn <id> [-p proj] [--agent [--kind claude]] [--on <machine>]
                                    open a workspace on it, claim it there, and
                                    start an agent in it; --no-focus to stay put,
                                    --on to run it on another machine
  wsp mandate [<proj>] [--clear]    standing direction: work here without asking
  wsp release                       unbind this pane
  wsp pin <proj> [-w ws]            pin a workspace to a project
  wsp pin --top [-w ws]             pin it outside the tree entirely
  wsp unpin [-w ws]                 take the pin off again
  wsp where                         what project am I in, and why
  wsp wip                           everything in flight, with agents
  wsp overlap                       who else is standing in this tree
  wsp peek [panel|view|board|<task>]  what is actually on that pane

{machines}
  wsp machine add <name> [<ssh>]    a second machine to run agents on; <ssh> is
                                    a Host alias from ~/.ssh/config, not an address
  wsp machine ls|machines           what exists, and whether it is answering
  wsp machine show <name>           ssh target, tunnel, last seen, why not
  wsp machine set <name> k=v…       ssh/herdr_sock/os/arch/status
  wsp machine rm <name> [--force]   retire it; --force removes the record

{plumbing}
  wsp panel [--full]                the sidebar replacement (runs in a pane);
                                    --full is the whole tree at the width of the
                                    workspace, which Z in the panel opens in a tab
  wsp view [<id>]                   detail pane; follows the panel unless given an id
  wsp kanban|board [<proj>] [--done]  the work as todo/doing/review/done columns;
                                    K in the panel opens it in a tab
  wsp panel install [--all]         split it into a workspace, or all of them
  wsp panel uninstall [-w ws]       take it back out
  wsp sync [--force]                push tokens to herdr once
  wsp daemon [-v]                   events + refresh loop (herdr [[startup]])
  wsp hook <event>                  herdr event-hook entrypoint
  wsp doctor                        integrity check
  wsp say "…" [--clear]             say where you have got to, on your pane
  wsp flag <id> ["why"]             raise a hand on a task, on every panel
  wsp flag <id> --title T --body -  …with a card: a heading and a paragraph
  wsp flag <id> --ask claim         …and a question a keypress answers
  wsp flag [--clear <id>]           what is raised; --clear lowers one
  wsp reconcile [--reap]            rebuild bindings from claims, and rename;
                                    --reap ends claims whose workspace is gone
  wsp adopt [--yes]                 turn live workspaces into tasks

Ids accept a bare suffix (003) or a unique title substring.
Every command takes --json. Set WSP_HOME to relocate the store.
--terse, or WSP_TERSE=1 for a whole session, leaves out what you already have:
the rules in `brief`, the blocked list in `wip`. Each halves; each says so."#,
        name = h("wsp"),
        projects = h("PROJECTS"),
        tasks = h("TASKS"),
        agents = h("AGENTS"),
        machines = h("MACHINES"),
        plumbing = h("PLUMBING"),
    );
}

#[cfg(test)]
mod tests {
    /// The help is the map, and a verb that is not on it does not exist as far
    /// as anyone reading is concerned. `wsp rename` had been there for weeks —
    /// the panel's `e` key runs it — and a task was filed saying renaming was
    /// impossible, with four titles left wrong in another project because the
    /// work stopped rather than being worked around. Nothing was broken; the
    /// map was short of three lines.
    ///
    /// So the map is checked against the territory: every arm of the dispatch
    /// has to appear in the help, under its own name or one of its aliases.
    /// Read out of this file rather than from a table both sides share, because
    /// a table is a third thing to keep true — this way the check reads exactly
    /// what a person reads, and what the binary actually answers to.
    const SRC: &str = include_str!("main.rs");

    fn dispatch() -> Vec<Vec<String>> {
        let body = SRC
            .split("let code = match args.cmd.as_str() {")
            .nth(1)
            .expect("the dispatch moved")
            .split("\n    };")
            .next()
            .unwrap();
        let mut out = Vec::new();
        for line in body.lines() {
            let Some((left, _)) = line.split_once("=>") else { continue };
            let left = left.trim();
            // An arm is one or more string literals: `"rm" | "remove" =>`.
            if !left.starts_with('"') || !left.ends_with('"') {
                continue;
            }
            let names: Vec<String> = left
                .split('|')
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
                .collect();
            if !names.is_empty() {
                out.push(names);
            }
        }
        out
    }

    fn help_text() -> &'static str {
        SRC.split("fn help()").nth(1).expect("the help moved")
    }

    #[test]
    fn every_verb_the_binary_answers_to_is_on_the_map() {
        let help = help_text();
        // `wsp start|review|reopen` puts three verbs on one line, so a name
        // counts wherever it is followed by a space, a newline or the next
        // alternative.
        let named = |n: &str| {
            [
                format!("wsp {n} "),
                format!("wsp {n}\n"),
                format!("wsp {n}|"),
                format!("|{n} "),
                format!("|{n}|"),
            ]
            .iter()
            .any(|pat| help.contains(pat))
        };
        let arms = dispatch();
        assert!(arms.len() > 20, "the dispatch parse found only {} arms", arms.len());
        let missing: Vec<&Vec<String>> =
            arms.iter().filter(|names| !names.iter().any(|n| named(n))).collect();
        assert!(missing.is_empty(), "verbs the help never mentions: {missing:?}");
    }

    /// The flag is the whole point and the variable is how a session sets it
    /// once, so both have to reach the same answer. `synth` is the path the
    /// panel and `spawn` build arguments on, and it carries no environment,
    /// which is why `terse()` reads the variable itself rather than being
    /// resolved at parse time.
    #[test]
    fn terse_is_the_flag_or_the_variable() {
        // Serialised by being one test: these mutate the process environment.
        std::env::remove_var("WSP_TERSE");
        assert!(!super::Args::synth("brief", &[], &[]).terse());
        assert!(super::Args::synth("brief", &[], &[("terse", "true")]).terse());

        std::env::set_var("WSP_TERSE", "1");
        assert!(super::Args::synth("brief", &[], &[]).terse());

        // A variable somebody else exported must not trim anyone's output.
        for off in ["0", "false", "no", "", "  "] {
            std::env::set_var("WSP_TERSE", off);
            assert!(!super::Args::synth("brief", &[], &[]).terse(), "WSP_TERSE={off:?} turned it on");
        }
        // …but the flag still wins over an explicit off.
        assert!(super::Args::synth("brief", &[], &[("terse", "true")]).terse());
        std::env::remove_var("WSP_TERSE");
    }
}
