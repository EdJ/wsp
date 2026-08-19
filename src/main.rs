//! wsp — workspace and task control plane for herdr.
//!
//! Durable facts (projects, tags, tasks) live in `~/wsp` as Markdown + git.
//! Live facts (panes, agent status) come from herdr's socket. This binary
//! joins them, and pushes the join back into herdr's sidebar as metadata
//! tokens.

use std::collections::HashMap;

mod agent_commands;
mod arrange;
mod cmd_agent;
mod cmd_attempts;
mod cmd_brief;
mod cmd_checkout;
mod cmd_govern;
mod cmd_install;
mod cmd_machine;
mod cmd_migrate;
mod cmd_mandate;
mod cmd_project;
mod cmd_resume;
mod cmd_sandbox;
mod cmd_spawn;
mod cmd_task;
mod cmd_verify;
mod cmd_worklist;
mod daemon;
mod detail;
mod detect_override;
mod draw;
mod fake;
mod fm;
mod guard;
mod herdr;
mod input;
mod kanban;
mod live;
mod model;
mod overlap;
mod panel;
mod place;
mod place_herdr;
mod place_super;
mod resolve;
mod sharing;
mod story;
mod store;
mod sync;
mod tunnel;
mod util;
mod worklist;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The short commit this binary was built from, or empty when the tree it was
/// built in was not a git checkout. `build.rs` carries the argument for why
/// these exist at all.
pub const COMMIT: &str = env!("WSP_COMMIT");

/// Whether that tree held work no commit does. `+dirty` is the load-bearing
/// half of the stamp: a commit hash describes everything about a build except
/// the patch sitting on top of it, and the patch is what goes missing.
pub const DIRTY: bool = matches!(env!("WSP_DIRTY").as_bytes(), b"1");

/// `0.1.0`, `0.1.0 (c52f3c8)`, or `0.1.0 (c52f3c8+dirty)`.
///
/// Printed by `--version` and by the help, which is the version string most
/// people actually see — an agent that runs `wsp help` to find a verb should
/// not have to run a second command to learn whether the binary answering is
/// the one somebody just installed.
pub fn version() -> String {
    match (COMMIT.is_empty(), DIRTY) {
        (true, _) => VERSION.to_string(),
        (false, false) => format!("{VERSION} ({COMMIT})"),
        (false, true) => format!("{VERSION} ({COMMIT}+dirty)"),
    }
}

/// Flags that never consume the following token.
const BOOL_FLAGS: &[&str] = &[
    "json", "all", "force", "top", "raw", "overview", "details", "decisions", "verbose", "quiet", "yes", "clear", "tree", "inbox", "open", "done",
    "help", "version", "no-commit", "closed", "here", "agent", "focus", "no-tree", "terse", "seen", "full",
    // `spawn --no-focus` is what focus not being asked for is now called, and it
    // is kept here rather than deleted so an invocation that still says it —
    // a script, a shell history, the README as it was — parses as a flag
    // instead of eating the task id after it.
    "no-focus",
    // `spawn --headless <task>`, whose positional is a task id.
    "headless",
    // `verify` takes paths as positionals, so every flag it owns has to be
    // known here or `wsp verify --check src/main.rs` eats the path as a value.
    "release", "check", "rm", "alone",
    // And `resume`, whose positional is a task or a project.
    "print",
    // And `checkout`, whose positional is a task id.
    "sweep",
    // And for `install`, whose positional is the binary to install.
    "dry-run",
    // Same for `sandbox`, whose positional is a sandbox name.
    "keep", "seed", "fake",
    // And `despawn`, whose positional is a task id.
    "keep-tree",
    // And `govern`, whose positional is a project: `wsp spawn -p wsp --govern`
    // and `wsp govern wsp --remove` both put the flag last, where anything not
    // known here swallows the argument after it.
    "govern", "remove",
    // And `worklist add <slug> <parent> --sub`, whose positionals are the list
    // and the parent, and `worklist show <slug> --log`.
    "sub", "log",
];

/// Flags that keep their meaning inside a command's payload.
///
/// Everything here is either a question about the invocation itself
/// (`--help`, `--version`) or a dial on how the answer is printed and
/// recorded. None of them is ever the thing being said, which is what makes
/// them safe to go on reading after [`LITERAL_AFTER`] has stopped flag
/// parsing: `wsp note 028 "…" --json` still prints JSON, while `-ui` and
/// `--parent …` reach the command as the text and the tag edits they are.
const GLOBAL_FLAGS: &[&str] = &["json", "help", "version", "no-commit", "terse", "quiet", "verbose"];

