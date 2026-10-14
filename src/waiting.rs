//! Whether a seat is waiting on somebody, and on whom. `wsp-172`.
//!
//! **One reading, two sources, and every reader asks this rather than its own
//! copy.** Ed, 2026-10-05: *a seat that is waiting on someone says so,
//! everywhere, and nothing treats it as idle.* The cycle that wrote that down
//! had two readers of one seat disagree: the attention pass read `cpd-254`'s
//! opencode permission prompt as `needs-a-person`, and seven hours later the
//! reconciler read the same seat as `idle`, told it to finish, and ended it.
//! The one that acts was the one that was wrong, and eight hours of work
//! survived only because nothing had committed it away.
//!
//! The two sources:
//!
//! - **The screen.** [`State::Blocked`] — a permission or trust prompt holding
//!   the keyboard. That is the backend's reading and not this module's: herdr
//!   scrapes it, and compound asks `compound-render`'s detector, whose opencode
//!   rule matches opencode 1.18's `△ Permission required` dialog (checked live
//!   against a real one on 2026-10-06). A prompt is waiting on **a person**,
//!   whatever seat governs the work: only a keypress answers it.
//! - **The store.** An open `wsp ask` whose asker is this seat, or this seat's
//!   task. The agent ran the verb, was told the answer comes back here, and is
//!   sitting at its prompt until it does — which reads `idle` on every screen.
//!   It is waiting on **whoever the question was routed to**: the governor that
//!   answers for the task, or a person when no seat is above it.
//!
//! - **Itself.** A shell of its own still running under the agent: a
//!   background command or a watch it started, which wakes it when it ends
//!   (`wsp-214`). Claude Code's footer says `1 shell still running`, and the
//!   screen says `idle`. It is waiting on **nobody**: not stuck, and not owed a
//!   person. See [`Shells`] for how it is read and where that reading stops.
//!
//! **No new state, and that is the brief's instruction rather than a taste.**
//! `place::State::Blocked` already means *stopped in front of a question only a
//! person can answer*; this adds the store half beside it as a reading over
//! [`State`], not a seventh variant of it, so a backend never has to know what
//! a question record is.
//!
//! # What a reader does with it
//!
//! Nothing nudges or ends a waiting seat: [`crate::repair`]'s stall repair
//! holds it with no ceiling and says so once, the barrier gate holds on it
//! ([`crate::cycle`]), and `wsp tell` refuses the prompt half — a sentence
//! typed at a dialog selects an answer nobody chose. The answer half is *not*
//! refused by `tell`, because an answer typed to an agent waiting for one is
//! the delivery, not an interruption.

use std::collections::BTreeMap;

use crate::place::State;
use crate::store::Store;

/// What the seat is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Why {
    /// A prompt on its screen that only a keypress answers.
    Prompt,
    /// Its own `wsp ask`, by the question's id.
    Answer(String),
    /// Shells of its own still running, and how many.
    Shell(usize),
}

/// Whom it is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum On {
    Person,
    /// The governor of this scope.
    Seat(String),
    /// Nobody: what it waits on is its own, and wakes it.
    Itself,
}

/// A seat that is waiting, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Wait {
    pub(crate) why: Why,
    pub(crate) on: On,
}

impl Wait {
    /// The word a column draws: `waiting · person`, `waiting · wsp-process`.
    pub(crate) fn word(&self) -> String {
        match &self.on {
            On::Person => "waiting · person".into(),
            On::Seat(scope) => format!("waiting · {scope}"),
            On::Itself => "waiting · itself".into(),
        }
    }

    /// Whether somebody owes this seat something: false for a seat waiting on
    /// itself. A governor's `members_waiting`, `needs_you` and the run's word
    /// to the governor are each about a seat somebody has to answer, and a
    /// seat waiting on its own shell is held without anybody being told.
    pub(crate) fn owed(&self) -> bool {
        self.on != On::Itself
    }

    /// The reason, as a clause a log line can carry.
    pub(crate) fn sentence(&self) -> String {
        let whom = match &self.on {
            On::Person => "a person".to_string(),
            On::Seat(scope) => format!("the {scope} governor"),
            On::Itself => "itself".to_string(),
        };
        match &self.why {
            Why::Prompt => format!("stopped on a prompt only {whom} can answer"),
            Why::Answer(id) => format!("waiting on {whom} to answer its question {id}"),
            Why::Shell(n) => format!(
                "waiting on {} of its own still running, which wakes it when {} ends",
                if *n == 1 { "a shell".to_string() } else { format!("{n} shells") },
                if *n == 1 { "it" } else { "each" }
            ),
        }
    }

