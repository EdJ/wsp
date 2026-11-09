//! The command line, parsed: `Args` and the tables its parse reads.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// Flags that never consume the following token.
pub const BOOL_FLAGS: &[&str] = &[
    "for-run",
    "json", "all", "force", "top", "raw", "overview", "details", "decisions", "verbose", "quiet", "yes", "clear", "tree", "inbox", "open", "done",
    "help", "version", "no-commit", "closed", "here", "agent", "focus", "no-tree", "terse", "seen", "full", "no-packet", "regen",
    // `worklist show --all --json --include-closed`.
    "include-closed",
    // `calls --polls` (wsp-488); `calls --all` is the existing "all".
    "polls",
    // `attempts --cost <row>`, whose positional is a task id.
    "cost",
    // `spawn --no-focus` is what focus not being asked for is now called, and it
    // is kept here rather than deleted so an invocation that still says it —
    // a script, a shell history, the README as it was — parses as a flag
    // instead of eating the task id after it.
    "no-focus",
    // `spawn --headless <task>`, whose positional is a task id.
    "headless",
    // `spawn --herdr <task>` the same way, and it is the OPT-IN since
    // `compound-112` flipped the default: compound is what a spawn gets when
    // it says nothing, and this asks for the fork by name. `--compound` stays
    // beside it and means what it always did, so a script, a shell history or
    // a work order that still says it keeps working.
    "herdr", "compound",
    // `verify` takes paths as positionals, so every flag it owns has to be
    // known here or `wsp verify --check src/main.rs` eats the path as a value.
    "release", "check", "rm", "alone", "no-alone", "no-retro", "packet", "at-clear", "defect", "strategic",
    // And `resume`, whose positional is a task or a project.
    "print",
    // And `checkout`, whose positional is a task id.
    "sweep", "all-roots",
    // And for `install`, whose positional is the binary to install.
    "dry-run",
    // `worklist output <slug> --none`, and the source of one it adds.
    "none", "wsp",
    // `worklist group <slug> N --together <more>`: a group's all-at-once mark
    // takes no value, or the word after it would be eaten (wsp-584).
    "together", "no-together",
    // Same for `sandbox`, whose positional is a sandbox name.
    "keep", "seed", "fake",
    // And `despawn`, whose positional is a task id.
    "keep-tree",
    // And `reopen`, whose positionals are the id and the reason.
    "no-seat",
    // `wsp review <id> "…" --hold-review` and `wsp yours --closed` (`wsp-547`).
    "hold-review", "closed",
    // And `govern`, whose positional is a project: `wsp spawn -p wsp --govern`
    // and `wsp govern wsp --remove` both put the flag last, where anything not
    // known here swallows the argument after it. `--rotate` is the same shape:
    // `wsp govern core --rotate` names the scope after the verb, and so does
    // the `--ending` a rotation starts behind itself, and `--take`, which
    // `wsp govern core --take` puts after the scope it takes.
    "govern", "remove", "rotate", "ending", "take",
    // `wsp spawn --primary` takes no positional, and `--primary --kind x`
    // must not read `--kind` as its value.
    "primary",
    // And `wsp watch <signal>…`, whose positionals are signal names.
    "now", "once", "status",
    // And `worklist add <slug> <parent> --sub`, whose positionals are the list
    // and the parent, and `worklist show <slug> --log|--verdicts|--stops`.
    "sub", "log", "verdicts", "stops",
    // `worklist add <slug> <task>… --move|--also`: the rows are positionals,
    // and the flag may sit between them.
    "move", "also",
    // And the return path. `wsp answer <id> --abandon "the reason"` is the word
    // order somebody types, and without this the reason is eaten as the flag's
    // value and the verb refuses for want of a sentence it was given. `--again`
    // is the escape from the repeat guard and is shared with both `tell` verbs.
    // `--anyway` is the escape from the readiness guard on a backend that
    // cannot vouch for a seat (`compound-097`), and it sits beside `--again`
    // for the same reason: both are a person overriding a refusal that is
    // right in general.
    // `--no-reply` (`wsp-521`) leaves the reply footer off a tell, and sits
    // before the sentence it must not eat.
    "abandon", "again", "anyway", "no-reply",
    // `wsp verified <member> --holds|--blocks`, `wsp spawn <member> --verify`
    // and `wsp migrate --verify-rows -n`: none takes a value, and each is
    // followed by a positional or a stream it must not eat.
    "holds", "blocks", "verify", "verify-rows", "barrier-rows", "run-notices",
    // `wsp check <task> --plan|--auto`: the task id is the positional, and
    // `--auto` is what the run's own trigger types.
    "plan", "auto", "set", "barrier", "inner", "affected", "tiers", "barrier-tree",
    // `wsp worklist followup <slug> <task>… --blocking|--next`: the rows are
    // positionals, and a flag between them must not eat the one after it.
    "blocking", "next",
    // `wsp ask <id> --person "…"` and `wsp answer <id> --as-person "…"`
    // (`wsp-233`): each is followed by the sentence, which it must not eat.
    "person", "as-person",
    // `wsp ask <id> --person --wait "…"` (`wsp-300`) blocks on the answer.
    "wait",
    // `wsp ask --cancel <mid> "why"` (`wsp-308`): the id and the reason follow.
    "cancel",
];

