//! wsp — workspace and task control plane for herdr.
//!
//! Durable facts (projects, tags, tasks) live in `~/wsp` as Markdown + git.
//! Live facts (panes, agent status) come from herdr's socket. This binary
//! joins them, and pushes the join back into herdr's sidebar as metadata
//! tokens.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

mod agent_commands;
mod arrange;
mod attention;
mod cmd_agent;
mod cmd_attempts;
mod cmd_brief;
mod cmd_checkout;
mod cmd_govern;
mod cmd_install;
mod cmd_machine;
mod cmd_mandate;
mod cmd_message;
mod cmd_migrate;
mod cmd_project;
mod cmd_resume;
mod cmd_sandbox;
mod cmd_spawn;
mod cmd_task;
mod cmd_verify;
mod cmd_watch;
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
mod message;
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

/// What this binary was built from, as one word a record can be compared
/// against: `c52f3c8`, `c52f3c8+dirty`, or empty when the tree was not a
/// checkout.
///
/// The same two halves `version()` prints, without the package number, because
/// the readers of this are not people: a file written by one build and read by
/// another asks *were these the same rules*, and `0.1.0` has never moved. The
/// dirt is in it for the reason [`DIRTY`] exists at all — two builds at one
/// commit with different patches on top are two different sets of rules, and
/// the patch is the half that is not written down anywhere else.
///
/// Empty compares equal to empty, so two binaries that cannot say where they
/// came from are treated as one build. That is the honest answer rather than a
/// safe one: there is nothing to compare, and a comparison that always failed
/// would put every reader of a stamp permanently in its unknown branch.
pub fn build_stamp() -> String {
    stamp_word(COMMIT, DIRTY)
}

/// The same word, made of a commit and a dirt flag that came from somewhere
/// else — a binary that is not this one, asked what it carries.
///
/// Split out of [`build_stamp`] rather than written twice because the two
/// sides of every comparison in the fleet are made here: a `wsp watch`
/// registers [`build_stamp`], and `wsp install` reads a stamp back out of the
/// artefact it is about to copy and has to produce the same word for the same
/// build. `cmd_install` spelled that rule out a second time and spelled it
/// against the *tree* instead, which is `worklist-042`: two shapes for one
/// question is how they come to disagree.
pub fn stamp_word(commit: &str, dirty: bool) -> String {
    match (commit.is_empty(), dirty) {
        (true, _) => String::new(),
        (false, false) => commit.to_string(),
        (false, true) => format!("{commit}+dirty"),
    }
}