    /// The published shape — `wsp wip --json`'s `waiting`, which compound's
    /// chrome reads. `on` is `person`, a scope, or `itself`; `shells` is set
    /// only on the last.
    pub(crate) fn json(&self) -> serde_json::Value {
        let (why, question, shells) = match &self.why {
            Why::Prompt => ("prompt", None, None),
            Why::Answer(id) => ("answer", Some(id.as_str()), None),
            Why::Shell(n) => ("shell", None, Some(*n)),
        };
        let on = match &self.on {
            On::Person => "person",
            On::Seat(scope) => scope.as_str(),
            On::Itself => "itself",
        };
        let mut v = serde_json::json!({ "why": why, "on": on, "question": question, "word": self.word() });
        if let Some(n) = shells {
            v["shells"] = n.into();
        }
        v
    }
}

/// The open questions, read once, indexed by who is sitting still behind each.
///
/// **Read once per pass and asked per seat**, because every reader here walks
/// many seats and `messages.json` is one file: the reconciler, a `wip` and a
/// watch tick each build one of these and ask it for every row.
#[derive(Debug, Clone, Default)]
pub(crate) struct Asks {
    by_pane: BTreeMap<String, (String, On)>,
    by_task: BTreeMap<String, (String, On)>,
}

impl Asks {
    /// Every question still owed an answer, and whom it waits on.
    ///
    /// [`crate::message::Message::wants_answering`], the population `wsp ask`
    /// lists and `wsp watch unanswered` reports, so a question this counts is
    /// one those surfaces show. It includes a record this build cannot read,
    /// and here that errs the right way: a seat wrongly held as waiting is
    /// left alone, and a seat wrongly read as idle is ended.
    ///
    /// The routing is [`crate::cmd_govern::answering_seat`]'s walk with its
    /// three reads taken once here: compound polls `wsp wip --json`, and a
    /// read of every project per open question is a cost paid on every poll.
    pub(crate) fn read(store: &Store) -> Asks {
        let mut asks = Asks::default();
        let open: Vec<_> = store
            .messages()
            .into_values()
            .filter(|m| m.wants_answering() && !m.is_reply() && m.waiting.is_some())
            .collect();
        if open.is_empty() {
            return asks;
        }
        let index = crate::resolve::Index::new(store.projects());
        let lists = crate::worklist::Running::read(store);
        let governors = store.governors();
        for m in &open {
            let Some(w) = &m.waiting else { continue };
            let on = m
                .about
                .task()
                .and_then(|id| store.find_task(id))
                .and_then(|t| crate::cmd_govern::seat_for(&governors, &index, lists.list_of(&t.id), t.project.as_deref()))
                .map(|s| On::Seat(s.scope))
                .unwrap_or(On::Person);
            // Oldest first and the later one wins, so the question a seat names
            // is its most recent — the one it is most likely sitting on.
            if !w.pane.is_empty() {
                asks.by_pane.insert(w.pane.clone(), (m.id.clone(), on.clone()));
            }
            if !w.task.is_empty() {
                asks.by_task.insert(w.task.clone(), (m.id.clone(), on));
            }
        }
        asks
    }

    /// The open question this seat, or the task it holds, is waiting on.
    ///
    /// **The task as well as the pane**, because the pane is the perishable
    /// half: a member ended and started again is a new seat on the same task,
    /// and the question its predecessor asked is still the one the work is
    /// waiting on.
    pub(crate) fn of(&self, seat: &str, task: &str) -> Option<Wait> {
        let hit = self.by_pane.get(seat).or_else(|| (!task.is_empty()).then(|| self.by_task.get(task)).flatten())?;
        Some(Wait { why: Why::Answer(hit.0.clone()), on: hit.1.clone() })
    }

    #[cfg(test)]
    pub(crate) fn one(pane: &str, task: &str, id: &str, on: On) -> Asks {
        let mut a = Asks::default();
        a.by_pane.insert(pane.to_string(), (id.to_string(), on.clone()));
        a.by_task.insert(task.to_string(), (id.to_string(), on));
        a
    }
}