/// Commands whose arguments stop being flags once they have their subject, and
/// how many positionals that subject takes.
///
/// This is the seam that `Args` was missing. Flag parsing is one function
/// shared by forty verbs, so it could not know that the token after
/// `wsp note 028` is prose rather than a flag — and prose in this store is
/// mostly *about* the CLI, so it begins with `--parent` or `-p` about as often
/// as not. Same defect from the other end: `wsp tag <id> +dsp -ui` is the
/// removal syntax the help documents, and `-ui` was read as a flag named `ui`,
/// added `dsp` and exited 0 having silently dropped the removal.
///
/// Five commands are listed and no more. Each takes an id and then a payload
/// that is the user's own vocabulary — free prose, or `+tag`/`-tag` — and none
/// of them owns a flag of its own beyond the global ones above, so nothing is
/// lost by stopping. `add`, `find`, `flag` and `say` take prose too but carry
/// real flags after it (`wsp add "…" -p wsp`, `wsp flag <id> --seen`), so they
/// keep ordinary parsing and lean on the whitespace rule in [`Args::scan`].
///
/// `--` still ends flag parsing everywhere, and is still the answer for the
/// case no rule can reach: a payload that is a single flag-shaped word on a
/// command that owns that flag.
const LITERAL_AFTER: &[(&str, usize)] =
    &[("note", 1), ("block", 1), ("park", 1), ("decide", 1), ("rename", 1), ("tag", 1)];

/// Flags a [`LITERAL_AFTER`] command owns, which therefore go on being read
/// inside its payload.
///
/// The rule above stops flag parsing at the subject *because* none of those
/// five owned a flag of its own. `wsp decide <id> "…" --supersedes d1` is the
/// first that does, and the two ways out of that were both worse: dropping
/// `decide` from the list puts prose beginning `--parent` back in reach of the
/// flag parser, which is the defect the list was written for, and demanding
/// the flag before the subject is a word order nobody types.
///
/// The cost is exact and small — a decision whose text is the bare word
/// `--supersedes` — and `--` still ends flag parsing everywhere.
const OWNED_AFTER: &[(&str, &str)] = &[("decide", "supersedes")];

pub struct Args {
    pub cmd: String,
    pub rest: Vec<String>,
    flags: HashMap<String, Vec<String>>,
}

impl Args {
    fn parse(argv: Vec<String>) -> Args {
        // Twice, because the rule depends on the command and the command is
        // itself the first thing the scan finds. The first pass is only ever
        // read for `cmd`: a leading flag may swallow a token that the strict
        // pass hands back as a positional, but neither pass can turn a
        // different token into the verb — the verb is the first bare word
        // either way.
        let cmd = Args::scan(&argv, None, &[]).cmd;
        let literal_after = LITERAL_AFTER.iter().find(|(c, _)| *c == cmd).map(|(_, n)| *n);
        let owned: Vec<&str> =
            OWNED_AFTER.iter().filter(|(c, _)| *c == cmd).map(|(_, f)| *f).collect();
        Args::scan(&argv, literal_after, &owned)
    }