/// `0.1.0`, `0.1.0 (c52f3c8)`, or `0.1.0 (c52f3c8+dirty)`.
///
/// Printed by `--version` and by the help, which is the version string most
/// people actually see — an agent that runs `wsp help` to find a verb should
/// not have to run a second command to learn whether the binary answering is
/// the one somebody just installed.
///
/// Built out of [`build_stamp`] rather than beside it, so what `cmd_install`
/// parses back out of `--version` and what a watch register holds cannot come
/// to disagree about the same binary.
pub fn version() -> String {
    match build_stamp().as_str() {
        "" => VERSION.to_string(),
        stamp => format!("{VERSION} ({stamp})"),
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
    // And `wsp watch <signal>…`, whose positionals are signal names.
    "now", "once", "status",
    // And `worklist add <slug> <parent> --sub`, whose positionals are the list
    // and the parent, and `worklist show <slug> --log`.
    "sub", "log",
    // And the return path. `wsp answer <id> --abandon "the reason"` is the word
    // order somebody types, and without this the reason is eaten as the flag's
    // value and the verb refuses for want of a sentence it was given. `--again`
    // is the escape from the repeat guard and is shared with both `tell` verbs.
    "abandon", "again",
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

/// Flags a verb still accepts and no longer reads.
///
/// [`unknown_flags`] refuses a flag nothing read and the help does not list,
/// and this is the one shape that is neither: a word kept alive on purpose so
/// an old invocation still parses. `spawn --no-focus` asks for what already
/// happens — the default flipped on 2026-08-17 — and it is in [`BOOL_FLAGS`]
/// precisely so a script that still says it fails to eat the id after it. That
/// compatibility has been paid for once; refusing the word now would spend it
/// again from the other end.
///
/// One entry, and it should stay short. A verb that has genuinely stopped
/// taking a flag deletes it from here and from [`BOOL_FLAGS`] together, which
/// is the moment the refusal is the right answer.
const ACCEPTED_UNREAD: &[(&str, &str)] = &[("spawn", "no-focus")];

pub struct Args {
    pub cmd: String,
    pub rest: Vec<String>,
    flags: HashMap<String, Vec<String>>,
    /// Flag names that took a word off the command line — `--from FILE`,
    /// `--title=T` — as against the ones that stand for themselves.
    ///
    /// This is half of the answer to `worklist-036`'s second question, and
    /// [`Args::dropped`] is the other half.
    valued: HashSet<String>,
    /// Which flags anything actually looked at. Interior mutability because
    /// every command takes `&Args` and a read is a read whether or not the
    /// caller holds it mutably; nothing here crosses a thread.
    read: RefCell<HashSet<String>>,
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
        // Which of them ate a word. See [`Args::dropped`]: a flag that stands
        // for itself costs nothing when nobody reads it, and one that took the
        // token after it has taken something that was going somewhere.
        let mut valued: HashSet<String> = HashSet::new();
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
                    valued.insert(name.clone());
                } else if BOOL_FLAGS.contains(&name.as_str()) {
                    entry.push("true".into());
                } else if i + 1 < argv.len() && !argv[i + 1].starts_with('-') {
                    entry.push(argv[i + 1].clone());
                    valued.insert(name.clone());
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
                    valued.insert(name.clone());
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
        Args { cmd, rest: positional, flags, valued, read: RefCell::default() }
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
            // A command line one command builds for another was never on a
            // command line, so there is no word to have been taken off one.
            // [`Args::dropped`] is asked about the invocation, once, in `main`.
            valued: HashSet::new(),
            read: RefCell::default(),
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.mark(name);
        self.flags.contains_key(name)
    }
    pub fn get(&self, name: &str) -> Option<String> {
        self.mark(name);
        self.flags.get(name).and_then(|v| v.first().cloned())
    }
    pub fn all(&self, name: &str) -> Vec<String> {
        self.mark(name);
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
    /// A flag was looked at. Every read goes through here, and that is the
    /// whole of the bookkeeping [`Args::dropped`] needs.
    fn mark(&self, name: &str) {
        self.read.borrow_mut().insert(name.to_string());
    }

    /// Every flag that took a word off the command line and that nothing read.
    ///
    /// # Why this and not a list of the flags each verb knows
    ///
    /// `worklist-036`: **wsp refuses no flag it does not know**, and a flag it
    /// does not know still eats the token after it. `wsp flag <id> --from FILE`
    /// bound the path to an option nothing read, raised a hand with `"text":
    /// ""` and exited 0 — the message lost inside the record, by the one verb
    /// whose whole job is to not lose one, through the spelling every brief
    /// tells an agent to use. Unattended, nobody reads the receipt: a
    /// governor's script with one wrong word raises empty hands all night and
    /// every command exits 0.
    ///
    /// The obvious repair is a vocabulary — each verb declaring what it takes,
    /// checked before dispatch, the way `cmd_task::edit_prose` already does for
    /// itself. It was weighed and not taken, and the reason is that the
    /// vocabulary is a *second copy* of what every verb already knows by
    /// reading its own flags: sixty entries kept by hand, whose omissions
    /// refuse commands that were always valid, and which nothing in the build
    /// can check, because a flag read three helpers deep is unreachable to any
    /// grep. The failure it fixes is silence; the failure it introduces is a
    /// verb that stops taking an argument it has always taken.
    ///
    /// So the thing refused is not an unknown *name* but a **dropped word**. A
    /// value that came off the command line and that nothing looked at is,
    /// exactly and by construction, a thing the caller said and wsp did not
    /// hear — no vocabulary, nothing to maintain, and no way for it to be
    /// wrong about a verb it has never heard of. It costs `--no-focus` nothing,
    /// which is the compatibility case the alternative had to argue with:
    /// a flag that stands for itself takes no word, so nobody reading it is
    /// nobody losing anything.
    ///
    /// What it does not catch is named where it will be read: a mistyped flag
    /// that stands alone — `wsp ls --al` for `--all` — drops no word and goes
    /// on being ignored, because a bare flag nothing read is indistinguishable
    /// from one a verb reads only down the branch this run did not take
    /// (`--force`, `--yes`, `--again`), and complaining about those would put
    /// noise on commands that are correct. That half needs the vocabulary, and
    /// is `worklist-038`.
    ///
    /// Checked in `main` after the command has run, which is the one honest
    /// place: a read happens while the verb runs, so nothing before dispatch
    /// can know. The command has therefore already done what it did — the
    /// message says so — and the exit code is what carries the failure to the
    /// script that would otherwise never hear.
    pub fn dropped(&self) -> Vec<String> {
        let read = self.read.borrow();
        let mut out: Vec<String> =
            self.valued.iter().filter(|n| !read.contains(*n)).cloned().collect();
        out.sort();
        out
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
        "watch" => cmd_watch::watch(&store, &args),
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
        // The return path. `flag` and `tell` are one-directional and stay that
        // way; these three are the case where something is owed back, and the
        // answer is a record before it is a sentence in somebody's pane.
        "ask" => cmd_message::ask(&store, &args),
        "answer" => cmd_message::answer(&store, &args),
        "ack" => cmd_message::ack(&store, &args),
        "reconcile" => {
            let r = cmd_agent::reconcile(&store, args.has("reap"));
            println!("reconciled {} binding(s) from claims", r.bound);
            println!("named {} pane(s) after the task they hold", r.named);
            if args.has("reap") {
                println!("ended {} claim(s) whose workspace is gone", r.reaped);
                println!("emptied {} seat(s) whose agent is gone", r.stood_down);
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
    std::process::exit(misheard(&args, code));
}

/// Say what the command line gave and wsp did not hear, and fail if there was
/// any.
///
/// Two halves, and they answer the two ways a word goes missing. A flag that
/// **took a word** nothing read is [`Args::dropped`] and `worklist-036`. A flag
/// that **stands alone** and is no flag of this verb is [`unknown_flags`] and
/// `worklist-038`. A flag that is both is reported once, as the second: "there
/// is no such flag" is the more useful of the two sentences, and it makes the
/// other one redundant.
///
/// **A command that already failed is left alone.** It has said why in its own
/// words, and a verb that stopped early may not have reached the flag it does
/// read — so the same message would be both noise and a lie about the verb.
/// What this is for is the *other* case: success reported over a word that
/// went nowhere.
fn misheard(args: &Args, code: i32) -> i32 {
    // Said in its own words already. See above.
    if code != 0 {
        return code;
    }
    let unknown = unknown_flags(args);
    let dropped: Vec<String> =
        args.dropped().into_iter().filter(|d| !unknown.iter().any(|u| u.name == *d)).collect();
    if unknown.is_empty() && dropped.is_empty() {
        return code;
    }
    let p = util::Paint::new();
    for u in &unknown {
        let name = p.bold(&format!("--{}", u.name));
        let meant = match &u.meant {
            Some(m) => format!(" — did you mean --{m}?"),
            None => String::new(),
        };
        match &u.verb {
            Some(v) => eprintln!("wsp: `wsp {v}` has no {name}{meant}"),
            None => eprintln!("wsp: no wsp verb takes {name}{meant}"),
        }
    }
    for name in &dropped {
        let value = args.get(name).unwrap_or_default();
        eprintln!(
            "wsp: {} took `{value}` off the command line and nothing read it — check the spelling.",
            p.bold(&format!("--{name}")),
        );
    }
    eprintln!("     The rest of `wsp {}` did what it says; that word went nowhere,", args.cmd);
    eprintln!("     and this is the only way you would have heard. `wsp help` has the flags.");
    2
}

/// A flag that was given, that nothing read, and that the verb does not take.
struct Unknown {
    name: String,
    /// The help entry the verdict came from — `ls`, `project add` — or `None`
    /// when the help does not describe this invocation and the claim is only
    /// that no verb anywhere takes the name.
    verb: Option<String>,
    /// The nearest flag it could have been.
    meant: Option<String>,
}

/// Every flag on the command line that wsp has no use for.
///
/// # Why the help and not a table
///
/// [`Args::dropped`] catches a flag that *ate a word*. The other half —
/// `worklist-038` — is a flag that stands alone: `wsp ls --al` for `--all`
/// drops nothing, so a read tally alone cannot tell it from `--force` on a
/// branch this run did not take. Telling those apart needs a vocabulary.
///
/// The vocabulary the row proposed was a table beside [`LITERAL_AFTER`], sixty
/// entries kept by hand, and its own overview said why that was not worth
/// having: a second copy of what every verb already knows, whose omissions
/// refuse commands that were always valid, and which nothing in the build can
/// check because a flag read three helpers deep is unreachable to any grep.
///
/// So the table is not written; it is **read off the help**, which is the
/// declaration that already exists. It is the document a person is sent to when
/// a flag is refused, it is maintained because it is the map, and
/// `every_verb_the_binary_answers_to_is_on_the_map` already checks it against
/// the dispatch. A verb's flags stop being a second copy when they are the
/// first one.
///
/// That still leaves the two failures the row feared, and both are closed by
/// what the parser already tracks:
///
/// - **A flag the help does not mention.** `--socket`, `--payload`, `--ratio`,
///   `--days` and eighteen others are real and undocumented, and refusing them
///   would be exactly the regression the row warned of. So a flag **anything
///   read** is never refused, whatever the help says — [`Args::mark`] records
///   the ask, not the answer, so `args.has("force")` counts even when `--force`
///   was not given. This also reaches what no grep can: `cmd_watch::spec`
///   reads `--every`, `--settle` and `--heartbeat` through a closure over a
///   `&str`, and the tally sees all three.
/// - **A flag read only down the branch this run did not take.** Then nothing
///   read it and the help must carry it. Two did not: `spawn --no-tree`, which
///   now asks before it branches, and `spawn --no-focus`, which nothing reads
///   by design and is in [`ACCEPTED_UNREAD`].
///
/// What is left is a name nobody asked about and no line of the help gives to
/// this verb, which is a typo or a flag meant for a different command.
///
/// The hazard that stays is the second case appearing later: a return added
/// ahead of a read turns an undocumented flag into a refused one, and no test
/// can see it coming because it is a control-flow change three files away.
/// Driving the verbs found two already — `peek --source`/`--lines`, which the
/// surface branch returns before reading, and `flag --seen`, which is not read
/// when no id is given — and both are now on the help, where they should have
/// been. That is the shape of the repair every time: one line on the map, not
/// an entry in a table nobody reads.
///
/// After the command has run, for [`Args::dropped`]'s reason: the read tally is
/// only complete once the verb has finished asking. The row wanted the refusal
/// ahead of the act; that is available only to a check with no read tally
/// behind it, and the tally is what makes this one safe.
fn unknown_flags(args: &Args) -> Vec<Unknown> {
    let read = args.read.borrow();
    let mut given: Vec<&str> = args
        .flags
        .keys()
        .map(String::as_str)
        .filter(|n| !read.contains(*n))
        .filter(|n| !GLOBAL_FLAGS.contains(n))
        .filter(|n| !ACCEPTED_UNREAD.iter().any(|(c, f)| *c == args.cmd && f == n))
        .collect();
    if given.is_empty() {
        return Vec::new();
    }
    given.sort_unstable();

    let table = vocabulary();
    let entry = help_entry(&table, args);
    let mut out = Vec::new();
    for name in given {
        // With an entry, the claim is about this verb. Without one — an alias
        // the help spells differently, a subcommand it does not list — the only
        // honest claim left is that no verb anywhere takes the name, which
        // still catches a typo and never refuses a flag some verb does take.
        let known: Vec<&str> = match &entry {
            Some((_, set)) => set.iter().map(String::as_str).collect(),
            None => table.values().flatten().map(String::as_str).collect(),
        };
        if known.contains(&name) {
            continue;
        }
        out.push(Unknown {
            name: name.to_string(),
            verb: entry.as_ref().map(|(k, _)| (*k).to_string()),
            meant: nearest(name, &known),
        });
    }
    out
}

/// The help entry this invocation is answered by.
///
/// `wsp project add` is its own line with its own flags and `wsp show <id>` is
/// not, so the subject is tried as a subcommand first and dropped when the help
/// has no such line. A verb the help *does* split into subcommands — `project`,
/// `worklist`, `panel` — answers for nothing but itself once a subject is
/// given: `panel storyboard` takes flags `panel` does not, and borrowing
/// `panel`'s list would refuse them.
fn help_entry<'a>(
    table: &'a HashMap<String, HashSet<String>>,
    args: &Args,
) -> Option<(&'a str, &'a HashSet<String>)> {
    let found = |k: &str| table.get_key_value(k).map(|(k, v)| (k.as_str(), v));
    if let Some(subject) = args.rest.first() {
        if let Some(hit) = found(&format!("{} {subject}", args.cmd)) {
            return Some(hit);
        }
        let prefix = format!("{} ", args.cmd);
        if table.keys().any(|k| k.starts_with(&prefix)) {
            return None;
        }
    }
    found(&args.cmd)
}

/// The flags the help gives each verb, keyed by the words a caller types.
///
/// Entries are `  wsp <verb>` lines and the indented prose under them, because
/// the help says `--focus` in the paragraph below `wsp spawn` as often as in
/// the usage line above it. A second word is a subcommand only when it is one
/// space along and made of letters, which is what separates `wsp project add`
/// from `wsp brief` and its column of description. Short flags go through
/// [`expand_short`], since that is the name [`Args`] stores.
///
/// Not cached: it is built at most once per process, and only on a run that
/// already has a word nothing read.
fn vocabulary() -> HashMap<String, HashSet<String>> {
    let mut table: HashMap<String, HashSet<String>> = HashMap::new();
    let mut keys: Vec<String> = Vec::new();
    for line in help_text().lines() {
        // Column zero is a section heading or the closing notes, which belong
        // to no verb.
        if !line.starts_with("  ") || line.trim().is_empty() {
            keys.clear();
            continue;
        }
        if let Some(usage) = line.strip_prefix("  wsp ") {
            keys = entry_keys(usage);
            for k in &keys {
                table.entry(k.clone()).or_default();
            }
        }
        if keys.is_empty() {
            continue;
        }
        for f in flags_named(line) {
            for k in &keys {
                table.get_mut(k).expect("the key was just inserted").insert(f.clone());
            }
        }
    }
    table
}

/// `project ls|projects [--tag T] …` is `["project ls", "project projects"]`.
fn entry_keys(usage: &str) -> Vec<String> {
    let word = |w: &str| {
        !w.is_empty()
            && !w.starts_with('-')
            && w.chars().all(|c| c.is_ascii_lowercase() || c == '-' || c == '|')
    };
    let verb = usage.split(' ').next().unwrap_or_default();
    if !word(verb) {
        return Vec::new();
    }
    // One space and then a word: `wsp machine ls`. Two or more spaces is the
    // description column — `wsp brief          what this pane is for`.
    let sub = usage[verb.len()..]
        .strip_prefix(' ')
        .map(|r| r.split(' ').next().unwrap_or_default())
        .filter(|w| word(w));
    let mut out = Vec::new();
    for v in verb.split('|') {
        match sub {
            Some(s) => out.extend(s.split('|').map(|s| format!("{v} {s}"))),
            None => out.push(v.to_string()),
        }
    }
    out
}

/// Every flag name a line of help mentions.
///
/// A dash only starts a flag when the character before it is not part of a
/// word, so `sub-task` and `write-ahead-only` name nothing, and `-ui` in
/// `wsp tag <id> +dsp -ui` is two letters and so not a short flag either.
fn flags_named(line: &str) -> Vec<String> {
    let c: Vec<char> = line.chars().collect();
    let wordish = |ch: char| ch.is_ascii_alphanumeric() || ch == '-';
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] != '-' || (i > 0 && wordish(c[i - 1])) {
            i += 1;
            continue;
        }
        if c.get(i + 1) == Some(&'-') {
            let start = i + 2;
            let mut j = start;
            while j < c.len() && (c[j].is_ascii_lowercase() || c[j].is_ascii_digit() || c[j] == '-')
            {
                j += 1;
            }
            if j > start && c[start].is_ascii_lowercase() {
                let name: String = c[start..j].iter().collect();
                out.push(name.trim_end_matches('-').to_string());
            }
            i = j.max(i + 2);
        } else if c.get(i + 1).is_some_and(char::is_ascii_lowercase)
            && !c.get(i + 2).copied().is_some_and(wordish)
        {
            out.push(expand_short(&c[i + 1].to_string()));
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

/// The flag a misspelling was probably reaching for, within two edits.
///
/// Worth the twenty lines because the refusal arrives *after* the command ran:
/// the caller is being told to run it again, and the whole cost of that is
/// finding the right word.
fn nearest(name: &str, known: &[&str]) -> Option<String> {
    if name.len() < 2 {
        return None;
    }
    known
        .iter()
        .map(|k| (edits(name, k), *k))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, k)| (*d, k.len()))
        .map(|(_, k)| k.to_string())
}

/// Levenshtein distance, one row at a time.
fn edits(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            row[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1);
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[b.len()]
}

fn help() {
    println!("{}", help_text());
}

/// The help as one string, so that the check on flag names can read it.
///
/// The argument for building it rather than printing it is in [`vocabulary`]:
/// this text is the only per-verb declaration of what a verb takes that already
/// exists, is already read by people, and is already checked against the
/// dispatch. Rendering it costs a `format!` of twelve kilobytes, paid only when
/// something on the command line went unread — which is never on a run that
/// spelled everything right.
fn help_text() -> String {
    let p = util::Paint::new();
    let h = |s: &str| p.bold(s);
    format!(
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
                                    the prose; the project you are in and open
                                    work only, unless --all, which also reaches
                                    the archive. It says when the answer is
                                    somewhere you did not look. Stops at 20
                                    hits and says how many more; --full for
                                    all of them
  wsp inbox                         tasks with no project
  wsp show <id> [--log]             full task, including notes. The log is
                                    the last few entries and says how many
                                    earlier ones it did not print; --log for
                                    all of them
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
  wsp archive [--days N] [-n] [--full]
                                    sweep done tasks older than 30d into
                                    archive/tasks/<month>/; --days 0 for every
                                    finished task there is, -n for which ones
                                    first and --full for all of that list

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
  wsp install --why - | --why --from FILE
                                    …with the reason read from a stream or a
                                    file, where a shell never sees it — it is
                                    the sentence the next agent reads off the
                                    lock, and prose typed between double quotes
                                    has every backtick in it run. The binary
                                    stays the positional: `--from` is where the
                                    sentence comes from, not the build
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
                                    `--tell -`, or `--from FILE`: between double
                                    quotes a shell runs every backtick in it, and
                                    the message arrives fluent with the nouns gone
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
  wsp watch [<project>] [<signal>…]  the few facts a governor acts on, as they
                                    become true: needs-a-person, review,
                                    blocked, flag, unanswered, agent-gone, and
                                    seat-stalled — the one whose subject is a
                                    governor rather than a piece of work. No
                                    arguments is this seat's whole scope. It
                                    says what it is watching, one line per
                                    change, a heartbeat while nothing happens,
                                    and why it stopped
  wsp watch --now                   …or the level read on its own: everything
                                    up right now, correct after any restart —
                                    the one call that says "nothing is up"
                                    rather than merely saying nothing
  wsp watch --once                  …or one tick against the last one's ledger,
                                    for a caller that holds no process
  wsp watch --for 2h | --until <id> | --every 30s | --settle 5m
                                    when to stop, how often to look, and how
                                    long a stopped agent must stay stopped
  wsp watch --status [--forget <k>]   who is watching, and whether they still
                                    are; a watch whose process died is a line
                                    in `wsp doctor` rather than a silence
  wsp overlap                       who else is standing in this tree
  wsp attempts [<task|proj>] [--all]  every attempt at that work: the tier it was
                                    spawned at, the tier that actually served it,
                                    how long to review, and whether it came back
  wsp peek [panel|view|board|<task>] [--source recent] [--lines N]
                                    what is on that pane, or the frame the
                                    sidebar surface last drew; --source recent
                                    reaches back through what has scrolled past,
                                    for when the question is what happened
                                    rather than what is showing
  wsp tell <id> "…" | - | --from F  say something to the agent holding that
                                    task, without ending it — `-` reads the
                                    message from stdin, --from from a file. The
                                    repair for an agent whose turn stopped: the
                                    conversation is intact, and a respawn throws
                                    it away.
                                    The same sentence twice inside two minutes
                                    is read as a retry and refused; `--again`
                                    means it

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
  wsp worklist edit <slug> --overview -    what has to be true before group 1
                                    starts — there is no barrier in front of it
                                    to carry a stop condition, so the list does
  wsp worklist ls|show <slug>       every list, or one: the groups, where it is
                                    up to, and which of them may still be edited
  Editing is write-ahead-only: a group at or behind where the list is up to has
  either run or is running, and is refused with what may be edited instead.

  wsp worklist next [<slug>]        what may start now, what is holding it, the
                                    prose to read at a barrier, or nothing left.
                                    No slug when the workspace holds the seat
  wsp worklist go [<slug>] ["…"]    start the list, or pass a barrier: records
                                    the verdict, sweeps the trees of the groups
                                    behind it, and says which members of the
                                    group that just landed touched one file
  wsp worklist hold [<slug>] "why"  start nothing more. What is already running
                                    is left to finish — work in flight cannot
                                    be unwound
  wsp worklist done <slug>          nothing left to want from it
  A barrier with prose at it will not pass until `go` is given a sentence, and
  the sentence is dated onto the group. Nothing spawns: `next` names the members
  and the governor runs `wsp spawn` per member.

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
  wsp flag <id> --from FILE         …with that paragraph out of a file, the
                                    spelling every other prose verb takes
  wsp flag <id> --ask claim         …and a question a keypress answers
  wsp flag [--clear <id>] [--seen <id>] [--seat]
                                    what is raised, and whose it is; --seat
                                    narrows it to this seat's own; --clear lowers
                                    one, and --seen puts the card away and
                                    leaves the hand up
  wsp ask <id> ["…"|-|--from F]     a question about a task, with a return path:
                                    the answer comes back to you and lands on a
                                    task's log. `wsp tell` is still for prose
  wsp ask                           what is open, who is waiting, and how long
  wsp answer <mid> "…"|-|--from F   close one: the log first, then whoever asked
  wsp answer <mid> --abandon "…"    the other ending, and it also goes home
  wsp ack <mid>                     an answer read, or a notification taken on
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
A flag wsp does not know still takes the word after it, so a command that ends
with a value nothing read says so and exits 2 — the word went nowhere. A flag
this page does not give the verb, and that the verb never asked about, is
refused by name and exits 2 the same way.
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
    )
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

    /// The help as it is written, not as it is rendered — the tests that read
    /// the map want the source. `fn help()` is the one-line printer and
    /// `help_text` under it holds the string, so splitting on the printer's
    /// signature reaches both.
    fn help_source() -> &'static str {
        SRC.split("fn help()").nth(1).expect("the help moved")
    }

    #[test]
    fn every_verb_the_binary_answers_to_is_on_the_map() {
        let help = help_source();
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

    /// **The class behind `worklist-036`**: a word taken off the command line
    /// that nothing read is a thing the caller said and wsp did not hear.
    ///
    /// `wsp flag <id> --from FILE` raised a hand with `"text": ""` and exited 0
    /// because `flag` had no `--from` and an unknown flag still eats the token
    /// after it. Unattended that is a night of empty hands and no failure
    /// anywhere. Asserted on both directions, because the reason this is a
    /// dropped *word* and not an unknown *name* is the second one: a flag that
    /// stands for itself takes nothing, so a verb that never reads it has lost
    /// nothing, and `--no-focus` — parsed on purpose and read by nobody — goes
    /// on costing nothing.
    #[test]
    fn a_word_taken_off_the_line_that_nothing_read_is_reported() {
        use super::Args;
        let parse = |line: &[&str]| Args::parse(line.iter().map(|s| (*s).to_string()).collect());

        let a = parse(&["flag", "acc-005", "--form", "finding.txt"]);
        assert_eq!(a.dropped(), vec!["form"], "the path went nowhere and nothing said so");
        assert_eq!(super::misheard(&a, 0), 2, "and the exit code carried it");

        // Read is read, however it was read.
        let a = parse(&["flag", "acc-005", "--from", "finding.txt"]);
        assert_eq!(a.get("from").as_deref(), Some("finding.txt"));
        assert!(a.dropped().is_empty(), "a flag the verb read is not dropped");

        // A flag that took no word costs nothing when nobody reads it, which is
        // what keeps `spawn --no-focus` parsing and free.
        let a = parse(&["spawn", "wsp-001", "--no-focus"]);
        assert!(a.dropped().is_empty());
        let a = parse(&["ls", "-p", "acc", "--wibble"]);
        assert_eq!(a.get("project").as_deref(), Some("acc"));
        assert!(a.dropped().is_empty(), "a bare unknown flag ate nothing");

        // `--name=Reverb Lab` took a word too — the `=` is a spelling, not a
        // different act.
        let a = parse(&["project", "add", "verb", "--nmae=Reverb Lab"]);
        assert_eq!(a.dropped(), vec!["nmae"]);

        // A command that already failed is left alone entirely: it has said why
        // in its own words, and a verb that stopped early may simply not have
        // reached the flag it does read.
        assert_eq!(super::misheard(&a, 1), 1);

        // A command line one command builds for another never met a shell, so
        // there is nothing on it to have been dropped.
        let a = Args::synth("flag", &["acc-005"], &[("from", "finding.txt")]);
        assert!(a.dropped().is_empty());
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

    /// The half of `worklist-036` that a read tally alone cannot reach: a flag
    /// that stands for itself, drops no word, and is not the flag it was meant
    /// to be. `--al` is `--all` mistyped, and it went by in silence.
    #[test]
    fn a_flag_the_verb_does_not_take_is_refused_by_name() {
        let a = args(&["ls", "--al"]);
        let u = super::unknown_flags(&a);
        assert_eq!(u.len(), 1, "a mistyped flag went by");
        assert_eq!(u[0].name, "al");
        assert_eq!(u[0].verb.as_deref(), Some("ls"), "the verdict named the wrong entry");
        assert_eq!(u[0].meant.as_deref(), Some("all"), "the near miss was not offered");
        assert_eq!(super::misheard(&a, 0), 2, "and the exit code did not carry it");
    }

    /// A real flag on the wrong verb, which is the case a spell-check misses:
    /// `--seat` is a flag, it is spelled right, and `ls` has never taken it.
    #[test]
    fn a_flag_of_another_verb_is_refused_on_this_one() {
        let a = args(&["ls", "--seat"]);
        let u = super::unknown_flags(&a);
        assert_eq!(u.len(), 1, "a flag borrowed from another verb was accepted");
        assert_eq!(u[0].verb.as_deref(), Some("ls"));
    }

    /// The refusal is never about the help alone. Twenty-odd flags are real and
    /// undocumented — `--socket`, `--payload`, `--ratio` — and the tally of
    /// what the verb *asked about* is what keeps them working. Whether they
    /// were given is beside the point: `Args::mark` records the ask.
    #[test]
    fn a_flag_the_verb_asked_about_is_never_refused() {
        let a = args(&["panel", "install", "--ratio", "0.3"]);
        assert!(!super::unknown_flags(&a).is_empty(), "the help does not list --ratio");
        a.get("ratio");
        assert!(super::unknown_flags(&a).is_empty(), "a flag the verb read was still refused");
    }

    /// `--no-focus` asks for what already happens and nothing reads it, which
    /// is the exact shape the row said this check would break. It is in
    /// `ACCEPTED_UNREAD` and it goes on parsing.
    #[test]
    fn a_flag_kept_only_for_compatibility_is_still_accepted() {
        assert!(super::unknown_flags(&args(&["spawn", "t-1", "--no-focus"])).is_empty());
        // And only on the verb that keeps it. Anywhere else it is a word that
        // means nothing, which is the honest answer.
        assert!(!super::unknown_flags(&args(&["ls", "--no-focus"])).is_empty());
    }

    /// The net under the whole thing: every usage line the help prints has to
    /// survive the check that is read off it. It is the same document twice,
    /// which is the point — what it can still catch is the *reading* going
    /// wrong: a continuation paragraph landing on the next verb's entry, a
    /// subcommand mistaken for a description column, a short flag not expanded
    /// to the name `Args` stores. Any of those refuses a command the help
    /// documents, and this is how that is heard at build time rather than by
    /// somebody typing it.
    #[test]
    fn every_flag_the_help_documents_is_accepted_by_the_verb_it_documents_it_for() {
        let mut lines = 0;
        for line in help_source().lines() {
            let Some(usage) = line.strip_prefix("  wsp ") else { continue };
            let keys = super::entry_keys(usage);
            let Some(key) = keys.first() else { continue };
            let flags = super::flags_named(line);
            if flags.is_empty() {
                continue;
            }
            lines += 1;
            let mut argv: Vec<String> = key.split(' ').map(str::to_string).collect();
            // A value, so the flag is not left standing at the end of the line
            // where `scan` would read the next flag as its argument.
            argv.extend(flags.iter().flat_map(|f| [format!("--{f}"), "x".into()]));
            let a = super::Args::parse(argv);
            let refused: Vec<String> =
                super::unknown_flags(&a).into_iter().map(|u| u.name).collect();
            assert!(refused.is_empty(), "`wsp {usage}` would be refused its own {refused:?}");
        }
        assert!(lines > 40, "the help parse found flags on only {lines} lines");
    }

    /// The other direction of `every_verb_the_binary_answers_to_is_on_the_map`,
    /// and the one that catches the reading rather than the writing. Every
    /// entry the vocabulary builds has to be a verb the binary answers to, or
    /// the parse has invented a command out of a description column — and an
    /// invented entry answers for a real invocation with the wrong list.
    #[test]
    fn every_entry_read_off_the_help_is_a_verb_the_binary_answers_to() {
        let arms: Vec<String> = dispatch().into_iter().flatten().collect();
        let table = super::vocabulary();
        assert!(table.len() > 60, "the help parse found only {} entries", table.len());
        let invented: Vec<&String> = table
            .keys()
            .filter(|k| !arms.contains(&k.split(' ').next().unwrap_or_default().to_string()))
            .collect();
        assert!(invented.is_empty(), "entries no verb answers to: {invented:?}");
    }

    /// A verb the help splits into subcommands answers for its subcommands and
    /// for nothing else. `panel storyboard` takes flags `panel` does not, and
    /// borrowing `panel`'s list would refuse them; `show <id>` is not a
    /// subcommand at all and must still be answered by `show`.
    #[test]
    fn a_subject_is_a_subcommand_only_where_the_help_says_so() {
        let table = super::vocabulary();
        let key = |argv: &[&str]| {
            let a = super::Args::parse(argv.iter().map(|s| (*s).to_string()).collect());
            super::help_entry(&table, &a).map(|(k, _)| k.to_string())
        };
        assert_eq!(key(&["project", "add", "x"]).as_deref(), Some("project add"));
        assert_eq!(key(&["show", "worklist-038"]).as_deref(), Some("show"));
        assert_eq!(key(&["panel"]).as_deref(), Some("panel"));
        assert_eq!(key(&["panel", "storyboard"]), None, "storyboard borrowed panel's flags");
    }

    /// An alias the help spells differently — `list` for `ls` — has no entry,
    /// and the check drops to the only claim it can still make honestly: no
    /// verb anywhere takes this name. It still catches the typo and it never
    /// refuses a flag that is real somewhere.
    #[test]
    fn an_alias_the_help_does_not_spell_is_still_spell_checked() {
        let a = args(&["list", "--al"]);
        let u = super::unknown_flags(&a);
        assert_eq!(u.len(), 1);
        assert!(u[0].verb.is_none(), "it claimed to know what `list` takes");
        assert_eq!(u[0].meant.as_deref(), Some("all"));
        // `--seat` is no flag of `ls`, but the fallback cannot say so.
        assert!(super::unknown_flags(&args(&["list", "--seat"])).is_empty());
    }

    /// Both halves report, and a flag that is both is one message. "There is no
    /// such flag" says everything the dropped word would have, and saying both
    /// about one word reads as two faults.
    #[test]
    fn a_word_lost_to_a_flag_that_does_not_exist_is_reported_once() {
        let a = args(&["flag", "wsp-1", "--form", "/tmp/x"]);
        let u = super::unknown_flags(&a);
        assert_eq!(u.len(), 1, "the unknown flag was not named");
        assert_eq!(u[0].meant.as_deref(), Some("from"));
        assert_eq!(a.dropped(), vec!["form"], "and it did take the path with it");
    }

    fn args(argv: &[&str]) -> super::Args {
        super::Args::parse(argv.iter().map(|s| (*s).to_string()).collect())
    }
}