/// Flags whose value may be a lone `-`, the stream. A dash is otherwise read as
/// the next flag, so `--choices -` would have left `-` a positional and the flag
/// a bare `true`. Only these, because a valued flag followed by a payload
/// `-` is how every other verb spells "nothing given, read the stream". And
/// `--note -` on `wsp artefact put`, whose note is prose like the rest (`wsp-376`).
pub const STREAM_VALUED: &[&str] = &["choices", "choice", "note"];

/// Flags that keep their meaning inside a command's payload.
///
/// Everything here is either a question about the invocation itself
/// (`--help`, `--version`) or a dial on how the answer is printed and
/// recorded. None of them is ever the thing being said, which is what makes
/// them safe to go on reading after [`LITERAL_AFTER`] has stopped flag
/// parsing: `wsp note 028 "…" --json` still prints JSON, while `-ui` and
/// `--parent …` reach the command as the text and the tag edits they are.
pub const GLOBAL_FLAGS: &[&str] = &["json", "help", "version", "no-commit", "terse", "quiet", "verbose"];

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
/// Six commands are listed and no more. Each takes an id and then a payload
/// that is the user's own vocabulary — free prose, or `+tag`/`-tag` — and none
/// of them owns a flag of its own beyond the global ones above, so nothing is
/// lost by stopping. `add`, `find`, `flag` and `say` take prose too but carry
/// real flags after it (`wsp add "…" -p wsp`, `wsp flag <id> --seen`), so they
/// keep ordinary parsing and lean on the whitespace rule in [`Args::scan`].
///
/// A payload that is *nothing but* a flag-shaped word is refused rather than
/// recorded — see [`swallowed_flag`] — and `--` is how a caller who meant
/// those words as the text says so.
pub const LITERAL_AFTER: &[Literal] = &[
    Literal { cmd: "note", subject: 1, payload: "log entry", stream: true },
    Literal { cmd: "block", subject: 1, payload: "question", stream: true },
    Literal { cmd: "park", subject: 1, payload: "reason", stream: true },
    // An account is prose *about the CLI* more reliably than any other payload
    // here — it is a sentence about what a verb now does — so it wants this seam
    // for the reason `note` does, and `-` for the reason `edit --overview` does.
    Literal { cmd: "review", subject: 1, payload: "account", stream: true },
    Literal { cmd: "decide", subject: 1, payload: "decision", stream: true },
    // A verdict is prose about code as reliably as an account is.
    Literal { cmd: "verified", subject: 1, payload: "verdict", stream: true },
    // The only row with no subject: `say` speaks for the pane it is run in, so
    // its payload starts at the first word. It is here for the stream form
    // alone — `agent-018` was an agent that followed the handbook's "give a
    // wsp verb its prose through a stream", got the literal `-` as its status
    // line, and was told nothing.
    Literal { cmd: "say", subject: 0, payload: "status line", stream: true },
    Literal { cmd: "rename", subject: 1, payload: "title", stream: false },
    // A caption is one line, but the spec gives every prose intake `-`, and a
    // caption beginning with a flag-shaped word is still the caption.
    Literal { cmd: "caption", subject: 1, payload: "caption", stream: true },
    Literal { cmd: "tag", subject: 1, payload: "tag edits", stream: false },
    // `wsp ref <id> -~/claude/spec.md` is the same `+`/`-` payload as `tag`,
    // and a path that begins with `-` here would be read as a flag for exactly
    // the reason `-ui` was.
    Literal { cmd: "ref", subject: 1, payload: "ref edits", stream: false },
];

