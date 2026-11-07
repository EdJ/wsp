//! wsp — workspace and task control plane for herdr.
//!
//! Durable facts (projects, tags, tasks) live in `~/wsp` as Markdown + git.
//! Live facts (panes, agent status) come from herdr's socket. This binary
//! joins them, and pushes the join back into herdr's sidebar as metadata
//! tokens.

use std::collections::{HashMap, HashSet};

mod affected;
pub(crate) use wsp_trees::{at_stop, check_tree, cmd_checkout, cmd_verify, gc, greens, guard, overlap, throttle, treecopy, warm};
mod base;
pub(crate) use wsp_base::{choices, input, risk, util};
pub(crate) use wsp_store::{agent_kind, caption, checks, decisions, governors, lapse, log, message, model, resolve, sharing, store, tags, verbs};
pub(crate) use wsp_wire::{detect_override, fake, far, herdr, launchd, place, place_compound, place_herdr, place_super, tunnel};
mod artefact;
mod registry;
mod after;
mod closing;
mod packet;
mod pickup;
mod packet_gen;
mod verifier_packet;
mod agent_commands;
mod barrier;
mod barrier_replay;
mod barrier_run;
mod cold;
mod hold;
mod hold_close;
#[cfg(test)]
mod testkit;
mod check;
mod build_output;
mod scope;
mod scratch;
mod leaked_agents;
mod disk;
mod arrange;
mod attention;
mod cmd_agent;
mod cmd_attempts;
mod cmd_brief;
mod cmd_burn;
mod cmd_cost;
mod cmd_report;
mod cmd_role;
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
mod cmd_stamp;
mod cmd_task;
mod cmd_verifications;
mod leg_seat;
mod leg_tree;
mod legs;
mod verification;
mod cmd_watch;
mod wake;
mod cmd_worklist;
mod cycle;
mod daemon;
mod detail;
mod did;
mod draw;
mod ending;
mod history;
mod kanban;
mod live;
mod kind_data;
mod kind_attention;
mod cmd_attend;
mod memory;
mod panel;
mod repair;
mod retro;
mod retro_run;
mod retro_series;
mod retro_yours;
mod room;
mod split;
mod stage;
mod starts;
mod stand_down;
mod story;
mod stuck;
mod sync;
mod timeline;
mod wallbuckets;
mod untaken;
mod waiting;
mod waits;
mod worklist;

pub use base::args::*;
pub use base::stamp::*;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");



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

/// The verbs that look first, in the words a refusal names them by.
///
/// Paired with [`dry_run`] by a test rather than derived from it: the list is
/// read by somebody who has just been refused and needs the nearest verb that
/// would have answered, and generating it from the match would print
/// `checkout` for the arm that also covers `--rm`, which is the sentence they
/// need. It is short because the property it describes is rare, and it going
/// stale is the one failure here that costs nothing but a wrong signpost.
const LOOKS_FIRST: &str =
    "archive, checkout, install, migrate, project rm, sandbox rm, scratch, verify --rm, worklist go, worklist group --drop, worklist rm";