/// The shells each seat's agent has running, read once per pass off one `ps`.
///
/// **Why the process table and not the screen or the row.** Seen three times
/// in two days (`wsp-214`): a member landed, started its release verify as a
/// background shell, and stopped. Its last line said so, the row's log said
/// so, and Claude Code's footer read `1 shell still running` — and the run told
/// it to finish and raised `needs-a-person` to the governor. A line the agent
/// writes is a line it can forget to write, and a footer is one agent kind's
/// drawing. The process is there whatever the agent said: Claude Code runs a
/// background `Bash`, and a `Monitor`, as a shell under its own process, and
/// that shell is what wakes it.
///
/// **What counts:** a shell running a command (`sh`, `bash`, `zsh`, `dash`,
/// `ksh` or `fish`, with `-c`) under the seat's root ([`crate::place::Place::root`]),
/// whose parent is not one — the subshells under it are the same command, not
/// another one. Two bounds, both from what else looks like that:
///
/// - **At least [`SETTLED`] old.** A hook and the status line are shells under
///   the agent too, and live for a moment. A background command younger than
///   that is read on the next pass.
/// - **Younger than [`LEFT_RUNNING`].** A shell the agent has not seen end in
///   an hour is a dev server it left running, not a command it is waiting on,
///   and holding the seat for it would hold it for good. Past that the seat
///   reads idle again and the run tells it, which is a sentence and not an end:
///   the end is a further threshold later.
///
/// A backend that cannot name a root, and a far machine whose process table
/// is not this one, read as no shells: the seat is idle as before.
#[derive(Debug, Clone, Default)]
pub(crate) struct Shells {
    by_seat: BTreeMap<String, usize>,
}

/// How long a shell has to have been running to be one the agent is waiting
/// on, in seconds. See [`Shells`].
const SETTLED: i64 = 5;

/// How long a shell may run and still be waited on, in seconds. See [`Shells`].
const LEFT_RUNNING: i64 = 60 * 60;

/// The shells that run a command.
const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "fish"];

impl Shells {
    /// These seats' shells, off the local backends' roots and one `ps`. No
    /// `ps` is run when none of them has a root here.
    ///
    /// Asked only about seats that read `Idle`, by every caller: a seat in a
    /// turn is working whatever it has running, and the reading is a process
    /// table that `wsp wip` would otherwise pay for on every poll.
    pub(crate) fn read<'a>(seats: impl IntoIterator<Item = &'a str>) -> Shells {
        // A test's seat ids are not this machine's, and a fake backend's are
        // not anybody's: a test hands the index in with [`Shells::one`].
        if cfg!(test) {
            return Shells::default();
        }
        let backends = crate::cmd_spawn::local_backends();
        let roots: Vec<(String, u32)> = seats
            .into_iter()
            .filter_map(|s| {
                let seat = crate::place::Seat::new(s);
                backends.iter().find_map(|b| b.root(&seat)).map(|pid| (s.to_string(), pid))
            })
            .collect();
        if roots.is_empty() {
            return Shells::default();
        }
        match std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,etime=,command="]).output() {
            Ok(out) => Shells::of(&String::from_utf8_lossy(&out.stdout), &roots),
            // Not "waiting": a `ps` that did not run is no evidence of a shell,
            // and the cost of reading none is a seat told, which is a sentence.
            Err(_) => Shells::default(),
        }
    }

    /// The reading over a `ps -o pid=,ppid=,etime=,command=` table.
    fn of(ps: &str, roots: &[(String, u32)]) -> Shells {
        struct Row {
            pid: u32,
            ppid: u32,
            age: i64,
            shell: bool,
        }
        let rows: Vec<Row> = ps
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let pid = it.next()?.parse().ok()?;
                let ppid = it.next()?.parse().ok()?;
                let age = elapsed(it.next()?)?;
                let command: Vec<&str> = it.collect();
                Some(Row { pid, ppid, age, shell: runs_a_command(&command) })
            })
            .collect();
        let shell: std::collections::BTreeSet<u32> = rows.iter().filter(|r| r.shell).map(|r| r.pid).collect();
        let mut by_seat = BTreeMap::new();
        for (seat, root) in roots {
            let mut tree = vec![*root];
            let mut i = 0;
            while i < tree.len() {
                let parent = tree[i];
                tree.extend(rows.iter().filter(|r| r.ppid == parent && !tree.contains(&r.pid)).map(|r| r.pid).collect::<Vec<_>>());
                i += 1;
            }
            let n = rows
                .iter()
                .filter(|r| r.pid != *root && tree.contains(&r.pid))
                .filter(|r| r.shell && !shell.contains(&r.ppid))
                .filter(|r| (SETTLED..LEFT_RUNNING).contains(&r.age))
                .count();
            if n > 0 {
                by_seat.insert(seat.clone(), n);
            }
        }
        Shells { by_seat }
    }

    /// This seat's own shells, as a wait — `None` when it has none.
    pub(crate) fn of_seat(&self, seat: &str) -> Option<Wait> {
        self.by_seat.get(seat).map(|n| Wait { why: Why::Shell(*n), on: On::Itself })
    }

    #[cfg(test)]
    pub(crate) fn one(seat: &str, n: usize) -> Shells {
        Shells { by_seat: BTreeMap::from([(seat.to_string(), n)]) }
    }
}