/// One row of [`LITERAL_AFTER`]: a verb, and what it does with the words past
/// its subject.
///
/// One table rather than three keyed alike, because the refusal below needs
/// two more facts about exactly these six verbs and a second list of them
/// would drift the first time a seventh is added.
pub struct Literal {
    pub cmd: &'static str,
    /// How many positionals the subject takes before the payload begins.
    pub subject: usize,
    /// What the payload becomes, named the way the verb's own output names it.
    /// The refusal says what was about to be written, and "recorded as the log
    /// entry" is a sentence the caller can check against what they meant.
    pub payload: &'static str,
    /// Whether `--from` inside the payload is read rather than recorded.
    ///
    /// It is not in [`OWNED_AFTER`] because it is deliberately *not* parsed as
    /// a flag: `cmd_task::payload_source` matches it out of the positionals so
    /// that `wsp note <id> "--from is add-only"` stays a sentence. That makes
    /// it invisible to [`swallowed_flag`], which would otherwise refuse the one
    /// spelling the handbook teaches. `rename` and `tag` have no stream form,
    /// so on them `--from` is the mistake it looks like.
    pub stream: bool,
}

/// The [`LITERAL_AFTER`] row for a verb, if it has one.
pub fn literal(cmd: &str) -> Option<&'static Literal> {
    LITERAL_AFTER.iter().find(|l| l.cmd == cmd)
}

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
pub const OWNED_AFTER: &[(&str, &str)] = &[
    ("decide", "supersedes"),
    // A review of a commit a verifier already blocked, put up as it is (`wsp-529`).
    ("review", "anyway"),
    // The verdict's own two words, which come after the member as often as not.
    ("verified", "holds"),
    ("verified", "blocks"),
    // And its caption (`wsp-241`), required, and typed after the verdict's
    // `--from FILE` more often than not.
    ("verified", "caption"),
    // `say` is the first row with `subject: 0`, so the payload begins at the
    // word after the verb and *every* flag it reads falls inside it. These
    // three are the whole of what `say` reads — `--pane` names the seat when
    // the process is not standing in one, `--clear` takes the label off, and
    // `--json` is the receipt — and without them here the row would trade the
    // silent `-` for three silently swallowed flags.
    ("say", "pane"),
    ("say", "clear"),
    ("say", "json"),
    // `wsp caption <id> --clear` is the verb's other half, and the subject
    // comes first as often as not.
    ("caption", "clear"),
];

/// Verbs on which a [`BOOL_FLAGS`] name takes a value instead.
///
/// One name collides today: `status`. On `watch` it is a mode — `wsp watch
/// --status` lists the watches and reads nothing after it — so there it stands
/// alone. Everywhere else the word is read as a filter somebody typed a value
/// for: `wsp ls -s done`, `wsp find -s review`, `wsp add "…" --status review`,
/// `wsp project add x --status done`. With the name in [`BOOL_FLAGS`] only,
/// every one of those parsed as a bare flag — the filter silently ignored,
/// `project add` writing the word `true` into the project's status — while the
/// value went on through as a positional nothing refused, because `status` is
/// a word the tally knows. `--status=done` worked throughout, which is how
/// this stayed hidden so long: nobody bitten once goes back to check the form
/// that bit them.
///
/// Listed per verb rather than struck from [`BOOL_FLAGS`] because that list
/// protects positionals which follow a verb's own flags (`spawn --headless
/// <id>`, `verify --check <path>`), and un-listing a name globally would hand
/// that hazard back the day `watch` grows a positional beside its modes. Keyed
/// on the top-level verb only, which is all [`Args::parse`] has read when this
/// table is consulted — `wsp project add` and its siblings share one entry,
/// and none of them reads `--status` as a mode.
pub const VALUED_ON: &[(&str, &str)] = &[
    ("ls", "status"),
    ("list", "status"),
    ("find", "status"),
    ("add", "status"),
    // `wsp review <id> --anyway "why"` carries its reason (`wsp-529`); elsewhere
    // `anyway` stands alone.
    ("review", "anyway"),
    ("project", "status"),
    // `spawn --agent` stands alone; on a worklist it names who runs a group.
    ("worklist", "agent"),
    // And on `leg add`, where it is the policy the leg runs on and `--why` the
    // reason: `--agent "claude opus high"` is read as a value, not as a flag
    // that leaves its policy behind as a positional.
    ("leg", "agent"),
    // `piece add`, the same way: the overview is a file, the tier a policy, and
    // each `--check` a line (`docs/pieces.md`, *The record*).
    ("piece", "overview"),
    ("piece", "agent"),
    ("piece", "check"),
    // `verify --check` stands alone; on `spawn` it names the group a barrier
    // check reads (`wsp-230`).
    ("spawn", "check"),
];

/// Whether this flag stands alone on this verb.
///
/// [`BOOL_FLAGS`] says which names never take a value and [`VALUED_ON`] names
/// the verbs on which one of them does. One helper rather than the condition
/// spelled twice, because [`Args::scan`]'s two branches already differ in how
/// they read a token that follows — the long form refuses any `-`-led word,
/// the short form only `--` — and the two agreeing *here* is what keeps
/// `-s done` and `--status done` meaning the same thing.
pub fn bool_flag(name: &str, valued_on: &[&str]) -> bool {
    BOOL_FLAGS.contains(&name) && !valued_on.contains(&name)
}

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
pub const ACCEPTED_UNREAD: &[(&str, &str)] = &[("spawn", "no-focus")];