/// Whether this invocation reads `-n`, and what to call it if it does not.
///
/// # Why a check before dispatch, when [`unknown_flags`] is after it
///
/// `-n` expands to `--dry-run` for **every** verb in [`expand_short`], and
/// until `worklist-050` five verbs read it and the other thirty-odd let it
/// parse, ignored it, and did what they were going to do. On a reading verb
/// that is noise the tally cleans up afterwards. On `wsp checkout <id> --rm -n`
/// it was the fault `worklist-044` was written about, arriving four more times:
/// **the word that means "show me first" is the word that does it anyway**, and
/// the only notice comes once the tree is gone.
///
/// [`unknown_flags`] cannot be that notice and says so in its own last
/// paragraph — it is a read tally, and a tally is only complete once the verb
/// has finished asking. The refusal has to arrive before the act, which is
/// available only to a check with **no read tally behind it**. This is that
/// check, and it is possible for exactly one word: `--dry-run` is the only flag
/// in wsp whose meaning does not vary by verb. Every other flag means what its
/// verb decides it means, which is why the vocabulary for those had to be read
/// off the help and consulted afterwards. *Do not do it, tell me what you would
/// do* needs no vocabulary — it needs one list of who honours it.
///
/// # Why a match and not a table
///
/// This is the shape of `main`'s own dispatch, ten lines below, aliases and
/// subcommands and all — a table would be that shape written a second time in a
/// different notation and would drift from it in the same edit. And it is one
/// flag over forty verbs rather than sixty flags, which is the size that made
/// [`Args::dropped`] refuse a vocabulary.
///
/// **Both ways of being wrong are safe, and that is the argument for keeping it
/// by hand.** A verb missing from here refuses a command that would have worked
/// and says so before doing anything. A verb wrongly here reads nothing, and
/// falls back to exactly the after-the-fact tally of today — because the check
/// asks through [`Args::given`] and never marks the word read.
///
/// The naming is folded in rather than computed beside it for the same reason:
/// the arms that dispatch on a subcommand are the arms whose refusal has to say
/// `worklist rm` rather than `worklist`, and that is one fact about a verb, not
/// two.
///
/// # Which removing verbs are on this list, and why the rest are not
///
/// `worklist-050` made every removing verb *safe* with one check and then gave
/// four of them a real dry run; `worklist-051` settled the remaining four, and
/// split them two and two. The line it drew is not about how much damage a verb
/// does — the two that stayed refused include the only verb in wsp that deletes
/// a record outright, and the two that got a preview are among the least
/// destructive there are. It is about **whether the caller can enumerate what
/// goes**:
///
/// - **A wildcard or a cascade earns a preview.** `sandbox rm --all` and
///   `verify --rm --all` match a set nobody typed; `project rm --force` and
///   `worklist rm` take things *with* the one you named — orphaned tasks, a
///   group emptied and dropped. In every one of these the removal computes a
///   set the caller did not write down and no other verb prints, so `-n` is the
///   only place that set is ever said out loud. `checkout --rm` is the same
///   shape one step in: the named thing is the tree, and the *branch* is the
///   consequence it computes.
/// - **One named thing that comes back is refused, and signposted.** `wsp rm`
///   and `wsp machine rm` remove exactly the record you typed, into an archive
///   or into a commit that git still has. Their whole preview is a `show`,
///   which exists — so what the refusal owes is the name of it, and
///   [`refuse_dry_run`] pays that. A dry run here would be a second `show`
///   living inside the verb that removes, kept in step with it by nothing.
/// - **A set that is not knowable ahead of the act is refused with the reason
///   on the verb.** `despawn` alone, and its argument is in
///   [`crate::cmd_spawn::despawn`]: every step is conditioned on herdr's answer
///   to the last one, so a preview is a report of what would be *attempted* —
///   which is a different sentence from what would happen, on exactly the runs
///   that matter.
///
/// The first bullet is why the arms below are not a list of dangerous verbs.
/// `wsp worklist rm` is on it and cannot lose a byte; `wsp machine rm --force`
/// is off it and deletes a file. What the flag is worth is a function of what
/// the caller cannot otherwise see.
fn dry_run(args: &Args) -> (bool, String) {
    let sub = args.rest.first().map(String::as_str).unwrap_or_default();
    let named = |reads: bool| (reads, format!("{} {sub}", args.cmd).trim_end().to_string());
    match args.cmd.as_str() {
        // `checkout` is here whole, and all three of its branches read the
        // word: the sweep always did, `--rm` does now, and so does making a
        // tree. Leaving the making branch out would put the same defect back
        // one branch along — the help says `checkout` takes `-n`, so nothing
        // downstream would have caught it either.
        "migrate" | "install" | "archive" | "checkout" => (true, args.cmd.clone()),
        "project" | "proj" | "p" => named(matches!(sub, "rm" | "remove" | "delete")),
        "sandbox" => named(matches!(sub, "rm" | "remove" | "stop")),
        "scratch" => (true, args.cmd.clone()),
        // `group` the way `verify` is: only its removing branch, `--drop`,
        // has anything to look at first.
        "worklist" | "wl" => {
            named(matches!(sub, "go" | "start" | "rm" | "remove") || (sub == "group" && args.given("drop")))
        }
        // The one arm that turns on a flag rather than a word, because that is
        // where `verify` keeps the difference: `wsp verify` is a build and has
        // nothing to look at first, `wsp verify --rm` removes trees nothing
        // else in wsp can list. Asked through `given` like the check itself —
        // reading `--rm` here would tell the tally somebody had looked at it,
        // and on `wsp add --rm` (which is not a verify) that is a word going
        // silently nowhere.
        "verify" => (args.given("rm"), args.cmd.clone()),
        "machine" | "machines" => named(false),
        "agent" => named(true),
        _ => (false, args.cmd.clone()),
    }
}

/// A flag typed into the payload of a [`LITERAL_AFTER`] verb, which is about
/// to be written down as the payload.
///
/// # The defect
///
/// `wsp note <id> --body -` exited 0, printed the ordinary receipt, and left
/// `- 2026-08-23 --body -` in the log. Same across `decide`, `park`, `block`,
/// `rename` and very nearly `tag`. The paragraph on stdin was read by nobody
/// and the caller was told it had worked, which is how `robustness-099`'s
/// review note stopped existing. It is not a slip the caller could have
/// caught: the project handbook told every agent that `--body -` was a way to
/// give a verb its prose.
///
/// # Why here, and why it is not [`unknown_flags`]
///
/// [`unknown_flags`] is the general answer and it never sees this one. Past
/// the subject of a listed verb, [`Args::scan`] has already decided a
/// `--`-led token is prose, so it is a positional and never enters the flag
/// map at all — there is nothing for a read tally to be missing. The two
/// rules meet exactly here: the rule that keeps `wsp note 028 "--parent is
/// add-only"` a sentence is the rule that swallows a mistyped flag whole.
///
/// That also makes the claim this check needs airtight, which the general one
/// could not manage without reading the help. Past the subject only
/// [`GLOBAL_FLAGS`] and [`OWNED_AFTER`] are parsed, so a `--`-led word in the
/// payload is unreachable to every line of code in the binary. No verb can be
/// reading it, documented or not, and there is no vocabulary to consult.
///
/// And it runs before the store is opened, where [`unknown_flags`] cannot: a
/// tally is only complete once the verb has finished asking, so its refusal
/// arrives after the record has been written. `wsp add "t" --body -` shows
/// what that is worth — exit 2, the flag named, and a task already created
/// with `-` on the end of its title.
///
/// # What counts
///
/// The payload *entire*, and nothing less: one `--`-led word, optionally with
/// one word after it or an `=value` inside it. That is the same line
/// `cmd_task::payload_source` draws for `--from`, and for the same reason —
/// prose here is mostly about the CLI, so a payload that merely *begins* with
/// a flag is a sentence somebody meant. A token holding a space is prose
/// whatever it starts with, which is the whitespace rule in [`Args::scan`]
/// read once more.
///
/// Three things are therefore still text and still work: a sentence
/// (`wsp note 028 "--parent is add-only"`), the tag removal syntax
/// (`wsp tag <id> -ui`, one dash), and anything at all after `--`.
fn swallowed_flag(args: &Args) -> Option<(&'static Literal, String, Option<String>)> {
    // The caller said these words are the text. That is the whole meaning of
    // `--` and it is what the help sends people to.
    if args.escaped {
        return None;
    }
    let l = literal(&args.cmd)?;
    let (head, value) = match args.rest.get(l.subject..)? {
        [one] => (one.as_str(), None),
        [one, two] => (one.as_str(), Some(two.clone())),
        _ => return None,
    };
    let body = head.strip_prefix("--")?;
    if body.is_empty() || body.contains(char::is_whitespace) {
        return None;
    }
    let name = body.split_once('=').map_or(body, |(n, _)| n);
    if l.stream && name == "from" {
        return None;
    }
    Some((l, name.to_string(), value))
}