    /// One pass over argv. `literal_after`, when set, is how many positionals
    /// this command parses normally before the rest of the line is its payload.
    fn scan(argv: &[String], literal_after: Option<usize>, owned: &[&str]) -> Args {
        let mut positional: Vec<String> = Vec::new();
        let mut flags: HashMap<String, Vec<String>> = HashMap::new();
        let mut i = 0;
        while i < argv.len() {
            let a = argv[i].clone();
            // The command counts as one of the positionals collected, so the
            // payload of a `LITERAL_AFTER` command starts once we hold it and
            // its subject.
            let past_subject = literal_after.is_some_and(|n| positional.len() > n);
            // Is this token a flag, or is it payload? A flag name never has a
            // space in it, whatever the rest of the token holds — so a quoted
            // sentence arriving whole is prose even on a command that parses
            // flags here, which is the shape this bites most often. Past its
            // subject, a listed command reads only the flags that mean the
            // same thing inside a payload as outside one.
            let is_flag = |name: &str| {
                !name.contains(char::is_whitespace)
                    && (!past_subject
                        || GLOBAL_FLAGS.contains(&name)
                        || owned.contains(&name))
            };

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
                if !is_flag(&name) {
                    positional.push(a);
                    i += 1;
                    continue;
                }
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
                if !is_flag(&name) {
                    positional.push(a);
                    i += 1;
                    continue;
                }
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
        "n" => "dry-run".into(),
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

/// Whether this invocation has to have a store to mean anything.
///
/// Three commands do not. `init` is what makes one. `doctor` reports on the
/// store's state, and "there isn't one" is the most useful thing it can say.
/// And `panel storyboard` reads neither the store nor herdr — it builds its
/// fixtures itself, which is the whole claim in `story.rs`'s header: "the
/// frames come out the same on a laptop with nothing running".
///
/// That claim was false, and not in `story.rs`. The refusal happens here,
/// before dispatch, so the one command documented as needing nothing exited 2
/// with "no store — run wsp init first" on exactly the machine it was written
/// for. A seam that a gate three files away can close is not a seam.
fn needs_store(args: &Args) -> bool {
    if matches!(args.cmd.as_str(), "init" | "doctor") {
        return false;
    }
    !(args.cmd == "panel" && args.rest.first().map(String::as_str) == Some("storyboard"))
}

fn main() {
    die_on_broken_pipe();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = Args::parse(argv);

    if args.has("version") || args.cmd == "version" {
        println!("wsp {}", version());
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
    if !store.exists() && needs_store(&args) {
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
        "find" | "search" => cmd_task::find(&store, &args),
        "inbox" => cmd_task::inbox(&store, &args),
        "show" | "cat" => cmd_task::show(&store, &args),
        "decide" => cmd_task::decide(&store, &args),
        "note" => cmd_task::note(&store, &args),
        "start" | "doing" => cmd_task::set_status(&store, &args, model::Status::Doing),
        "done" | "close" => cmd_task::done(&store, &args),
        "block" => cmd_task::block(&store, &args),
        "park" | "pause" => cmd_task::park(&store, &args),
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

        "attempts" => cmd_attempts::attempts(&store, &args),
        "brief" => cmd_brief::brief(&store, &args),
        "commit-help" => cmd_brief::commit_help(&store, &args),
        "verify" => cmd_verify::verify(&store, &args),
        "checkout" => cmd_checkout::checkout(&store, &args),
        "land" => cmd_checkout::land(&store, &args),
        "install" => cmd_install::install(&store, &args),
        "sandbox" => cmd_sandbox::sandbox(&store, &args),
        "claim" => cmd_agent::claim(&store, &args),
        "spawn" => cmd_spawn::spawn(&store, &args),
        "resume" => cmd_resume::resume(&store, &args),
        "despawn" => cmd_spawn::despawn(&store, &args),
        "machine" | "machines" => cmd_machine::dispatch(&store, &args),
        // The verbs that compose a list. Running one — `next`, `go`, `hold`,
        // `done` — extends this same dispatch and lands after it.
        "worklist" | "wl" => cmd_worklist::dispatch(&store, &args),
        "mandate" => cmd_mandate::mandate(&store, &args),
        "govern" => cmd_govern::govern(&store, &args),
        "release" => cmd_agent::release(&store, &args),
        "pin" => cmd_agent::pin(&store, &args),
        "unpin" => cmd_agent::unpin(&store, &args),
        "where" => cmd_agent::where_am_i(&store, &args),
        "wip" | "status" => cmd_agent::wip(&store, &args),
        "overlap" => cmd_agent::overlap(&store, &args),
        "peek" => cmd_agent::peek(&store, &args),
        "sync" => cmd_agent::sync_once(&store, &args),
        "hook" => cmd_agent::hook(&store, &args),
        // Not `hook`, deliberately, and the two are not variants of one thing.
        // That one is herdr's plugin channel — a multiplexer saying a pane
        // exited — and it ends in a full `sync` over the socket. This is an
        // agent saying what *it* is doing, from inside its own hook, several
        // times a turn, and it must touch nothing but its own seat's file.
        "report" => place_super::report(&args),
        "doctor" => cmd_agent::doctor(&store, &args),
        "adopt" => cmd_agent::adopt(&store, &args),
        "migrate" => cmd_migrate::run(&store, &args),
        "code" => cmd_migrate::code(&store, &args),
        "view" => detail::run(&store, &args),
        "kanban" | "board" => kanban::run(&store, &args),
        "say" => cmd_agent::say(&store, &args),
        "tell" => cmd_agent::tell(&store, &args),
        "flag" => cmd_agent::flag(&store, &args),
        "reconcile" => {
            let r = cmd_agent::reconcile(&store, args.has("reap"));
            println!("reconciled {} binding(s) from claims", r.bound);
            println!("named {} pane(s) after the task they hold", r.named);
            if args.has("reap") {
                println!("ended {} claim(s) whose workspace is gone", r.reaped);
                println!("emptied {} seat(s) whose workspace is gone", r.stood_down);
                println!("forgot {} panel record(s) whose workspace is gone", r.forgotten);
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
        // The same panel, drawn by a host that owns the cells rather than by a
        // terminal: JSON in on stdin, frames out on stdout. Not for people —
        // herdr's forked sidebar spawns it.
        "surface" => panel::surface(&store),

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
        r#"{name} {version} — workspace and task control plane for herdr

{projects}
  wsp init                          create the store at ~/wsp
  wsp project add <slug> [--name N] [--parent P] [--tag T]… [--root PATH]…
  wsp project ls|projects [--tag T] list projects
  wsp tree                          hierarchy with open counts
  wsp project show <id> [--decisions] [--handbook]  brief, tags, roots, tasks
  wsp project edit <id> --handbook -   what an arriving agent is told: what the
                                    work is for, and which file in the repo
                                    holds the map of the code
  wsp project set <id> k=v…         name/parent/status/brief/tags/roots
  wsp project rm <id> [--force]     retire it to the archive; --force orphans
                                    the tasks and children it still held

{tasks}
  wsp add "title" [-p proj] [-t tag]… [--prio high] [--ref PATH]
  wsp add "title" --parent <id>     a sub-task, filed where its parent is
  wsp ls [-p proj] [-t tag] [-s status] [--all]
  wsp find <text> [-p proj] [--all] [--full]
                                    every task the words are in — the title or
                                    the prose; the project you are in unless
                                    --all, and it says when the answer is
                                    somewhere you did not look. Stops at 20
                                    hits and says how many more; --full for
                                    all of them
  wsp inbox                         tasks with no project
  wsp show <id>                     full task, including notes
  wsp start|review|reopen <id>      move through the workflow
  wsp done <id> [--force]           complete; --force over open sub-tasks
  wsp block <id> "reason"           stop it: somebody owes you an answer
  wsp park <id> "reason"            not yet, deliberately — say what brings
                                    it back. Open work, sorted last and drawn
                                    quiet, and not counted as wanting you
  wsp decide <task|proj> "…"      record what was settled, and why
  wsp decide <t|p> "…" --supersedes d1   …and which earlier one it replaces
  wsp note <id> "text"              append to the log
  wsp block|park|decide|note <id> - | --from FILE
                                    …or from stdin, or a file. A paragraph
                                    typed between double quotes is rewritten by
                                    the shell — every backtick in it runs a
                                    command — and `-` is the path that never
                                    meets one. For the log, one entry is one
                                    line, so what arrives on several is folded
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
  wsp brief --session               …and the work itself: the task's prose, what
                                    binds it, what it names, the handbook. The
                                    SessionStart hook's call — paid once at the
                                    top of a session, not on every brief after
  wsp commit-help                   how to commit in a tree somebody else is in
  wsp checkout [<id>]               a working tree of your own for the task in
                                    hand, under .worktrees/, on its own branch —
                                    nobody else's edits are in it and yours are
                                    in nobody else's commit
  wsp checkout [<id>] --rm [--force]  end it, when the task is genuinely over;
                                    the branch stays if it holds work, and
                                    --force is needed to lose uncommitted work
  wsp checkout --sweep [-n]         …or every tree here whose task is closed and
                                    nobody removed; skips any tree somebody is
                                    standing in or has work in, -n to look first
  wsp land [<id>]                   rebase it onto the trunk and fast-forward the
                                    trunk onto it; prints what actually moved.
                                    The tree stays — landing is not finishing
  wsp verify [<path>…] [--check] [--release] [--rm [--all]]
                                    build and test your change at HEAD, in one
                                    of a few warm trees this machine shares —
                                    yours alone while it builds, and cold only
                                    when they are all busy; --rm drops the one
                                    you built in, --all every free one
  wsp verify --alone                …or every test in a process of its own,
                                    ~90s, naming the failures and nothing
                                    else — what to reach for when a test goes
                                    red and then green
  wsp install [<path>] [--why "…"] [-n] [--force] [--to PATH]
                                    put that build at ~/.local/bin/wsp, one
                                    install at a time — the one file nothing can
                                    isolate; defaults to your verify tree's
                                    release build, -n to look without touching it
  wsp sandbox [--seed] [--name N]   a whole isolated wsp — its own herdr session,
                                    store and state — and the exports to use it;
                                    inside it `wsp` is the binary you ran
  wsp sandbox --run "cmd" [--keep]  …or run one thing in it and take it down
  wsp sandbox --fake [--stage F]    …or with no herdr at all: a backend that
                                    answers the socket out of a state you write
                                    down, so wsp can be driven through the ones
                                    a real herdr cannot be put in
  wsp sandbox ls|rm [<name>] [--all]  what is up, and how to drop it
  wsp claim <id>                    bind this pane to a task, leaving the last
  wsp spawn <id> [-p proj] [--agent [--kind claude]] [--on <machine>]
                 [--model <m>] [--effort <e>]
                                    open a workspace on it, claim it there, and
                                    start an agent in it; --focus to go there,
                                    --on to run it on another machine, --full to
                                    start it with sub-agents, workflows and the
                                    MCP servers it is otherwise spawned without.
                                    --model fable|opus|sonnet|haiku, any with
                                    [1m], and --effort low|medium|high|xhigh|max
                                    say what tier to start it at; say neither and
                                    it starts on your settings file, as before.
                                    haiku opens in manual mode, so it is refused
                                    unless --focus says you will be at the pane
  wsp despawn <id> | --pane <seat>  the other end of it, and the whole ending:
                                    end the agent, release the claim, remove the
                                    worktree. A seat that will not close keeps
                                    its claim; a tree with uncommitted work in it,
                                    or with somebody in it, is kept and said so.
                                    --keep-tree leaves the checkout alone
  wsp resume [<id>] [--print]       the agents that were running before herdr
                                    restarted, offered back one row at a time:
                                    ␣ to pick, ↵ to bring those back on the
                                    session they were on. With an id, that one —
                                    which may reach further back than the last
                                    census. --print says how to do it by hand
  wsp mandate [<proj>] [--clear]    standing direction: work here without asking
  wsp govern [<proj>] [--clear]     take the custodial seat on a project: raised
                                    hands under it arrive here instead of on a
                                    person's panel, and this pane stops reading
                                    as an agent that has stalled; --clear stands
                                    down and leaves the seat open, --remove takes
                                    the seat off the project altogether
  wsp govern <proj> --tell "…" | -  say something to whoever is in that seat —
                                    the panel's T, from a shell. Direction is
                                    long prose full of identifiers, so reach for
                                    `--tell -` and pipe it: between double quotes
                                    a shell runs every backtick in it, and the
                                    message arrives fluent with the nouns gone
  wsp spawn -p <proj> --govern      …or start one: a workspace on the project, an
                                    agent in it, the seat taken, and a custodial
                                    work order rather than a claim
  wsp release                       unbind this pane, leaving whatever is in it
  wsp release <id>                  …or end that task's claim, wherever it is
                                    held — including a claim no pane is under
  wsp pin <proj> [-w ws]            pin a workspace to a project
  wsp pin --top [-w ws]             pin it outside the tree entirely
  wsp unpin [-w ws]                 take the pin off again
  wsp where                         what project am I in, and why
  wsp wip                           everything in flight, with agents
  wsp overlap                       who else is standing in this tree
  wsp attempts [<task|proj>] [--all]  every attempt at that work: the tier it was
                                    spawned at, the tier that actually served it,
                                    how long to review, and whether it came back
  wsp peek [panel|view|board|<task>]  what is on that pane, or the frame the
                                    sidebar surface last drew
  wsp tell <id> "…" | -             say something to the agent holding that
                                    task, without ending it — `-` reads the
                                    message from stdin. The repair for an agent
                                    whose turn stopped: the conversation is
                                    intact, and a respawn throws it away

{machines}
  wsp machine add <name> [<ssh>]    a second machine to run agents on; <ssh> is
                                    a Host alias from ~/.ssh/config, not an address
  wsp machine ls|machines           what exists, and whether it is answering
  wsp machine show <name>           ssh target, tunnel, last seen, why not
  wsp machine set <name> k=v…       ssh/backend_at/os/arch/status
  wsp machine rm <name> [--force]   retire it; --force removes the record

{worklists}
  wsp worklist new <slug> "title"   a queue of groups of tasks, run in order,
                                    outside the projects its members live in —
                                    it references them, nothing moves
  wsp worklist add <slug> <task>…   one call, one group; its members run at
                                    the same time. --group N joins a group
                                    that exists instead of making one
  wsp worklist add <slug> <parent> --sub   …or that parent's open sub-tasks as
                                    one group, resolved now and not live
  wsp worklist rm <slug> <task>…    take members out; a group left empty goes
  wsp worklist mv <slug> <task> --group N   between groups, or --after N for a
                                    new one between two that exist
  wsp worklist group <slug> N [--parallel N|none] [--stop "…"|-]
                                    a cap on the work, and the prose read at
                                    the barrier after that group — `-` reads it
                                    from a stream, where a shell never sees it
  wsp worklist ls|show <slug>       every list, or one: the groups, where it is
                                    up to, and which of them may still be edited
  Editing is write-ahead-only: a group at or behind where the list is up to has
  either run or is running, and is refused with what may be edited instead.

{plumbing}
  wsp panel [--full]                the sidebar replacement (runs in a pane);
                                    --full is the whole tree at the width of the
                                    workspace, which Z in the panel opens in a tab
  wsp view [<id>]                   detail pane; follows the panel unless given an id
  wsp kanban|board [<proj>] [--done]  the work as todo/doing/review/done columns;
                                    K in the panel opens it in a tab
  wsp panel install [--all]         split it into a workspace, or all of them —
                                    the way it works without a herdr that draws
                                    the sidebar itself; skipped automatically
                                    while `wsp surface` is running
  wsp panel uninstall [-w ws]       take it back out
  wsp surface                       the panel for a host that owns the cells:
                                    one JSON object per line each way, frames
                                    out. Started by herdr, not by a person
  wsp sync [--force]                push tokens to herdr once
  wsp daemon [-v]                   events + refresh loop (herdr [[startup]])
  wsp hook <event>                  herdr event-hook entrypoint
  wsp report <hook>                 a headless agent's Claude Code hook, saying
                                    what it is doing; silent outside a seat
  wsp doctor                        integrity check
  wsp say "…" [--clear]             say where you have got to, on your pane
  wsp flag <id> ["why"]             raise a hand on a task — at the seat that
                                    governs it, or on every panel if there is none
  wsp flag <id> --title T --body -  …with a card: a heading and a paragraph
  wsp flag <id> --ask claim         …and a question a keypress answers
  wsp flag [--clear <id>] [--seat]  what is raised, and whose it is; --seat
                                    narrows it to this seat's own; --clear lowers
  wsp reconcile [--reap]            rebuild bindings from claims, and rename;
                                    --reap ends claims whose workspace is gone
  wsp adopt [--yes]                 turn live workspaces into tasks
  wsp code [<proj> [<code>]]        the prefix a project's ids take, so a long
                                    slug can still number short: strata-prototype
                                    with code sp gives sp-062. Defaults to the
                                    slug; tasks already handed out keep theirs
  wsp migrate [-n] [--all]          renumber dated ids into each project's own
                                    space, rewriting every reference; -n plans it
                                    and writes nothing. Old ids go on resolving
  wsp migrate --refs <path> [-n]    …and bring a source tree's comments forward

Ids are `<project>-NNN`, continuous within a project rather than within a day,
and a task filed nowhere is `inbox-NNN` until `wsp mv -p` files it — the one
place an id changes, and it is recorded so the old one still resolves.
Ids accept a bare suffix (003) or a unique title substring; a suffix that names
more than one task now lists them rather than answering "no such task".
Text that starts with a flag is text: `wsp note <id> "--parent is add-only"` and
`wsp tag <id> +dsp -ui` both mean what they say. `--` still ends flag parsing,
for the one case that needs it — a payload that is a single flag-shaped word.
Every command takes --json. Set WSP_HOME to relocate the store.
--terse, or WSP_TERSE=1 for a whole session, leaves out what you already have:
the rules in `brief`, the blocked list in `wip`. Each halves; each says so."#,
        name = h("wsp"),
        version = version(),
        projects = h("PROJECTS"),
        tasks = h("TASKS"),
        agents = h("AGENTS"),
        machines = h("MACHINES"),
        worklists = h("WORKLISTS"),
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

    /// A stamp nothing checks is a stamp that can quietly stop being taken,
    /// which is this task's own history: it was marked done once with no
    /// `build.rs` in the tree at all, and `wsp --version` went on saying
    /// `0.1.0` for a day with nobody able to tell from the output that
    /// anything was missing. So the test is not that the string has the right
    /// shape — it is that when this is built where it is developed, in a
    /// checkout, the build actually put a commit in it.
    ///
    /// Guarded on the tree being a checkout, because a build from an unpacked
    /// tarball is allowed to have no stamp, and `build.rs` says so.
    #[test]
    fn a_binary_built_in_a_checkout_knows_which_commit_it_is() {
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(env!("CARGO_MANIFEST_DIR"))
            .args(["rev-parse", "--short", "HEAD"])
            .env_remove("GIT_INDEX_FILE")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let Some(head) = head.filter(|h| !h.is_empty()) else { return };

        assert_eq!(super::COMMIT, head, "the build stamped a commit the tree does not have");
        let v = super::version();
        assert!(v.starts_with(super::VERSION), "{v}");
        assert!(v.contains(&format!("({head}")), "the version does not carry the commit: {v}");
        assert_eq!(v.contains("+dirty"), super::DIRTY, "the dirt flag and the string disagree: {v}");
    }

    /// The flag is the whole point and the variable is how a session sets it
    /// once, so both have to reach the same answer. `synth` is the path the
    /// panel and `spawn` build arguments on, and it carries no environment,
    /// which is why `terse()` reads the variable itself rather than being
    /// resolved at parse time.
    #[test]
    fn terse_is_the_flag_or_the_variable() {
        // `WSP_TERSE` is process-wide and cargo runs tests in threads, so being
        // one test only serialises this against itself. The lock is what
        // serialises it against the test next door — the suite's rule is one
        // process-wide resource, one lock, and this was one of the last three
        // places still relying on nobody else happening to look
        // (`robustness-074`). Bare rather than `isolated` because nothing under
        // it reaches a store or a herdr.
        let _env = crate::util::env_lock();
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

    /// What the user typed has to reach the command they typed it at.
    ///
    /// Two tasks, one defect, from opposite ends. `wsp note 028 "--parent
    /// exists only on wsp add"` answered with the usage line: `Args::parse`
    /// took the leading `--parent` for a flag and the prose was gone. And
    /// `wsp tag <id> +dsp -ui` — the removal syntax the help documents — added
    /// `dsp`, dropped the `-ui` into a flag named `ui`, and exited 0.
    ///
    /// Both are the parser deciding what a token means without knowing which
    /// command it is parsing for, so both are asserted here, on the parser,
    /// rather than through the commands they were reported on.
    #[test]
    fn a_payload_that_looks_like_a_flag_still_reaches_its_command() {
        use super::Args;
        let parse = |line: &[&str]| Args::parse(line.iter().map(|s| (*s).to_string()).collect());

        // The prose end. Free text in this store is mostly *about* the CLI, so
        // it begins with a flag about as often as not.
        let a = parse(&["note", "028", "--parent exists only on wsp add"]);
        assert_eq!(a.cmd, "note");
        assert_eq!(a.text(1), "--parent exists only on wsp add");
        assert!(!a.has("parent"), "the prose was read as a flag");

        for verb in ["block", "park", "decide", "rename"] {
            let a = parse(&[verb, "028", "-p is not a thing on this command"]);
            assert_eq!(a.text(1), "-p is not a thing on this command", "{verb}");
            assert!(!a.has("project"), "{verb} lost its payload to a flag");
        }

        // …and a flag the command owns is still its flag inside the payload,
        // which is the one exception. `decide` is the only command with one.
        let a = parse(&["decide", "wsp", "the store is the only writer", "--supersedes", "d1"]);
        assert_eq!(a.text(1), "the store is the only writer", "the prose kept the flag out");
        assert_eq!(a.get("supersedes").as_deref(), Some("d1"));
        // Nobody else's flag becomes readable by being on the list.
        let a = parse(&["note", "028", "text", "--supersedes", "d1"]);
        assert!(!a.has("supersedes"), "`note` does not own it, so it is payload");

        // `add` keeps ordinary parsing — its flags come *after* the title —
        // so what saves it is that no flag name has a space in it.
        let a = parse(&["add", "--parent exists only on wsp add", "-p", "wsp"]);
        assert_eq!(a.rest, vec!["--parent exists only on wsp add"]);
        assert_eq!(a.get("project").as_deref(), Some("wsp"));

        // The tag end, in the exact shape the help documents.
        let a = parse(&["tag", "wsp-055", "+dsp", "-ui"]);
        assert_eq!(a.rest, vec!["wsp-055", "+dsp", "-ui"]);
        assert!(!a.has("ui"), "the removal was eaten by the flag parser");
        // And the removal-only shape, which used to fail loudly instead.
        assert_eq!(parse(&["tag", "wsp-055", "-tmp"]).rest, vec!["wsp-055", "-tmp"]);
        // `--` was the workaround and stays the escape hatch for the case no
        // rule can reach: a payload that is one flag-shaped word.
        assert_eq!(parse(&["tag", "wsp-055", "--", "-tmp"]).rest, vec!["wsp-055", "-tmp"]);
    }

    /// The other half of the same change: nothing that used to parse may stop.
    ///
    /// Stopping flag parsing at a command's payload is only safe because the
    /// five commands that do it own no flags of their own, and because the
    /// ones that do — `add`, `find`, `flag`, `spawn` — were left alone. This
    /// is that claim, written down.
    #[test]
    fn the_flags_that_are_flags_still_parse() {
        use super::Args;
        let parse = |line: &[&str]| Args::parse(line.iter().map(|s| (*s).to_string()).collect());

        // Globals go on meaning what they mean inside a payload, on both sides
        // of the subject.
        let a = parse(&["note", "028", "the tail is right", "--json"]);
        assert!(a.json() && a.text(1) == "the tail is right");
        let a = parse(&["note", "--json", "028", "the tail is right"]);
        assert!(a.json() && a.text(1) == "the tail is right");
        assert!(parse(&["tag", "028", "+dsp", "--json"]).json());

        // Commands that carry flags after their prose keep them.
        let a = parse(&["add", "Retune the early reflections", "-p", "verb", "-t", "dsp", "--prio", "high"]);
        assert_eq!(a.rest, vec!["Retune the early reflections"]);
        assert_eq!(a.get("project").as_deref(), Some("verb"));
        assert_eq!(a.get("tag").as_deref(), Some("dsp"));
        assert_eq!(a.get("prio").as_deref(), Some("high"));
        let a = parse(&["find", "reverb", "-p", "wsp", "--all"]);
        assert_eq!(a.rest, vec!["reverb"]);
        assert!(a.has("all") && a.get("project").as_deref() == Some("wsp"));
        let a = parse(&["flag", "028", "why this stopped", "--seen"]);
        assert!(a.has("seen") && a.text(1) == "why this stopped");

        // A value may hold spaces — it is the *name* that never does.
        assert_eq!(parse(&["project", "add", "verb", "--name=Reverb Lab"]).get("name").as_deref(), Some("Reverb Lab"));
        assert_eq!(parse(&["project", "add", "verb", "--name", "Reverb Lab"]).get("name").as_deref(), Some("Reverb Lab"));
        assert_eq!(parse(&["govern", "wsp", "--tell", "come and look at this"]).get("tell").as_deref(), Some("come and look at this"));

        // `mv --parent` is the flag the prose above is *about*, on the command
        // that really owns it.
        assert_eq!(parse(&["mv", "028", "--parent", "014"]).get("parent").as_deref(), Some("014"));

        // The verb is found the same way whatever leads the line — the first
        // pass exists only to answer this.
        assert_eq!(parse(&["-p", "wsp", "ls"]).cmd, "ls");
        assert_eq!(parse(&["--json", "note", "028", "text"]).cmd, "note");
    }

    /// A rule that names a command nobody dispatches is a rule that does
    /// nothing, and it would do nothing silently — the payload would go on
    /// being parsed as flags with the table looking correct. Same check the
    /// help gets, for the same reason.
    #[test]
    fn every_command_whose_payload_is_literal_is_a_command() {
        let arms = dispatch();
        for (cmd, _) in super::LITERAL_AFTER {
            assert!(
                arms.iter().any(|names| names.iter().any(|n| n == cmd)),
                "`{cmd}` is in LITERAL_AFTER but nothing dispatches it"
            );
        }
    }

    /// The storyboard is the offline surface, and the gate in front of dispatch
    /// is the only thing that can make it not be. Asserted on the predicate
    /// rather than by running the binary, because what went wrong is a
    /// condition, not a code path: `panel` needs the store and `panel
    /// storyboard` does not, and those two differ by one word in `rest`.
    #[test]
    fn the_storyboard_runs_with_no_store() {
        use super::{needs_store, Args};
        assert!(!needs_store(&Args::synth("panel", &["storyboard"], &[])));
        assert!(!needs_store(&Args::synth("init", &[], &[])));
        assert!(!needs_store(&Args::synth("doctor", &[], &[])));

        // The exemption is that one subcommand and no more of `panel`: the
        // live panel reads the store on its first frame, and letting it start
        // without one trades a clear refusal for an empty tree.
        assert!(needs_store(&Args::synth("panel", &[], &[])));
        assert!(needs_store(&Args::synth("panel", &["install"], &[])));
        assert!(needs_store(&Args::synth("ls", &[], &[])));
        // And it is `panel storyboard`, not the word anywhere in the line.
        assert!(needs_store(&Args::synth("storyboard", &[], &[])));
    }
}