/// Whether a command line is a shell running a command: `/bin/zsh -c …`,
/// `bash -lc …`, and not a login shell sitting at its own prompt.
fn runs_a_command(command: &[&str]) -> bool {
    let Some(argv0) = command.first() else { return false };
    let name = argv0.rsplit('/').next().unwrap_or(argv0).trim_start_matches('-');
    SHELLS.contains(&name)
        && command[1..]
            .iter()
            .take_while(|w| w.starts_with('-') && !w.starts_with("--"))
            .any(|w| w[1..].contains('c'))
}

/// `ps`'s `etime`, `[[dd-]hh:]mm:ss`, in seconds.
fn elapsed(etime: &str) -> Option<i64> {
    let (days, rest) = match etime.split_once('-') {
        Some((d, r)) => (d.parse::<i64>().ok()?, r),
        None => (0, etime),
    };
    let mut secs = 0;
    for part in rest.split(':') {
        secs = secs * 60 + part.parse::<i64>().ok()?;
    }
    Some(days * 86_400 + secs)
}

/// **The one reading.** A seat in `state`, holding `task`, is waiting — or not.
///
/// The prompt wins over the question: a modal holding the keyboard is the
/// repair a person has to make first, and an agent can be in one while a
/// question of its own is standing. The question wins over the seat's own
/// shells, because somebody owes it the answer and nobody owes it the shell.
///
/// **The question and the shells count only on a seat that reads `Idle`** — at
/// its prompt, which is where an agent that asked and stopped is sitting. One
/// mid-turn asked and carried on, and is working; one that is `Gone` has
/// nobody left to wait, and holding it would keep a dead seat from ever being
/// started again; one nobody can read is not waiting on anybody as far as this
/// can say.
pub(crate) fn reading(state: Option<State>, asks: &Asks, shells: &Shells, seat: &str, task: &str) -> Option<Wait> {
    match state? {
        State::Idle => asks.of(seat, task).or_else(|| shells.of_seat(seat)),
        other => on_screen(other),
    }
}

/// The screen half alone, for the one reader that refuses only it: `wsp tell`.
///
/// A sentence typed at a dialog selects an answer nobody chose, so `tell`
/// refuses a prompt. It does *not* refuse a seat waiting on its own question,
/// because that is how an answer arrives.
pub(crate) fn on_screen(state: State) -> Option<Wait> {
    (state == State::Blocked).then_some(Wait { why: Why::Prompt, on: On::Person })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_on_screen_is_waiting_on_a_person_whoever_governs_the_work() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Seat("wsp-process".into()));
        let w = reading(Some(State::Blocked), &asks, &Shells::default(), "cpd-1", "wsp-1").expect("a prompt is waiting");
        assert_eq!(w.why, Why::Prompt, "the keyboard is held, so that is the repair first");
        assert_eq!(w.word(), "waiting · person");
    }

    #[test]
    fn an_open_ask_reads_waiting_on_the_seat_it_was_routed_to_while_the_screen_says_idle() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Seat("wsp-process".into()));
        let w = reading(Some(State::Idle), &asks, &Shells::default(), "cpd-1", "wsp-1").expect("idle at its prompt, and waiting");
        assert_eq!(w.word(), "waiting · wsp-process");
        assert!(w.sentence().contains("q-1"), "{}", w.sentence());
    }

    #[test]
    fn a_restarted_member_inherits_the_question_its_task_is_waiting_on() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Person);
        assert!(reading(Some(State::Idle), &asks, &Shells::default(), "cpd-9", "wsp-1").is_some(), "same task, new seat");
        assert!(reading(Some(State::Idle), &asks, &Shells::default(), "cpd-9", "wsp-2").is_none(), "another task is not");
    }

    #[test]
    fn an_open_ask_holds_only_a_seat_sitting_at_its_prompt() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Person);
        for state in [State::Working, State::Gone, State::Starting, State::Unknown, State::Empty] {
            assert_eq!(reading(Some(state), &asks, &Shells::default(), "cpd-1", "wsp-1"), None, "{state:?}");
        }
        assert_eq!(reading(None, &asks, &Shells::default(), "cpd-1", "wsp-1"), None, "nobody can say");
    }

    #[test]
    fn idle_with_nothing_asked_is_not_waiting() {
        assert_eq!(reading(Some(State::Idle), &Asks::default(), &Shells::default(), "cpd-1", "wsp-1"), None);
        assert_eq!(reading(None, &Asks::default(), &Shells::default(), "cpd-1", "wsp-1"), None);
    }

    /// The `ps` table measured under a compound seat on 2026-10-14, cut down:
    /// `compound-sup` is the seat's root, `claude` the agent under it, and
    /// Claude Code's background `Bash` a `zsh -c` of its own with the command
    /// under that. Ages are `etime`.
    const TABLE: &str = "\
    1     0 13-01:13:00 /sbin/launchd