/// Say what the word would have become, before it becomes it.
///
/// Two sentences the caller cannot get anywhere else. **What it was about to
/// write**, because the failure this replaces was a success — the receipt
/// looked right and the log had to be re-read to find the damage. And
/// **nothing was read from stdin**, because the caller with a paragraph in a
/// pipe needs to know whether they still have it; the refusal happens before
/// the store is open, so the pipe is untouched and the file behind it is
/// exactly where it was.
///
/// Only said when there is a pipe to reassure about. On a terminal it would be
/// a line about a hazard the caller was never in.
fn refuse_swallowed(l: &Literal, name: &str, value: Option<&str>) -> i32 {
    let p = util::Paint::new();
    let typed = match value {
        Some(v) => format!("--{name} {v}"),
        None => format!("--{name}"),
    };
    eprintln!(
        "wsp: `wsp {}` has no {}, so `{typed}` was about to be recorded as the",
        l.cmd,
        p.bold(&format!("--{name}")),
    );
    eprint!("     {}. Nothing was written", l.payload);
    if util::stdin_is_tty() {
        eprintln!(".");
    } else {
        eprintln!(", and nothing was read from stdin.");
    }
    if l.stream {
        eprintln!(
            "     A paragraph goes in by `wsp {} <id> --from -` or a bare `-`; `--` first",
            l.cmd
        );
        eprintln!("     records a flag-shaped word as the text.");
    } else {
        eprintln!("     `--` first records a flag-shaped word as the text.");
    }
    2
}

/// Say that `-n` is not read here, before anything has happened.
///
/// The one line that matters is *nothing has been done*, because the reader
/// typed `-n` precisely to find out what would be, and every previous version
/// of this sentence arrived after the answer had been acted on.
///
/// # Why three verbs get a paragraph and the rest get a list
///
/// A refusal on a verb that removes something is read by somebody who wanted an
/// answer, and the list at the bottom does not give them one — it names the
/// nearest verbs that look first, which for `wsp rm` is none of them. So the
/// three removing verbs that stay refused after `worklist-051` each carry **the
/// read that does answer the question they were asked**, and for two of the
/// three that read is a plain `show`. That is the argument for refusing them
/// rather than the apology for it: a preview here would be a second and worse
/// `wsp show`, built inside the verb that removes, and kept in step with it by
/// nothing.
///
/// `despawn` is the exception in kind rather than in shape. Its preview would
/// not be redundant, it would be *wrong* — see [`crate::cmd_spawn::despawn`] —
/// so what it names is the read for the half of the question that is a
/// directory, and it says why the other half has no answer.
fn refuse_dry_run(verb: &str) -> i32 {
    let p = util::Paint::new();
    eprintln!("wsp: `wsp {verb}` does not look first, so {} is refused. Nothing has been done.", p.bold("-n"));
    for line in signpost(verb) {
        eprintln!("     {line}");
    }
    eprintln!("     Verbs that do look first: {}", p.dim(LOOKS_FIRST));
    2
}