pub struct Args {
    pub cmd: String,
    pub rest: Vec<String>,
    pub flags: HashMap<String, Vec<String>>,
    /// Flag names that took a word off the command line — `--from FILE`,
    /// `--title=T` — as against the ones that stand for themselves.
    ///
    /// This is half of the answer to `worklist-036`'s second question, and
    /// [`Args::dropped`] is the other half.
    pub valued: HashSet<String>,
    /// Which flags anything actually looked at. Interior mutability because
    /// every command takes `&Args` and a read is a read whether or not the
    /// caller holds it mutably; nothing here crosses a thread.
    pub read: RefCell<HashSet<String>>,
    /// Whether a bare `--` ended flag parsing.
    ///
    /// The only thing that tells `wsp note <id> -- --body -`, which means
    /// *record those words*, from `wsp note <id> --body -`, which means the
    /// caller believed `--body` was a flag. Both reach the payload as the same
    /// two tokens, so [`swallowed_flag`] cannot tell them apart from the
    /// payload alone — and refusing the first would take away the escape hatch
    /// the help sends people to.
    pub escaped: bool,
}

impl Args {
    pub fn parse(argv: Vec<String>) -> Args {
        // Twice, because the rule depends on the command and the command is
        // itself the first thing the scan finds. The first pass is only ever
        // read for `cmd`: a leading flag may swallow a token that the strict
        // pass hands back as a positional, but neither pass can turn a
        // different token into the verb — the verb is the first bare word
        // either way.
        let cmd = Args::scan(&argv, None, &[], &[]).cmd;
        let literal_after = literal(&cmd).map(|l| l.subject);
        let owned: Vec<&str> =
            OWNED_AFTER.iter().filter(|(c, _)| *c == cmd).map(|(_, f)| *f).collect();
        let valued_on: Vec<&str> =
            VALUED_ON.iter().filter(|(c, _)| *c == cmd).map(|(_, f)| *f).collect();
        Args::scan(&argv, literal_after, &owned, &valued_on)
    }

    /// One pass over argv. `literal_after`, when set, is how many positionals
    /// this command parses normally before the rest of the line is its payload.
    pub fn scan(argv: &[String], literal_after: Option<usize>, owned: &[&str], valued_on: &[&str]) -> Args {
        let mut positional: Vec<String> = Vec::new();
        let mut flags: HashMap<String, Vec<String>> = HashMap::new();
        // Which of them ate a word. See [`Args::dropped`]: a flag that stands
        // for itself costs nothing when nobody reads it, and one that took the
        // token after it has taken something that was going somewhere.
        let mut valued: HashSet<String> = HashSet::new();
        let mut escaped = false;
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
                    escaped = true;
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
                } else if bool_flag(&name, valued_on) {
                    entry.push("true".into());
                } else if i + 1 < argv.len()
                    && (!argv[i + 1].starts_with('-') || (argv[i + 1] == "-" && STREAM_VALUED.contains(&name.as_str())))
                {
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
                if bool_flag(&name, valued_on) {
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
        Args { cmd, rest: positional, flags, valued, read: RefCell::default(), escaped }
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
            // …and no `--` either: the payload is passed as the positional it
            // already is, so nothing had to be escaped to get here.
            escaped: false,
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.mark(name);
        self.flags.contains_key(name)
    }
    /// Was the flag given — asked *without* counting it as read.
    ///
    /// One caller, [`dry_run`]'s check in `main`, and the exception is the
    /// whole point of it: that check runs before dispatch, and marking the word
    /// there would tell [`Args::dropped`] and [`unknown_flags`] that something
    /// had looked at it. The read tally is what catches a verb wrongly listed
    /// as reading `-n`, so the check that decides on the strength of the list
    /// must not be the thing that silences the tally.
    pub fn given(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }
    pub fn get(&self, name: &str) -> Option<String> {
        self.mark(name);
        self.flags.get(name).and_then(|v| v.first().cloned())
    }
    /// Every value the flag was given, as typed: not split at commas, which
    /// [`Args::all`] does and which a label like `Rail, wide` would not survive.
    pub fn each(&self, name: &str) -> Vec<String> {
        self.mark(name);
        self.flags.get(name).cloned().unwrap_or_default()
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
    pub fn mark(&self, name: &str) {
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

pub fn expand_short(s: &str) -> String {
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