56312     1    02:03:00 /Users/ed/.local/bin/compound-sup
56320 56312    02:02:58 claude --strict-mcp-config -n wsp-214-cpd-1025 --model opus
56558 56320       00:04 caffeinate -i -t 300
64546 56320    12:31 /bin/zsh -c source /Users/ed/.claude/shell-snapshots/snapshot-zsh-1.sh && eval 'wsp verify --release'
64550 64546    12:30 /bin/zsh -c source /Users/ed/.claude/shell-snapshots/snapshot-zsh-1.sh && eval 'wsp verify --release'
64551 64550    12:30 cargo test --release
70001 56320       00:01 /bin/sh -c ~/.claude/statusline.sh
80000     1    05:00 /usr/local/bin/compound-sup
80001 80000    04:58 claude -n wsp-9-cpd-2
";

    #[test]
    fn an_agent_idle_on_its_own_background_shell_is_waiting_on_itself_and_owed_nothing() {
        let shells = Shells::of(TABLE, &[("cpd-1025".into(), 56312)]);
        let w = reading(Some(State::Idle), &Asks::default(), &shells, "cpd-1025", "wsp-214").expect("its verify is running");
        assert_eq!(w.why, Why::Shell(1), "one shell, and not the subshell under it, the hook or caffeinate");
        assert_eq!(w.word(), "waiting · itself");
        assert!(!w.owed(), "nobody is needed: the shell wakes it when it ends");
        assert_eq!(w.json()["on"], "itself");
        assert_eq!(w.json()["shells"], 1);
        assert!(w.sentence().contains("a shell of its own"), "{}", w.sentence());
    }

    #[test]
    fn another_seats_shell_is_not_this_seats() {
        let shells = Shells::of(TABLE, &[("cpd-1025".into(), 56312), ("cpd-2".into(), 80000)]);
        assert!(shells.of_seat("cpd-2").is_none(), "cpd-2's agent has nothing running under it");
        assert!(shells.of_seat("cpd-9").is_none(), "nor has a seat nobody gave a root");
    }

    #[test]
    fn a_shell_of_a_moment_is_a_hook_and_one_of_hours_was_left_running() {
        let table = "\
  100     1    01:00:00 compound-sup
  101   100    59:00 claude
  102   101       00:02 /bin/sh -c wsp hook stop
  103   101    01:00:00 bash -lc npm run dev
";
        let shells = Shells::of(table, &[("cpd-1".into(), 100)]);
        assert!(shells.of_seat("cpd-1").is_none(), "a two-second hook and an hour-old server are not waited on");
        assert!(Shells::of(&table.replace("01:00:00 bash", "59:59 bash"), &[("cpd-1".into(), 100)]).of_seat("cpd-1").is_some());
    }

    #[test]
    fn a_question_outranks_the_shell_and_neither_holds_a_seat_in_a_turn() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Person);
        let shells = Shells::one("cpd-1", 2);
        let w = reading(Some(State::Idle), &asks, &shells, "cpd-1", "wsp-1").expect("waiting");
        assert_eq!(w.why, Why::Answer("q-1".into()), "somebody owes it the answer; nobody owes it the shell");
        assert!(w.owed());
        for state in [State::Working, State::Gone, State::Starting, State::Unknown, State::Empty] {
            assert_eq!(reading(Some(state), &Asks::default(), &shells, "cpd-1", "wsp-1"), None, "{state:?}");
        }
    }

    #[test]
    fn a_login_shell_at_its_prompt_is_not_a_command() {
        assert!(runs_a_command(&["/bin/zsh", "-c", "x"]));
        assert!(runs_a_command(&["bash", "-lc", "x"]));
        assert!(!runs_a_command(&["-zsh"]), "a terminal's own shell");
        assert!(!runs_a_command(&["zsh", "--login"]));
        assert!(!runs_a_command(&["cargo", "-c", "x"]));
        assert_eq!(elapsed("13-01:13:00"), Some(13 * 86_400 + 3600 + 13 * 60));
        assert_eq!(elapsed("00:04"), Some(4));
        assert_eq!(elapsed("bad"), None);
    }
}