/// What a refused removing verb is told instead of a preview.
///
/// Held apart from [`refuse_dry_run`] so it can be read as data. The claim
/// these lines make is not about wording — it is that every verb still refusing
/// `-n` after `worklist-051` names a read that *does* answer it, and that claim
/// is checkable only if the sentences are values rather than `eprintln!`s
/// halfway down a function. The test asks two things of each: that there is an
/// entry at all, and that the verb it points at is one that exists.
///
/// Empty for the rest, which is the honest answer for `wsp ls -n`: there is no
/// preview to send anybody to, because there is nothing to preview.
fn signpost(verb: &str) -> &'static [&'static str] {
    // The verbs that removed things on this word are the reason the refusal
    // exists, so each of the ones still refused names the read that answers
    // what they were asked, rather than leaving it to be looked up.
    match verb {
        "despawn" => &[
            "Ending an agent is a run of steps across herdr, each one decided by",
            "the last one's answer, so what it would do is not knowable until it",
            "does it. For the half that is a directory: `wsp checkout <id> --rm -n`.",
        ],
        "rm" => &[
            "The task goes to the archive and comes back; what goes quietly is the",
            "claim on it and every pane bound to it. `wsp show <id>` names both,",
            "which is the whole of what a dry run here could have told you.",
        ],
        "machine rm" => &[
            "Without --force this retires the machine and is reversible. With it the",
            "record is deleted — and committed, so the store's git has it. There is",
            "one record and no cascade: `wsp machine show <name>` is all of it.",
        ],
        _ => &[],
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
    if matches!(args.cmd.as_str(), "init" | "doctor" | "throttle") {
        return false;
    }
    if args.cmd == "agent" && matches!(args.rest.first().map(String::as_str), Some("kinds" | "pi-extension")) {
        return false;
    }
    !(args.cmd == "panel" && args.rest.first().map(String::as_str) == Some("storyboard"))
}

fn main() {
    die_on_broken_pipe();
    let _ = store::BUILD_STAMP.set(base::stamp::build_stamp);
    cmd_checkout::install_engine(cmd_checkout::Engine {
        passed: worklist::passed_by_running,
        poke_task: cycle::poke_task,
        runs: |store, task| worklist::Running::read(store).of(task).is_some_and(|p| p.run),
        after_land: check::after_land,
        rerun: check::rerun,
        held: |store| cmd_agent::locate(&cmd_agent::Whereabouts::live(store)).task.map(|t| t.id),
    });

    place::install_above(place::Above {
        command: agent_commands::command,
        closes: hold_close::closes,
        heard: hold_close::heard,
        hold: hold::hold,
        hold_secs: hold::SECS,
    });

    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Before parsing: what follows `run` is the build tool's own command line,
    // flags and all, and `Args` would take `--release` for one of ours.
    if let [verb, run, rest @ ..] = argv.as_slice() {
        if verb == "throttle" && run == "run" {
            std::process::exit(throttle::run_raw(rest));
        }
    }
    let args = Args::parse(argv);

    if args.has("version") || args.cmd == "version" {
        println!("wsp {}", version());
        return;
    }
    if args.cmd.is_empty() || args.cmd == "help" || args.has("help") {
        help();
        return;
    }

    // Before the store is even opened, which is what "before the act" has to
    // mean for a word whose whole content is "do not act". See [`dry_run`].
    if args.given("dry-run") {
        let (reads, verb) = dry_run(&args);
        if !reads {
            std::process::exit(refuse_dry_run(&verb));
        }
    }

    // Beside the check above and for its reason: a word that is about to be
    // written into the record has to be refused before the record is opened.
    // See [`swallowed_flag`].
    if let Some((l, name, value)) = swallowed_flag(&args) {
        std::process::exit(refuse_swallowed(l, &name, value.as_deref()));
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

    // A call the executor's shim forwarded: its tree verbs sync the agent's work
    // back first and run on the seat's tree; checkout and land are refused.
    match treecopy::from_executor(&store, &args.cmd) {
        Some(Err(why)) => {
            eprintln!("wsp: {why}");
            std::process::exit(1);
        }
        Some(Ok(tree)) => {
            if let Err(e) = std::env::set_current_dir(&tree) {
                eprintln!("wsp: cannot enter {}: {e}", util::contract(&tree));
                std::process::exit(1);
            }
        }
        None => {}
    }

    let code = match registry::find(&args.cmd) {
        Some(verb) => (verb.run)(&store, &args),
        None => {
            eprintln!("wsp: unknown command `{}`. Try `wsp help`.", args.cmd);
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
    // Asked rather than typed out. A consumer that cannot discover the
    // words is back to guessing from prose, and a list written here by
    // hand is a sixth class away from telling them something false.
    let classes = cmd_watch::Class::every().map(|c| c.word()).join(", ");
    format!(
        "{} {} — workspace and task control plane for herdr\n\n{}\n\n{}",
        p.bold("wsp"),
        version(),
        registry::help_sections(&|s| p.bold(s), &classes),
        HELP_NOTES,
    )
}

/// What closes the help: not about any one verb, so no module owns it.
const HELP_NOTES: &str = r#"Ids are `<project>-NNN`, continuous within a project rather than within a day,
and a task filed nowhere is `inbox-NNN` until `wsp mv -p` files it — the one
place an id changes, and it is recorded so the old one still resolves.
Ids accept a bare suffix (003) or a unique title substring; a suffix that names
more than one task now lists them rather than answering "no such task".
Text that starts with a flag is text: `wsp note <id> "--parent is add-only"` and
`wsp tag <id> +dsp -ui` both mean what they say. A payload that is nothing but a
flag-shaped word reads the other way — a flag the verb has not got — and is
refused before anything is written; `--` first says you meant the words.
A flag wsp does not know still takes the word after it, so a command that ends
with a value nothing read says so and exits 2 — the word went nowhere. A flag
this page does not give the verb, and that the verb never asked about, is
refused by name and exits 2 the same way.
Every command takes --json. Set WSP_HOME to relocate the store.
--terse, or WSP_TERSE=1 for a whole session, leaves out what you already have:
the rules in `brief`, the blocked list in `wip`. Each halves; each says so."#;

#[cfg(test)]
mod tests {
    /// `wsp reopen <id> --no-seat "reason"` with the flag first: valued, it
    /// would eat the reason and the reopen would refuse for want of one.
    #[test]
    fn no_seat_on_reopen_takes_no_value() {
        use crate::Args;
        let args = Args::parse(["reopen", "m-1", "--no-seat", "what is owed"].iter().map(|s| (*s).to_string()).collect());
        assert!(args.has("no-seat"));
        assert_eq!(args.rest, ["m-1", "what is owed"], "the reason stays a positional");
    }

    /// `status` is a mode word on `watch` and a filter value everywhere else
    /// it is read, and [`BOOL_FLAGS`] could only say the first half. So
    /// `wsp ls -p fork -s done` parsed as a bare flag plus a stray positional:
    /// the documented filter silently ignored, and — on `project add`, which
    /// stores what it is given — the literal word `true` written into a
    /// project's status. The inline form kept working throughout, which is why
    /// nobody retested the space form that bit them.
    #[test]
    fn a_status_filter_takes_its_value_on_the_verbs_that_read_one() {
        use crate::Args;
        let parse = |line: &[&str]| Args::parse(line.iter().map(|s| (*s).to_string()).collect());

        // The verbs that filter by it get the value they were typed.
        let args = parse(&["ls", "-p", "fork", "-s", "done"]);
        assert_eq!(args.get("status").as_deref(), Some("done"));
        assert_eq!(args.get("project").as_deref(), Some("fork"), "and nothing around it was eaten");
        let long = parse(&["find", "--status", "review"]);
        assert_eq!(long.get("status").as_deref(), Some("review"));
        assert!(long.rest.is_empty(), "the value went to the flag, not to the positionals");
        let add = parse(&["add", "a title", "--status", "review"]);
        assert_eq!(add.get("status").as_deref(), Some("review"));
        assert_eq!(add.rest.first().map(String::as_str), Some("a title"), "the subject stays the subject");
        let proj = parse(&["project", "add", "x", "--status", "done"]);
        assert_eq!(proj.get("status").as_deref(), Some("done"));

        // And watch keeps its mode: bare, or followed by other flags, it still
        // stands alone and takes nothing.
        let bare = parse(&["watch", "--status"]);
        assert_eq!(bare.get("status").as_deref(), Some("true"));
        let guarded = parse(&["watch", "--once", "--status"]);
        assert_eq!(guarded.get("status").as_deref(), Some("true"));
        assert!(!guarded.has("once-value"), "no flag grew a second name");
    }

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
    fn dispatch() -> Vec<Vec<String>> {
        crate::registry::ALL
            .iter()
            .flat_map(|vs| vs.iter())
            .map(|v| v.names.iter().map(|n| (*n).to_string()).collect())
            .collect()
    }

    /// The help as it is rendered: the map is assembled from the verbs, so
    /// reading it is reading what the registry says.
    fn help_source() -> String {
        super::help_text()
    }

    /// `wsp help` is the same bytes it was before the verbs carried their own
    /// help — everything after the version line, which names the build.
    #[test]
    fn the_help_is_what_it_was_before_the_registry() {
        let help = super::help_text();
        let (_, rest) = help.split_once('\n').expect("a first line");
        assert_eq!(rest, include_str!("../fixtures/help.txt").trim_end_matches('\n'));
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

        // `review` most of all: an account of work on this CLI is a paragraph
        // whose first word is a flag name about as often as any note here is.
        for verb in ["block", "park", "decide", "rename", "review"] {
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

    /// The other end of that rule: a payload that is *only* a flag is refused.
    ///
    /// `wsp note <id> --body -` exited 0 and left `- 2026-08-23 --body -` in
    /// the log while the paragraph on stdin went unread — the same across
    /// `decide`, `park`, `block` and `rename`, because the rule above had
    /// already decided the token was prose. Asserted on
    /// [`super::swallowed_flag`] rather than through the verbs because the
    /// point of the check is that it answers before any of them is reached.
    #[test]
    fn a_payload_that_is_only_a_flag_is_refused_rather_than_recorded() {
        use super::Args;
        let parse = |line: &[&str]| Args::parse(line.iter().map(|s| (*s).to_string()).collect());
        let caught = |line: &[&str]| super::swallowed_flag(&parse(line)).map(|(l, n, v)| (l.cmd, n, v));

        // Every verb on the list, in the spelling the handbook taught. The
        // subject comes off the row rather than being written in: `say` speaks
        // for the pane it runs in and takes none, and a line with an id in it
        // would be three positionals to the check rather than two.
        for l in super::LITERAL_AFTER {
            let mut line = vec![l.cmd];
            line.extend(std::iter::repeat("028").take(l.subject));
            line.extend(["--body", "-"]);
            assert_eq!(
                caught(&line),
                Some((l.cmd, "body".to_string(), Some("-".to_string()))),
                "{} recorded the flag as its {}",
                l.cmd,
                l.payload,
            );
        }
        // Both other shapes of a flag standing alone.
        assert_eq!(caught(&["note", "028", "--body"]), Some(("note", "body".into(), None)));
        assert_eq!(caught(&["note", "028", "--body=x"]), Some(("note", "body".into(), None)));

        // What must go on being text. A sentence that merely begins with a
        // flag is the case `LITERAL_AFTER` exists for, and the whitespace rule
        // is what tells it from a flag however many words follow.
        assert_eq!(caught(&["note", "028", "--parent is add-only"]), None);
        assert_eq!(caught(&["note", "028", "--parent", "is", "add-only"]), None);
        // One dash is the tag removal syntax, and a bare `-` is the stream.
        assert_eq!(caught(&["tag", "028", "-ui"]), None);
        assert_eq!(caught(&["note", "028", "-"]), None);
        // `--` is the escape hatch, and it has to survive reaching the payload
        // as the very tokens the check refuses without it.
        assert_eq!(caught(&["note", "028", "--", "--body", "-"]), None);
        // A verb not on the list parses `--body` as a flag, where the read
        // tally in `unknown_flags` is what answers for it.
        assert_eq!(caught(&["add", "a title", "--body", "-"]), None);

        // `--from` is read out of the payload by the four prose verbs, so it
        // is the one name they must not refuse — and the two with no stream
        // form still do.
        for l in super::LITERAL_AFTER {
            let got = caught(&[l.cmd, "028", "--from", "-"]);
            assert_eq!(got.is_none(), l.stream, "{} answered wrongly for --from", l.cmd);
        }
        assert_eq!(caught(&["note", "028", "--from"]), None);
        assert_eq!(caught(&["note", "028", "--from=/tmp/x.md"]), None);
        // …and nothing else is exempted by being near it.
        assert!(caught(&["note", "028", "--form", "-"]).is_some());
    }

    /// `wsp flag --person --json` is what compound's hand asks (`wsp-272`):
    /// `person` is a global boolean, so it eats no task id and the verb lists.
    #[test]
    fn flag_person_lists_rather_than_raising() {
        let a = super::Args::parse(["flag", "--person", "--json"].iter().map(|s| (*s).to_string()).collect());
        assert!(a.has("person") && a.json());
        assert!(a.rest.is_empty(), "no task named, so it lists: {:?}", a.rest);
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
        // `wsp-233`: the person's flags sit before the sentence, which they
        // must not eat — a question refused for want of the words it was given.
        let a = parse(&["ask", "compound-392", "--person", "keep the VERSION bump?"]);
        assert!(a.has("person") && a.text(1) == "keep the VERSION bump?");
        let a = parse(&["answer", "m-1", "--as-person", "keep it"]);
        assert!(a.has("as-person") && a.text(1) == "keep it");

        // `wsp-299`: a lone `-` is the stream for `--choices` and `--choice`
        // and for no other valued flag, and a label with a comma stays whole.
        let a = parse(&["ask", "m-1", "--person", "--choices", "-"]);
        assert_eq!((a.get("choices").as_deref(), a.rest.len()), (Some("-"), 1), "{:?}", a.rest);
        let a = parse(&["answer", "m-2", "--choice", "-"]);
        assert_eq!((a.get("choice").as_deref(), a.rest.len()), (Some("-"), 1));
        let a = parse(&["answer", "m-2", "--choice", "Views=Card", "--choice=Views=Rail, wide", "--other", "Q=free words"]);
        assert_eq!(a.each("choice"), vec!["Views=Card", "Views=Rail, wide"]);
        assert_eq!(a.each("other"), vec!["Q=free words"]);
        // `wsp-300`: `--wait` is a switch (the sentence after it is the
        // question), and `--timeout` takes its seconds.
        let a = parse(&["ask", "wsp-300", "--person", "--wait", "--timeout", "90", "land now?"]);
        assert!(a.has("wait") && a.get("timeout").as_deref() == Some("90") && a.text(1) == "land now?", "{:?}", a.rest);
        // `--cancel` is a standing flag: the id and the reason after it are
        // positionals, not its value (`wsp-308`).
        let a = parse(&["ask", "--cancel", "m-1", "no longer needed"]);
        assert!(a.has("cancel") && a.rest == ["m-1", "no longer needed"], "{:?}", a.rest);
        let a = parse(&["ask", "m-1", "--wait"]);
        assert!(a.has("wait") && a.rest.len() == 1);
        let a = parse(&["answer", "m-2", "--kind", "-"]);
        assert_eq!(a.get("kind").as_deref(), Some("true"), "any other flag still reads a dash as the next word");

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

    /// `wsp-230`. `check` stands alone on `verify`, so a barrier check's spawn
    /// — the one line `cycle` writes for every barrier — had its group number
    /// read as a second positional and was refused. A test built with
    /// `Args::synth` never meets the parser, which is how it got past one.
    #[test]
    fn spawn_check_takes_the_group_and_verify_check_still_stands_alone() {
        let parse = |line: &[&str]| crate::Args::parse(line.iter().map(|s| (*s).to_string()).collect());
        let a = parse(&["spawn", "gpui-migration", "--agent", "--check", "4", "--kind", "claude"]);
        assert_eq!(a.get("check").as_deref(), Some("4"));
        assert_eq!(a.rest, vec!["gpui-migration"], "and the group is not a second positional");
        let a = parse(&["verify", "--check", "src/cycle.rs"]);
        assert!(a.has("check"));
        assert_eq!(a.rest, vec!["src/cycle.rs"]);
    }

    /// `wsp-291`. `agent` is a bare flag everywhere but `spawn --agent`-style
    /// verbs that list it in [`VALUED_ON`]; on `leg add` it carries the policy,
    /// and without its entry the policy was dropped and the words passed on as
    /// positionals.
    #[test]
    fn leg_add_agent_takes_its_policy_and_why_its_reason() {
        let parse = |line: &[&str]| crate::Args::parse(line.iter().map(|s| (*s).to_string()).collect());
        let a = parse(&["leg", "add", "m-1", "plan", "--order", "o.md", "--agent", "claude opus high", "--why", "chooses"]);
        assert_eq!(a.get("agent").as_deref(), Some("claude opus high"));
        assert_eq!(a.get("why").as_deref(), Some("chooses"));
        assert_eq!(a.rest, vec!["add", "m-1", "plan"], "and the policy is not a positional");
        // Elsewhere it is still a bare flag that leaves the next word alone.
        let a = parse(&["spawn", "--agent", "m-1"]);
        assert_eq!(a.rest, vec!["m-1"]);
    }

    /// `wsp-419`: `--move` and `--also` are bare flags, so the task ids after
    /// them are still positionals — `add second --move r-1` must not read
    /// `r-1` as the flag's value and add nothing.
    #[test]
    fn worklist_add_move_and_also_leave_the_rows_after_them_alone() {
        let parse = |line: &[&str]| crate::Args::parse(line.iter().map(|s| (*s).to_string()).collect());
        let a = parse(&["worklist", "add", "second", "--move", "r-1", "r-2"]);
        assert_eq!(a.rest, vec!["add", "second", "r-1", "r-2"]);
        assert!(a.has("move"));
        let a = parse(&["worklist", "add", "second", "--also", "--why", "both", "r-1"]);
        assert_eq!(a.rest, vec!["add", "second", "r-1"]);
        assert!(a.has("also"));
        assert_eq!(a.get("why").as_deref(), Some("both"));
    }

    /// A rule that names a command nobody dispatches is a rule that does
    /// nothing, and it would do nothing silently — the payload would go on
    /// being parsed as flags with the table looking correct. Same check the
    /// help gets, for the same reason.
    /// `agent-018`: an agent followed the handbook's "give a wsp verb its
    /// prose through a stream", ran `… | wsp say -`, and wore the single
    /// character `-` as its status line until somebody noticed.
    ///
    /// Asserted on the parse rather than by calling `say`, because the failure
    /// was upstream of it: `say` was not on [`LITERAL_AFTER`], so `--from` was
    /// a flag to be swallowed and `-` was never a stream to anybody. Calling
    /// `say` needs a herdr pane and would test the store, which the handbook
    /// forbids; this tests the seam the bug was actually at.
    #[test]
    fn say_takes_its_prose_off_a_stream_like_every_other_prose_verb() {
        use super::Args;
        let parse = |line: &[&str]| Args::parse(line.iter().map(|s| (*s).to_string()).collect());
        let source = |line: &[&str]| {
            let a = parse(line);
            crate::cmd_task::payload_source(a.rest.get(0..).unwrap_or_default())
        };

        // The three spellings the other prose verbs take, now true of `say`.
        assert_eq!(source(&["say", "-"]), Some("-".into()), "a bare `-` names stdin");
        assert_eq!(source(&["say", "--from", "/tmp/s.md"]), Some("/tmp/s.md".into()));
        assert_eq!(source(&["say", "--from=/tmp/s.md"]), Some("/tmp/s.md".into()));

        // `--from` survives the parse as a positional rather than being eaten
        // as a flag — the half of the bug that made the handbook's spelling
        // unreachable however `say` read its payload.
        assert_eq!(parse(&["say", "--from", "/tmp/s.md"]).rest, vec!["--from", "/tmp/s.md"]);

        // And what must go on being a status line. A sentence is one however
        // it starts, which is why the row is `stream: true` and not a flag.
        assert_eq!(source(&["say", "landed the doorbell fix"]), None);
        assert_eq!(source(&["say", "--from is how you pass a file"]), None);
        assert_eq!(source(&["say"]), None, "no payload is `--clear`, not a stream");

        // Caught by driving a sandbox, not by this suite, and so written down
        // here: `say` is the first row with `subject: 0`, which stops flag
        // parsing at the word after the verb. Every flag it reads therefore
        // has to be in `OWNED_AFTER` or it becomes payload — `--pane` was
        // swallowed whole and `say` answered "no pane to name" on a line that
        // named one.
        let a = parse(&["say", "--pane", "w1:p1", "landed it"]);
        assert_eq!(a.get("pane").as_deref(), Some("w1:p1"), "`--pane` is a flag, not the status line");
        assert_eq!(a.text(0), "landed it");
        assert!(parse(&["say", "--clear", "--pane", "w1:p1"]).has("clear"));
        assert!(parse(&["say", "--json", "--pane", "w1:p1"]).json());

        // Every flag `say` reads is owned. A fourth added to the verb without
        // a row here is swallowed silently, which is this bug's whole shape.
        for f in ["pane", "clear", "json"] {
            assert!(
                super::OWNED_AFTER.contains(&("say", f)),
                "`say` reads --{f} but does not own it, so it lands in the payload"
            );
        }
    }

    #[test]
    fn every_command_whose_payload_is_literal_is_a_command() {
        let arms = dispatch();
        for l in super::LITERAL_AFTER {
            let cmd = l.cmd;
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
        assert!(!needs_store(&Args::synth("agent", &["pi-extension"], &[])));

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
        let a = args(&["list", "--alll"]);
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

    /// The whole of `worklist-050` in one assertion: on a verb that does not
    /// look first, `-n` is refused, and refused *before* the verb runs. The
    /// check is asked here in isolation because that is the only way to see the
    /// ordering — in `main` it is two lines above the dispatch, and a test that
    /// ran the dispatch to find out would be a test of the removal.
    ///
    /// Three verbs now, not five. `worklist-051` gave `verify --rm` and
    /// `worklist rm` real dry runs and left these three refusing on arguments
    /// of their own — see [`super::dry_run`] for the line between them. What is
    /// asserted here is the same property either way: `-n` on a verb that does
    /// not look first is refused, and refused *before* the verb runs.
    #[test]
    fn a_verb_that_cannot_look_first_refuses_n_rather_than_ignoring_it() {
        for argv in [
            &["despawn", "t-1"][..],
            &["rm", "t-1"][..],
            &["machine", "rm", "seat"][..],
        ] {
            let mut with_n: Vec<&str> = argv.to_vec();
            with_n.push("-n");
            let a = args(&with_n);
            let (reads, _) = super::dry_run(&a);
            assert!(!reads, "`wsp {}` accepted -n and would have done the real thing", argv.join(" "));
        }
    }

    /// The ones that do, with their aliases — and with the two invocations that
    /// prove the answer is not a property of the verb alone. `wsp verify --rm`
    /// looks first and plain `wsp verify` has nothing to look at, which is one
    /// verb split by a *flag*; `wsp sandbox rm` looks first and `wsp sandbox
    /// ls` does not, which is one verb split by a subcommand.
    #[test]
    fn a_verb_that_does_look_first_is_let_through_with_its_aliases() {
        for argv in [
            &["archive", "--days", "0"][..],
            &["checkout", "t-1", "--rm"][..],
            &["install"][..],
            &["migrate"][..],
            &["project", "rm", "batch"][..],
            &["p", "delete", "batch"][..],
            &["sandbox", "rm", "--all"][..],
            &["verify", "--rm"][..],
            &["verify", "--rm", "--all"][..],
            &["wl", "start", "night"][..],
            &["worklist", "rm", "night", "t-1"][..],
            &["wl", "remove", "night", "t-1"][..],
            &["worklist", "group", "night", "2", "--drop"][..],
        ] {
            let a = args(argv);
            assert!(super::dry_run(&a).0, "`wsp {}` reads -n and was refused it", argv.join(" "));
        }
    }

    /// `verify` is the one arm that turns on a flag, and the flag is the whole
    /// difference: `wsp verify` builds and has nothing to preview, `wsp verify
    /// --rm` removes trees no other verb in wsp can list. Asserted apart from
    /// the loops above because a table of invocations that all answer the same
    /// way cannot show a verb answering both.
    ///
    /// The second half is the one that would go wrong quietly. The check asks
    /// through `given`, so looking at `--rm` here must not tell the tally that
    /// somebody read it — otherwise `wsp add "t" --rm` would parse a word
    /// nothing acts on and lose the only warning there is about it.
    #[test]
    fn verify_looks_first_only_when_it_is_removing_and_asking_does_not_read_the_flag() {
        assert!(!super::dry_run(&args(&["verify"])).0, "a build has a dry run to offer");
        assert!(!super::dry_run(&args(&["verify", "src/main.rs"])).0, "a build has a dry run to offer");
        assert!(super::dry_run(&args(&["verify", "--rm"])).0, "the removing branch was refused -n");

        let a = args(&["verify", "--rm"]);
        assert!(super::dry_run(&a).0);
        assert!(
            !a.read.borrow().contains("rm"),
            "the check marked --rm read before dispatch, so a verb that then ignored it \
             would have the one tally that catches that silenced"
        );
    }

    /// A refusal has to say which verb, and a verb that dispatches on a
    /// subcommand has to be named by both words — "wsp worklist does not look
    /// first" is false, since `wsp worklist go` does.
    #[test]
    fn the_refusal_names_the_subcommand_when_the_verb_has_one() {
        assert_eq!(super::dry_run(&args(&["worklist", "rm", "night"])).1, "worklist rm");
        assert_eq!(super::dry_run(&args(&["sandbox", "ls"])).1, "sandbox ls");
        assert_eq!(super::dry_run(&args(&["despawn", "t-1"])).1, "despawn");
        assert_eq!(super::dry_run(&args(&["machine", "rm", "seat"])).1, "machine rm");
        assert_eq!(super::dry_run(&args(&["verify"])).1, "verify");
    }

    /// A refusal that only says no is the thing `worklist-051` decided against
    /// twice: `wsp rm` and `wsp machine rm` stay refused *because* the read that
    /// answers them already exists, so the refusal has to name it or the
    /// argument for refusing is not being made to the person it is made about.
    ///
    /// The signpost is checked for the verb it points at rather than word for
    /// word, so the sentence can be rewritten without the test being about
    /// prose.
    #[test]
    fn a_removing_verb_that_stays_refused_names_the_read_that_answers_it() {
        for (verb, points_at) in
            [("rm", "wsp show"), ("machine rm", "wsp machine show"), ("despawn", "wsp checkout")]
        {
            let said = super::signpost(verb).join(" ");
            assert!(!said.is_empty(), "`wsp {verb}` is refused -n with nothing to go on");
            assert!(
                said.contains(points_at),
                "the refusal on `wsp {verb}` does not send anybody to `{points_at}`: {said}"
            );
            // And the verb it sends them to has to be one wsp has. `dry_run`
            // answers for every string, so the question is not "is this a verb"
            // but "does the help describe it" — a signpost to a name the help
            // has never heard of is the same wrong turn as a stale
            // `LOOKS_FIRST`, one indirection along.
            let named = points_at.strip_prefix("wsp ").unwrap();
            assert!(
                super::help_text().contains(&format!("wsp {named}")),
                "the refusal on `wsp {verb}` sends people to `{points_at}`, which the help does not list"
            );
            assert!(
                super::LOOKS_FIRST.split(", ").all(|n| n != verb),
                "`wsp {verb}` is signposted as looking first and also refuses -n"
            );
        }
    }

    /// The other half of `worklist-051`, and the one that is about the verb an
    /// agent types most rather than about the ones that delete. `-n` on a
    /// reading verb is refused, with nothing to send anybody to — and that is
    /// the answer, not an omission.
    ///
    /// It could have been made a silent no-op: `wsp ls -n` is `wsp ls`, so the
    /// word is *true* there. What that would cost is the property this whole
    /// check is built on. Today the list below has one meaning — these verbs
    /// look first — and both ways of being wrong about it are safe. Accepting
    /// `-n` on readers needs a second list, of verbs where the word is
    /// harmless, and that list's mistakes are not symmetrical: a reader left
    /// off it is refused, which costs a retype, and a *remover* wrongly on it
    /// does the thing on the word that means do not do the thing, which is
    /// `worklist-044` back with a new spelling. One list whose errors are all
    /// benign beats two lists where the second one can kill a tree.
    ///
    /// And the cost being paid for that is small and was already being paid:
    /// `wsp ls -n` exited 2 before this check existed too, on the tally
    /// afterwards. What changed is that the refusal arrives first and the
    /// listing no longer prints — which is a retype, on a command that has
    /// nothing to lose.
    #[test]
    fn a_reading_verb_refuses_n_too_and_has_no_second_read_to_offer() {
        for argv in [&["ls"][..], &["show", "t-1"][..], &["projects"][..]] {
            let a = args(argv);
            assert!(!super::dry_run(&a).0, "`wsp {}` accepts -n", argv.join(" "));
            assert!(
                super::signpost(&super::dry_run(&a).1).is_empty(),
                "`wsp {}` is refused with a signpost, as if a preview of a read were a thing",
                argv.join(" ")
            );
        }
    }

    /// The signpost in the refusal is read by somebody who has just been told
    /// no and wants the nearest verb that would have answered, so a name on it
    /// that is not one is worse than a short list.
    #[test]
    fn every_verb_the_refusal_signposts_really_does_look_first() {
        for name in super::LOOKS_FIRST.split(", ") {
            let argv: Vec<&str> = name.split(' ').collect();
            assert!(
                super::dry_run(&args(&argv)).0,
                "the refusal sends people to `wsp {name}`, which does not read -n"
            );
        }
    }

    /// The check runs before dispatch, so it must not be the thing that tells
    /// the tally somebody looked. If it marked the word, a verb wrongly listed
    /// as reading `-n` would go on ignoring it *and* have `unknown_flags`
    /// silenced — the one fallback the list's mistakes have.
    #[test]
    fn asking_whether_n_was_given_is_not_reading_it() {
        let a = args(&["ls", "-n"]);
        assert!(a.given("dry-run"), "the word was on the command line");
        assert_eq!(a.dropped(), Vec::<String>::new(), "-n stands alone and takes no word");
        assert_eq!(super::unknown_flags(&a).len(), 1, "the tally stopped seeing an unread -n");
    }

    /// `wsp add "-p" -p demo` is ambiguous by construction — the first `-p` is
    /// a flag whichever way the shell quoted it — so the line that works, and
    /// the one a program building argv has to write, is the value behind `--`.
    /// Pinned for the two verbs the row found missing, so the escape stays one.
    #[test]
    fn a_dash_led_title_or_slug_behind_double_dash_is_the_positional() {
        let a = args(&["add", "-p", "demo", "--", "-p"]);
        assert_eq!(a.get("project").as_deref(), Some("demo"));
        assert_eq!(a.text(0), "-p");
        let a = args(&["project", "add", "--name", "N", "--", "-x"]);
        assert_eq!(a.rest, vec!["add", "-x"]);
        assert_eq!(a.get("name").as_deref(), Some("N"));
    }

    fn args(argv: &[&str]) -> super::Args {
        super::Args::parse(argv.iter().map(|s| (*s).to_string()).collect())
    }
}
