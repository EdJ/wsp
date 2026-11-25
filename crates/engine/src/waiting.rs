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
//!   the keyboard. That is the backend's reading and not this module's: backend
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
use crate::util;

/// What the seat is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// A prompt on its screen that only a keypress answers.
    Prompt,
    /// Its own `wsp ask`, by the question's id.
    Answer(String),
    /// Shells of its own still running, and how many.
    Shell(usize),
    /// Standing in its own `wsp ask --person --wait`, by the question's id
    /// (`wsp-300`). It is inside a tool call, so the screen says `working`:
    /// this is the only reading that counts it, and it counts whatever the
    /// runner's word is.
    Question(String),
}

/// Whom it is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum On {
    Person,
    /// The governor of this scope.
    Seat(String),
    /// Nobody: what it waits on is its own, and wakes it.
    Itself,
}

/// A seat that is waiting, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    pub why: Why,
    pub on: On,
    /// What it is waiting on, when that is a shell of its own (`wsp-536`):
    /// the work under it, how long, and how long it has been quiet.
    pub own: Option<Own>,
}

impl Wait {
    /// The word a column draws: `waiting · person`, `waiting · wsp-process`.
    pub fn word(&self) -> String {
        match &self.on {
            On::Person => "waiting · person".into(),
            On::Seat(scope) => format!("waiting · {scope}"),
            On::Itself => match self.own.as_ref().filter(|o| o.quiet >= SHOWN) {
                // Before the limit, so a person sees it going quiet (`wsp-536`).
                Some(o) => format!("waiting · itself · quiet {}", util::duration_human(o.quiet)),
                None => "waiting · itself".into(),
            },
        }
    }

    /// Why it is held and since when, in one line: `waiting on itself: cargo
    /// test (4m, busy)`. What `wip --json` carries as `reason`.
    pub fn reason(&self) -> String {
        match (&self.why, &self.own) {
            (Why::Shell(_), Some(o)) => {
                let work = if o.work.is_empty() { "a shell" } else { o.work.as_str() };
                let state = match o.quiet >= SHOWN {
                    true => format!("quiet {}", util::duration_human(o.quiet)),
                    false => "busy".to_string(),
                };
                format!("waiting on itself: {work} ({}, {state})", util::duration_human(o.age))
            }
            _ => self.sentence(),
        }
    }

    /// Whether somebody owes this seat something: false for a seat waiting on
    /// itself. A governor's `members_waiting`, `needs_you` and the run's word
    /// to the governor are each about a seat somebody has to answer, and a
    /// seat waiting on its own shell is held without anybody being told.
    pub fn owed(&self) -> bool {
        self.on != On::Itself
    }

    /// The reason, as a clause a log line can carry.
    pub fn sentence(&self) -> String {
        let whom = match &self.on {
            On::Person => "a person".to_string(),
            On::Seat(scope) => format!("the {scope} governor"),
            On::Itself => "itself".to_string(),
        };
        match &self.why {
            Why::Prompt => format!("stopped on a prompt only {whom} can answer"),
            Why::Answer(id) => format!("waiting on {whom} to answer its question {id}"),
            // In a tool call that waits on a person: `wsp ask --wait`, or a
            // dialog a hook holds for one (`wsp-301`).
            Why::Question(id) => format!("held in a tool call for {whom} to answer {id}"),
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
    pub fn json(&self) -> serde_json::Value {
        let (why, question, shells) = match &self.why {
            Why::Prompt => ("prompt", None, None),
            Why::Answer(id) => ("answer", Some(id.as_str()), None),
            Why::Question(id) => ("question", Some(id.as_str()), None),
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
        if let Some(o) = &self.own {
            v["work"] = o.work.as_str().into();
            v["age"] = o.age.into();
            v["quiet"] = o.quiet.into();
        }
        v["reason"] = self.reason().into();
        // §26.8 D9 names it `id`; `question` stays beside it as `answer`'s.
        if let (Why::Question(id), Some(o)) = (&self.why, v.as_object_mut()) {
            o.insert("id".into(), id.as_str().into());
        }
        v
    }
}

/// What a turn's last message said, read off the seat's screen (`wsp-581`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Words {
    /// The final paragraph, on one line and trimmed.
    pub text: String,
    /// Whether it carries a request: a line beginning "What I need", "Need" or
    /// "Either … or", or a question. A prompt for a judge, never a block.
    pub asks: bool,
}

/// The most of the last paragraph a tell carries, in characters.
const WORDS_SHOWN: usize = 360;

/// The final paragraph of a seat's screen, with the agent's own chrome left
/// out: the prompt box, its borders, the status footer, tool-result lines.
/// `None` where nothing readable is on it.
///
/// **No judgement over the wording** beyond [`Words::asks`]: the governor
/// reads the paragraph and decides (`wsp-581`).
pub fn last_words(screen: &str) -> Option<Words> {
    let mut block: Vec<String> = Vec::new();
    let mut last: Vec<String> = Vec::new();
    for raw in screen.lines() {
        let l = raw.trim();
        let l = l.trim_matches(|c| c == '│' || c == '┃').trim();
        if l.is_empty() || is_chrome(l) {
            if !block.is_empty() {
                last = std::mem::take(&mut block);
            }
            continue;
        }
        block.push(l.trim_start_matches(['⏺', '●']).trim().to_string());
    }
    if !block.is_empty() {
        last = block;
    }
    last.retain(|l| !l.is_empty());
    if last.is_empty() {
        return None;
    }
    let bare = |l: &str| l.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '?').to_lowercase();
    let joined = last.join(" ");
    let asks = last.iter().any(|l| {
        let b = bare(l);
        let word = |w: &str| b.strip_prefix(w).is_some_and(|r| r.chars().next().is_none_or(|c| !c.is_alphanumeric()));
        word("what i need") || word("need") || (word("either") && joined.to_lowercase().contains(" or "))
    }) || joined.contains('?');
    Some(Words { text: util::truncate(&joined, WORDS_SHOWN), asks })
}

/// What Claude Code writes in a `tool_result` when a call was refused — by the
/// person or by auto mode's classifier (`wsp-606`).
pub const DENIED: &str = "Permission for this action was denied";

/// How a turn ended, read off its transcript rather than its screen
/// (`wsp-606`): the turn's last assistant text, and whether any call in that
/// turn was denied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnEnd {
    /// When the turn ended — the seat's `Stop` hook, in epoch seconds.
    pub at: i64,
    /// The last paragraph of the turn's last assistant text, judged by
    /// [`last_words`]'s rule.
    pub words: Option<Words>,
    /// A `tool_result` in the turn carried [`DENIED`].
    pub denied: bool,
}

impl TurnEnd {
    /// Whether this turn ended wanting somebody: a request in its last words,
    /// or a call it was refused.
    pub fn wants(&self) -> bool {
        self.denied || self.words.as_ref().is_some_and(|w| w.asks)
    }
}

/// The most of a transcript's tail read for its last turn, in bytes.
const TRANSCRIPT_TAIL: u64 = 1 << 20;

/// The last turn of a Claude Code transcript (JSONL), from its last prompt on:
/// `(last words, denied)`. A prompt is a `user` line whose content is the
/// person's (or wsp's) text, not a `tool_result`.
pub fn last_turn(jsonl: &str) -> (Option<Words>, bool) {
    let mut text: Option<String> = None;
    let mut denied = false;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("user") => {
                let results = content.as_array().is_some_and(|a| a.iter().any(|b| b["type"] == "tool_result"));
                if results {
                    denied |= line.contains(DENIED);
                } else if v["isMeta"] != true {
                    text = None;
                    denied = false;
                }
            }
            Some("assistant") => {
                let said: Vec<&str> = content
                    .as_array()
                    .map(|a| a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect())
                    .unwrap_or_default();
                let said = said.join("\n\n");
                if !said.trim().is_empty() {
                    text = Some(said);
                }
            }
            _ => {}
        }
    }
    (text.as_deref().and_then(last_words), denied)
}

/// [`last_turn`] over the file at `path`, reading at most its last
/// [`TRANSCRIPT_TAIL`] bytes. `None` where it cannot be read.
pub fn read_last_turn(path: &std::path::Path) -> Option<(Option<Words>, bool)> {
    use crate::host::Host;
    // Read through the host, which drops a partial first line.
    Some(last_turn(&crate::host::LOCAL.transcript_tail(path, TRANSCRIPT_TAIL)?))
}

/// A screen line that is the agent's frame and not its words.
fn is_chrome(l: &str) -> bool {
    l.chars().all(|c| "─━│┃╭╮╰╯┌┐└┘├┤ -=_".contains(c))
        || l.starts_with(['>', '❯', '⏵', '✻', '✽', '✢', '·', '⎿'])
        || ["shell still running", "shells still running", "for shortcuts", "to interrupt", "bypass permissions", "auto-accept", "ctrl+"]
            .iter()
            .any(|f| l.to_lowercase().contains(f))
}

impl Wait {
    /// The one line a governor gets when a held member's turn ends
    /// (`wsp-581`): what the seat waits on and the last paragraph it wrote,
    /// and whether that paragraph is a request nothing raised. Only for a seat
    /// waiting on itself: every other wait already has its ask or prompt.
    pub fn turn_over(&self, id: &str, seat: &str, words: Option<&Words>) -> String {
        let (work, age) = match &self.own {
            Some(o) => (
                if o.work.is_empty() { "a shell".to_string() } else { o.work.clone() },
                util::duration_human(o.age),
            ),
            None => ("a shell".to_string(), "just now".to_string()),
        };
        let said = match words {
            Some(w) => format!("\"{}\"", w.text),
            None => "(nothing readable on its screen)".to_string(),
        };
        let mut line = format!("{id} ({seat}): turn over, waiting on its own `{work}` (`{age}`); its last words: {said}");
        if words.is_some_and(|w| w.asks) {
            line.push_str(&format!(
                " — that reads as a request and no ask is raised (an unraised ask, for you to judge; not blocked). `wsp peek {id}` shows its screen."
            ));
        }
        line
    }
}

/// Whether a hook is holding a dialog for this question: the person's,
/// whatever governs the row, because only they can answer it.
fn hook_held(m: &crate::message::Message) -> bool {
    m.for_person && m.choices.as_ref().is_some_and(|c| c.json().get("via").and_then(|v| v.as_str()) == Some("hook"))
}

/// Whom an open question waits on: the governor that answers for its task, or
/// the person when no seat is above it or a hook holds a dialog for it.
///
/// **One route, read by the line that says who is owed and by the verb that
/// lets them close it** (`wsp-608`). The barrier once said a member waited "on
/// the compounding governor" while `wsp answer` refused that governor, so a run
/// stood overnight on a question nobody it named could close. Ed, 2026-11-12:
/// *governors should be able to answer questions … that's the same as a raised
/// hand*. So the seat this names may answer it, and [`crate::cmd_message`]
/// asks this rather than a copy.
pub fn route(store: &Store, m: &crate::message::Message) -> On {
    let index = crate::resolve::Index::new(store.projects());
    let lists = crate::worklist::Running::read(store);
    route_with(store, m, &index, &lists, &store.governors())
}

fn route_with(
    store: &Store,
    m: &crate::message::Message,
    index: &crate::resolve::Index,
    lists: &crate::worklist::Running,
    governors: &BTreeMap<String, serde_json::Value>,
) -> On {
    if hook_held(m) {
        return On::Person;
    }
    m.about
        .task()
        .and_then(|id| store.find_task(id))
        .and_then(|t| {
            // The row's allocation too (`wsp-418`): past the project levels the
            // answer is its user's primary.
            let user = store.allocated_to(&t.id);
            crate::governors::seat_for(governors, index, lists.list_of(&t.id), t.project.as_deref(), user.as_deref())
        })
        .map(|s| On::Seat(s.scope))
        .unwrap_or(On::Person)
}

/// The most of a prompt's text a raise carries, in characters.
const PROMPT_SHOWN: usize = 600;

/// The most of a screen's last lines a prompt's text is read from.
const PROMPT_LINES: usize = 12;

/// The prompt on a seat's screen, as one line: the last [`PROMPT_LINES`] lines
/// with words on them, the frame's borders and the footer's key hints left out
/// and the options kept (`wsp-616`). A dialog is drawn at the foot of the
/// screen: an AskUserQuestion's question and numbered options, a permission
/// dialog's call and `1. Yes / 2. No`. `None` where nothing readable is on it.
pub fn prompt_text(screen: &str) -> Option<String> {
    let footer = ["enter to select", "esc to cancel", "to navigate", "for shortcuts", "ctrl+", "shift+tab"];
    let lines: Vec<&str> = screen
        .lines()
        .map(|raw| raw.trim().trim_matches(|c| "│┃".contains(c)).trim())
        .filter(|l| !l.is_empty() && !l.chars().all(|c| "─━│┃╭╮╰╯┌┐└┘├┤ -=_".contains(c)))
        .filter(|l| !footer.iter().any(|f| l.to_lowercase().contains(f)))
        .map(|l| l.trim_start_matches(['⏺', '●', '❯', '›']).trim())
        .filter(|l| !l.is_empty())
        .collect();
    let from = lines.len().saturating_sub(PROMPT_LINES);
    (from < lines.len()).then(|| util::truncate(&lines[from..].join(" · "), PROMPT_SHOWN))
}

/// **A run seat at an on-screen prompt is raised at once** (`wsp-616`,
/// compounding-449): a verifier sat at an AskUserQuestion with nothing
/// raised, because a prompt is read by nobody. To whom [`route`] names — the
/// governor that answers for the row, else the person — with the prompt's
/// text. `what` names the seat's work (`m-1`, `the v-1 verifier's pass`);
/// `screen` is its screen, when it could be read; `list` is the run the seat
/// is in, whose governor hears it the way the run tells it everything else
/// ([`crate::cycle::tell`]) when the route names that governor. Returns whom
/// it went to.
///
/// A note rather than a question: a question with this seat behind it would
/// hold the seat as waiting on an answer long after the dialog is gone.
pub fn raise_prompt(
    store: &Store,
    seat: &str,
    about: crate::message::About,
    what: &str,
    screen: Option<&str>,
    list: Option<&crate::model::Worklist>,
) -> On {
    use crate::message::{self, Kind, Message, Party};
    let said = match screen.and_then(prompt_text) {
        Some(p) => format!("its screen reads: \"{p}\""),
        None => "its screen could not be read".to_string(),
    };
    let said = match screen.and_then(crate::local_trust::dialog_fix) {
        Some(fix) => format!("{said}. That is {fix}"),
        None => said,
    };
    let text = format!(
        "{what} ({seat}) is stopped on a prompt on its screen — an AskUserQuestion or a permission dialog — and nothing \
         moves until it is answered; {said}. Only a keypress at the seat answers it: `wsp peek --pane {seat}` shows it."
    );
    let mut m = Message::new(Party::wsp(), Kind::Note, &text).about(about);
    let on = route(store, &m);
    match &on {
        On::Seat(scope) => m.for_seat = Some(scope.clone()),
        _ => m = m.for_the_person(),
    }
    if let Err(e) = message::raise(store, &m) {
        crate::store::log_line(store, &format!("{what} ({seat}): its prompt could not be raised: {e}"));
        return on;
    }
    crate::store::log_line(store, &format!("{what} ({seat}) is at a prompt on its screen — raised to {}", match &on {
        On::Seat(scope) => format!("the {scope} governor"),
        _ => "the person".to_string(),
    }));
    if let On::Seat(scope) = &on {
        match list.filter(|w| &w.id == scope) {
            Some(w) => crate::cycle::tell(store, w, &text),
            None => crate::cmd_message::say_to_governor(store, scope, &m),
        }
    }
    on
}

/// The open questions, read once, indexed by who is sitting still behind each.
///
/// **Read once per pass and asked per seat**, because every reader here walks
/// many seats and `messages.json` is one file: the reconciler, a `wip` and a
/// watch tick each build one of these and ask it for every row.
#[derive(Debug, Clone, Default)]
pub struct Asks {
    by_pane: BTreeMap<String, (String, On)>,
    by_task: BTreeMap<String, (String, On)>,
    /// The person's questions a live `wsp ask --wait` is blocked on, by the
    /// asker's pane and by its task.
    awaited_pane: BTreeMap<String, String>,
    awaited_task: BTreeMap<String, String>,
    /// The ones a hook is holding a dialog for, by the same keys.
    held_by_pane: BTreeMap<String, String>,
    held_by_task: BTreeMap<String, String>,
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
    pub fn read(store: &Store) -> Asks {
        Asks::of_messages(store, store.messages(), &|pid| crate::host::Host::is_alive(&crate::host::LOCAL, pid))
    }

    /// [`Asks::read`] over `messages` read elsewhere (`wip`'s API path reads
    /// them from a `state` reply), with `alive` saying whether a waiter's pid
    /// still runs.
    pub fn of_messages(store: &Store, messages: BTreeMap<String, crate::message::Message>, alive: &dyn Fn(u32) -> bool) -> Asks {
        let mut asks = Asks::default();
        let open: Vec<_> = messages
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
            // A hook-held dialog is the person's whatever governs the row:
            // only they can answer it, and a seat's guard refuses it.
            let held = hook_held(m);
            let on = route_with(store, m, &index, &lists, &governors);
            // Oldest first and the later one wins, so the question a seat names
            // is its most recent — the one it is most likely sitting on.
            if !w.pane.is_empty() {
                asks.by_pane.insert(w.pane.clone(), (m.id.clone(), on.clone()));
                if held {
                    asks.held_by_pane.insert(w.pane.clone(), m.id.clone());
                }
            }
            if !w.task.is_empty() {
                asks.by_task.insert(w.task.clone(), (m.id.clone(), on));
                if held {
                    asks.held_by_task.insert(w.task.clone(), m.id.clone());
                }
            }
            // A wait stands only while its process does: a waiter killed with
            // Esc leaves the mark behind, and the seat is then at its prompt
            // like any asker.
            if m.for_person && m.awaited.as_ref().is_some_and(|a| alive(a.pid)) {
                if !w.pane.is_empty() {
                    asks.awaited_pane.insert(w.pane.clone(), m.id.clone());
                }
                if !w.task.is_empty() {
                    asks.awaited_task.insert(w.task.clone(), m.id.clone());
                }
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
    pub fn of(&self, seat: &str, task: &str) -> Option<Wait> {
        if let Some(w) = self.held(seat, task) {
            return Some(w);
        }
        let hit = self.by_pane.get(seat).or_else(|| (!task.is_empty()).then(|| self.by_task.get(task)).flatten())?;
        Some(Wait { why: Why::Answer(hit.0.clone()), on: hit.1.clone(), own: None })
    }

    /// The person's question this seat is blocked on in `wsp ask --wait`.
    pub fn awaited(&self, seat: &str, task: &str) -> Option<Wait> {
        let id = self.awaited_pane.get(seat).or_else(|| (!task.is_empty()).then(|| self.awaited_task.get(task)).flatten())?;
        Some(Wait { why: Why::Question(id.clone()), on: On::Person, own: None })
    }

    /// The dialog a hook is holding for this seat, if one is: a person's to
    /// answer, and held whatever state the seat reads.
    pub fn held(&self, seat: &str, task: &str) -> Option<Wait> {
        let id = self.held_by_pane.get(seat).or_else(|| (!task.is_empty()).then(|| self.held_by_task.get(task)).flatten())?;
        Some(Wait { why: Why::Question(id.clone()), on: On::Person, own: None })
    }

    #[cfg(test)]
    pub fn waiter(pane: &str, task: &str, id: &str) -> Asks {
        let mut a = Asks::one(pane, task, id, On::Person);
        a.awaited_pane.insert(pane.to_string(), id.to_string());
        a.awaited_task.insert(task.to_string(), id.to_string());
        a
    }

    #[cfg(test)]
    pub fn held_one(pane: &str, task: &str, id: &str) -> Asks {
        let mut a = Asks::one(pane, task, id, On::Person);
        a.held_by_pane.insert(pane.to_string(), id.to_string());
        a.held_by_task.insert(task.to_string(), id.to_string());
        a
    }

    #[cfg(test)]
    pub fn one(pane: &str, task: &str, id: &str, on: On) -> Asks {
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
/// another one. Bounds, all from what else looks like that:
///
/// - **At least [`SETTLED`] old.** A hook and the status line are shells under
///   the agent too, and live for a moment. A background command younger than
///   that is read on the next pass.
/// - **Younger than [`LEFT_RUNNING`].** A shell the agent has not seen end in
///   an hour is a dev server it left running, not a command it is waiting on.
/// - **With work under it** (`wsp-536`, Ed 2026-11-08: *we're getting a lot of
///   delays when agents finish their turn*). The shell's own tree is read at
///   the turn's end: a compiler, a test binary, `cargo`, a server is work; a
///   `sleep`, a `tail -f`, a `grep`, a `pgrep` is a watcher, and a shell with
///   only watchers under it, or nothing, is a dead wait that wakes nobody
///   (`wsp-530` sat on one with no process in its tree at all). One
///   [`crate::stuck`] names as waiting on itself — an `until … pgrep -f` loop
///   that matches its own line — never ends by itself and never counts.
/// - **Held until it ends, once it has been seen working** (`compound-react-041`).
///   A shell read with work under it is remembered by its pid and start, and
///   holds the seat on every later pass while that process lives: when only a
///   watcher is under it for a moment (a poll loop between its calls), and
///   when its work uses no CPU (blocked on cargo's build lock, on a server, on
///   a sandbox). The rule this replaced released the hold after a minute with
///   no CPU, and `compound-react-024` lost a 32-minute attempt to it: held at
///   21:56:42, told at 21:57:42 with its `sh` still running, ended ten minutes
///   later with seven uncommitted paths. A hung test now holds the seat until
///   [`LEFT_RUNNING`]; the wait shows `quiet` from [`SHOWN`] on, so a person
///   sees it. CPU is read as the tree's total cputime against the last
///   reading, kept with the held shells in `shell-cpu.json` in the state dir.
///
/// The seat whose shells have all ended reads idle on that pass, and the normal
/// path applies: told to finish, then ended and reseated.
///
/// A backend that cannot name a root, and a far machine whose process table
/// is not this one, read as no shells: the seat is idle as before.
#[derive(Debug, Clone, Default)]
pub struct Shells {
    by_seat: BTreeMap<String, Own>,
}

/// What a seat's own shells are doing, from the process tree under them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Own {
    /// How many shells.
    pub n: usize,
    /// The work under them, `cargo test`; empty when a test names none.
    pub work: String,
    /// How long the oldest has run, in seconds.
    pub age: i64,
    /// How long the tree has used no CPU, in seconds.
    pub quiet: i64,
}

/// How long a shell has to have been running to be one the agent is waiting
/// on, in seconds. See [`Shells`].
const SETTLED: i64 = 5;

/// How long a shell may run and still be waited on, in seconds. See [`Shells`].
const LEFT_RUNNING: i64 = 60 * 60;

/// From how quiet the wait says so, in seconds.
const SHOWN: i64 = 10;

/// A reading older than this is not a baseline: the tree is read afresh.
const GAP: i64 = 120;

/// The shells that run a command.
const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "fish"];

/// Programs that wait or look and do no work: what a `while`/`until` loop is
/// made of. A tree of only these holds nothing.
const WATCHERS: [&str; 20] = [
    "sleep", "tail", "grep", "egrep", "fgrep", "pgrep", "ps", "cat", "head", "wc", "awk", "sed", "stat", "ls", "date", "test", "[", "true",
    "timeout", "tee",
];

/// The cputime each seat's tree showed at the last reading, so a later one can
/// say how long it has been the same.
#[derive(Debug, Clone, Default)]
pub struct Memory {
    /// seat → (cpu ms, when it last changed, when it was last read).
    by_seat: BTreeMap<String, (i64, i64, i64)>,
    /// seat → the shells it was last held on: (pid, when it started, the work
    /// seen under it). A shell here holds until it ends (`compound-react-041`).
    held: BTreeMap<String, Vec<(u32, i64, String)>>,
}

/// How far two readings of one shell's start may differ, in seconds: `etime`
/// and the clock are each whole seconds.
const SAME_START: i64 = 2;

impl Memory {
    fn path() -> std::path::PathBuf {
        let state = match std::env::var_os("WSP_STATE") {
            Some(v) => std::path::PathBuf::from(v),
            None => Store::default_state(),
        };
        state.join("shell-cpu.json")
    }

    fn load() -> Memory {
        let read = std::fs::read_to_string(Memory::path()).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        let Some(serde_json::Value::Object(map)) = read else { return Memory::default() };
        let mut memory = Memory::default();
        for (seat, v) in map {
            let Some(a) = v.as_array() else { continue };
            let (Some(c), Some(ch), Some(seen)) =
                (a.first().and_then(|v| v.as_i64()), a.get(1).and_then(|v| v.as_i64()), a.get(2).and_then(|v| v.as_i64()))
            else {
                continue;
            };
            // The fourth field is the held shells; a file from before it has none.
            let held: Vec<(u32, i64, String)> = a
                .get(3)
                .and_then(|v| v.as_array())
                .map(|hs| {
                    hs.iter()
                        .filter_map(|h| {
                            let h = h.as_array()?;
                            Some((h.first()?.as_u64()? as u32, h.get(1)?.as_i64()?, h.get(2)?.as_str()?.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !held.is_empty() {
                memory.held.insert(seat.clone(), held);
            }
            memory.by_seat.insert(seat, (c, ch, seen));
        }
        memory
    }

    fn save(&mut self, now: i64) {
        self.by_seat.retain(|_, (_, _, seen)| now - *seen < 600);
        let by_seat = &self.by_seat;
        self.held.retain(|seat, _| by_seat.contains_key(seat));
        let map: serde_json::Map<String, serde_json::Value> = self
            .by_seat
            .iter()
            .map(|(k, (c, ch, seen))| {
                let held: Vec<serde_json::Value> =
                    self.held.get(k).into_iter().flatten().map(|(pid, start, work)| serde_json::json!([pid, start, work])).collect();
                (k.clone(), serde_json::json!([c, ch, seen, held]))
            })
            .collect();
        let path = Memory::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension(format!("json.{}", std::process::id()));
        if std::fs::write(&tmp, serde_json::Value::Object(map).to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// How long this seat's tree has shown `cpu`, and the reading kept.
    fn quiet(&mut self, seat: &str, cpu: i64, now: i64) -> i64 {
        let since = match self.by_seat.get(seat) {
            Some((c, changed, seen)) if *c == cpu && now - *seen <= GAP => *changed,
            _ => now,
        };
        self.by_seat.insert(seat.to_string(), (cpu, since, now));
        now - since
    }

    /// The work this seat was held on under the shell `pid` that started at
    /// `start`, when it was held on that shell at the last reading.
    fn was_held(&self, seat: &str, pid: u32, start: i64) -> Option<&str> {
        self.held
            .get(seat)?
            .iter()
            .find(|(p, s, _)| *p == pid && (s - start).abs() <= SAME_START)
            .map(|(_, _, work)| work.as_str())
    }
}

impl Shells {
    /// These seats' shells, off the local backends' roots and one `ps`. No
    /// `ps` is run when none of them has a root here.
    ///
    /// Asked only about seats that read `Idle`, by every caller: a seat in a
    /// turn is working whatever it has running, and the reading is a process
    /// table that `wsp wip` would otherwise pay for on every poll.
    pub fn read<'a>(seats: impl IntoIterator<Item = &'a str>, self_wait: &dyn Fn(&str) -> bool) -> Shells {
        // A test's seat ids are not this machine's, and a fake backend's are
        // not anybody's: a test hands the index in with [`Shells::one`].
        if cfg!(test) {
            return Shells::default();
        }
        let backends = crate::place::local_backends();
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
        match crate::host::Host::shells(&crate::host::LOCAL) {
            Some(ps) => {
                let now = util::epoch_secs();
                let mut memory = Memory::load();
                let shells = Shells::of(&ps, &roots, now, &mut memory, self_wait);
                memory.save(now);
                shells
            }
            // Not "waiting": a `ps` that did not run is no evidence of a shell,
            // and the cost of reading none is a seat told, which is a sentence.
            None => Shells::default(),
        }
    }

    /// The reading over a `ps -o pid=,ppid=,etime=,time=,command=` table.
    fn of(ps: &str, roots: &[(String, u32)], now: i64, memory: &mut Memory, self_wait: &dyn Fn(&str) -> bool) -> Shells {
        struct Row {
            pid: u32,
            ppid: u32,
            age: i64,
            cpu: i64,
            shell: bool,
            command: String,
        }
        let rows: Vec<Row> = ps
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let pid = it.next()?.parse().ok()?;
                let ppid = it.next()?.parse().ok()?;
                let age = elapsed(it.next()?)?;
                let cpu = cpu_ms(it.next()?)?;
                let command: Vec<&str> = it.collect();
                Some(Row { pid, ppid, age, cpu, shell: runs_a_command(&command), command: command.join(" ") })
            })
            .collect();
        let shell: std::collections::BTreeSet<u32> = rows.iter().filter(|r| r.shell).map(|r| r.pid).collect();
        let below = |root: u32| -> Vec<u32> {
            let mut tree = vec![root];
            let mut i = 0;
            while i < tree.len() {
                let parent = tree[i];
                tree.extend(rows.iter().filter(|r| r.ppid == parent && !tree.contains(&r.pid)).map(|r| r.pid).collect::<Vec<_>>());
                i += 1;
            }
            tree
        };
        let mut by_seat = BTreeMap::new();
        for (seat, root) in roots {
            let tree = below(*root);
            let mine: Vec<&Row> = rows.iter().filter(|r| r.pid != *root && tree.contains(&r.pid)).collect();
            let (mut n, mut age, mut cpu, mut work) = (0, 0, 0, String::new());
            let mut holding: Vec<(u32, i64, String)> = Vec::new();
            for sh in mine.iter().filter(|r| r.shell && !shell.contains(&r.ppid)).filter(|r| (SETTLED..LEFT_RUNNING).contains(&r.age)) {
                // A pgrep guard that matches itself ends only when the tool's
                // limit ends it: it holds nothing (`stuck::waits_on_itself_line`, passed in: `waiting` sits below `stuck`).
                if self_wait(&ran(&sh.command)) {
                    continue;
                }
                let under = below(sh.pid);
                let doing: Vec<&&Row> = mine.iter().filter(|r| r.pid != sh.pid && under.contains(&r.pid) && !r.shell).collect();
                let works: Vec<&&&Row> = doing.iter().filter(|r| !is_watcher(&r.command)).collect();
                // Seen working once, it holds until it ends (`compound-react-041`):
                // a watcher alone under it now is a poll between its calls.
                let start = now - sh.age;
                let named = match (works.first(), memory.was_held(seat, sh.pid, start)) {
                    (Some(w), _) => name_of_work(&w.command),
                    (None, Some(before)) => before.to_string(),
                    (None, None) => continue,
                };
                n += 1;
                age = age.max(sh.age);
                cpu += doing.iter().map(|r| r.cpu).sum::<i64>();
                if work.is_empty() {
                    work = named.clone();
                }
                holding.push((sh.pid, start, named));
            }
            if n == 0 {
                memory.by_seat.remove(seat);
                memory.held.remove(seat);
                continue;
            }
            // Quiet is said, and holds as long as busy (`compound-react-041`).
            let quiet = memory.quiet(seat, cpu, now);
            memory.held.insert(seat.clone(), holding);
            by_seat.insert(seat.clone(), Own { n, work, age, quiet });
        }
        Shells { by_seat }
    }

    /// This seat's own shells, as a wait — `None` when it has none.
    pub fn of_seat(&self, seat: &str) -> Option<Wait> {
        self.by_seat.get(seat).map(|o| Wait { why: Why::Shell(o.n), on: On::Itself, own: Some(o.clone()) })
    }

    /// The shells each seat's manager counted under its agent (`seats`'s
    /// `shells`, `docs/agent-manager.md` 4.3), for the idle seats asked: no
    /// `ps` here. A count alone: the manager does not yet say the work under a
    /// shell, its age or its quiet, so the bounds [`Shells`] names are the
    /// manager's to keep.
    pub fn counted<'a>(seats: impl IntoIterator<Item = &'a str>, counts: &BTreeMap<String, u32>) -> Shells {
        let by_seat = seats
            .into_iter()
            .filter_map(|s| counts.get(s).filter(|n| **n > 0).map(|n| (s.to_string(), Own { n: *n as usize, ..Own::default() })))
            .collect();
        Shells { by_seat }
    }

    #[cfg(test)]
    pub fn own(seat: &str, own: Own) -> Shells {
        Shells { by_seat: BTreeMap::from([(seat.to_string(), own)]) }
    }

    #[cfg(test)]
    pub fn one(seat: &str, n: usize) -> Shells {
        Shells { by_seat: BTreeMap::from([(seat.to_string(), Own { n, ..Own::default() })]) }
    }
}

/// **A seat's shells that wait on themselves, ended** (`wsp-611`): a told seat
/// in a Bash call reads the telling only when the call returns, and a loop on
/// `pgrep -f "<its own pattern>"` returns at the tool's 600 s limit. Ending that
/// one shell returns the call now, with the telling next. The pids ended; none
/// when the seat has no root here or no such shell.
pub fn end_self_waits(seat: &str, self_wait: &dyn Fn(&str) -> bool) -> Vec<u32> {
    if cfg!(test) {
        return Vec::new();
    }
    let s = crate::place::Seat::new(seat);
    let Some(root) = crate::place::local_backends().iter().find_map(|b| b.root(&s)) else { return Vec::new() };
    use crate::host::{Host, LOCAL};
    let Some(out) = LOCAL.table("pid=,ppid=,command=", false) else { return Vec::new() };
    let pids = self_wait_shells(&out, root, self_wait);
    LOCAL.signal(&pids, "TERM");
    pids
}

/// Over a `ps -o pid=,ppid=,command=` table: the outermost shells under `root`
/// whose command waits on itself. Not `root`, and not a shell inside one of
/// them (the `$(…)` a loop forks has the same line).
fn self_wait_shells(ps: &str, root: u32, self_wait: &dyn Fn(&str) -> bool) -> Vec<u32> {
    let rows: Vec<(u32, u32, String, bool)> = ps
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let command: Vec<&str> = it.collect();
            Some((pid, ppid, command.join(" "), runs_a_command(&command)))
        })
        .collect();
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        tree.extend(rows.iter().filter(|r| r.1 == parent && !tree.contains(&r.0)).map(|r| r.0).collect::<Vec<_>>());
        i += 1;
    }
    let waiting: Vec<u32> = rows
        .iter()
        .filter(|r| r.0 != root && tree.contains(&r.0) && r.3 && self_wait(&ran(&r.2)))
        .map(|r| r.0)
        .collect();
    waiting.iter().copied().filter(|pid| !rows.iter().any(|r| r.0 == *pid && waiting.contains(&r.1))).collect()
}

/// The command a `ps` line for Claude Code's shell actually runs: what is
/// inside `eval '…'`, un-quoted, or the whole line when there is no `eval`.
fn ran(command: &str) -> String {
    let Some(i) = command.find("eval ") else { return command.to_string() };
    let mut chars = command[i + 5..].chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    out.push(c);
                }
            }
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        c => out.push(c),
                    }
                }
            }
            '\\' => out.extend(chars.next()),
            c if c.is_whitespace() => break,
            c => out.push(c),
        }
    }
    match out.is_empty() {
        true => command.to_string(),
        false => out,
    }
}

fn program(command: &str) -> &str {
    let argv0 = command.split_whitespace().next().unwrap_or("");
    argv0.rsplit('/').next().unwrap_or(argv0)
}

fn is_watcher(command: &str) -> bool {
    WATCHERS.contains(&program(command))
}

/// `cargo test`, not `cargo test --release -p x`: the program and its verb.
fn name_of_work(command: &str) -> String {
    let mut it = command.split_whitespace().skip(1);
    let verb = it.next().filter(|w| !w.starts_with('-') && !w.contains('/') && w.chars().count() <= 16);
    match verb {
        Some(v) => format!("{} {v}", program(command)),
        None => program(command).to_string(),
    }
}

/// `ps`'s `time`, `[[dd-]hh:]mm:ss[.cc]`, in milliseconds.
fn cpu_ms(time: &str) -> Option<i64> {
    let (days, rest) = match time.split_once('-') {
        Some((d, r)) => (d.parse::<i64>().ok()?, r),
        None => (0, time),
    };
    let mut secs = 0.0f64;
    for part in rest.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86_400_000 + (secs * 1000.0).round() as i64)
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
pub fn reading(state: Option<State>, asks: &Asks, shells: &Shells, seat: &str, task: &str) -> Option<Wait> {
    let state = state?;
    // A dialog a hook holds for the person is the seat's whole story whatever
    // the screen says — `working` (a tool call is in flight) or `blocked` (the
    // pane's own copy of the dialog is drawn) — and it wins over both. A seat
    // that is gone has nobody left to wait.
    if !matches!(state, State::Gone | State::Empty) {
        if let Some(w) = asks.held(seat, task) {
            return Some(w);
        }
    }
    // A seat blocked in `wsp ask --wait` is `Working` to the screen: its tool
    // call is running. The prompt still wins (below), and a seat that is gone
    // waits on nothing.
    if !matches!(state, State::Blocked | State::Gone) {
        if let Some(w) = asks.awaited(seat, task) {
            return Some(w);
        }
    }
    match state {
        State::Idle => asks.of(seat, task).or_else(|| shells.of_seat(seat)),
        other => on_screen(other),
    }
}

/// The screen half alone, for the one reader that refuses only it: `wsp tell`.
///
/// A sentence typed at a dialog selects an answer nobody chose, so `tell`
/// refuses a prompt. It does *not* refuse a seat waiting on its own question,
/// because that is how an answer arrives.
pub fn on_screen(state: State) -> Option<Wait> {
    (state == State::Blocked).then_some(Wait { why: Why::Prompt, on: On::Person, own: None })
}

// ---- the seat's whole wait, in one field (`wsp-552`) ----------------------

/// What the run has said to a member's seat since its newest start, read off
/// the row's log by [`crate::repair::run_said`]: each the epoch it was said.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Said {
    /// Told to finish, land and review, or say what is left.
    pub told: Option<i64>,
    /// Sent back, or its seat ended by a governor for the run to reseat.
    pub sent_back: Option<i64>,
    /// A send-back whose reason did not reach the pane.
    pub undelivered: Option<i64>,
}

/// Which wait a seat is in: the `kind` of `wip --json`'s `wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A turn in flight.
    Working,
    /// Its own shell, which wakes it.
    Itself,
    /// A prompt or a question only a person answers.
    Person,
    /// A question routed to a governor.
    Governor,
    /// A row or list its own row rests on (`after:`) that has not finished.
    OtherWork,
    /// Stopped, and nothing said to it yet.
    Idle,
    /// Stopped, and told to finish by the run.
    Told,
    /// Ended after a send-back or a governor's despawn; the run starts it again.
    Reseating,
    /// Nothing typed reaches it: a send-back that did not land, a prompt with
    /// text nobody sent, a first order not received, a far machine gone
    /// silent, or a shell call that never ends ([`crate::stuck`]).
    CannotBeTold,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Working => "working",
            Kind::Itself => "itself",
            Kind::Person => "person",
            Kind::Governor => "governor",
            Kind::OtherWork => "other_work",
            Kind::Idle => "idle",
            Kind::Told => "told",
            Kind::Reseating => "reseating",
            Kind::CannotBeTold => "cannot_be_told",
        }
    }
}

/// A seat's wait as `wip --json` publishes it, and the line `wip` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatWait {
    pub kind: Kind,
    /// What it waits on: a question id, a governor's scope, a row or list, the
    /// work under its shell, the stuck call's key. `None` where it is nobody.
    pub target: Option<String>,
    /// Since when, as an epoch; published as ISO.
    pub since: Option<i64>,
    /// How long the work under its own shell has used no CPU — set only on
    /// `itself`.
    pub quiet_secs: Option<i64>,
    /// One line, with its age: `waiting on itself: cargo nextest (4m, busy)`.
    pub line: String,
}

impl SeatWait {
    /// `{kind, target, since, quiet_secs, line}`, the field names compound's
    /// census reads (compounding-208).
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind.key(),
            "target": self.target,
            "since": self.since.map(util::iso_at),
            "quiet_secs": self.quiet_secs,
            "line": self.line,
        })
    }
}

/// What one row of `wip` knows about its seat, for [`seat_wait`].
#[derive(Debug, Clone)]
pub struct Facts<'a> {
    pub state: State,
    /// [`reading`]'s answer for the seat.
    pub waiting: Option<&'a Wait>,
    /// When the seat came into its state, as an epoch.
    pub in_state_since: Option<i64>,
    /// A shell call [`crate::stuck`] named: its key, its word, how long held.
    pub stuck: Option<(&'static str, &'static str, i64)>,
    pub said: Said,
    /// `unreachable 4m`, for a seat on a far machine gone silent.
    pub unreachable: Option<String>,
    pub unsent: bool,
    /// The seat's supervisor has refused a tell this many ticks running, and the
    /// last reason it gave (`wsp-634`). Nothing was typed, so it is not `unsent`.
    pub refusing: Option<(u64, String)>,
    pub dropped: bool,
    /// The first row or list its own row rests on that has not finished, and
    /// that one's standing: `("pieces-trial", "planned: alone")`.
    pub rests_on: Option<(String, String)>,
    pub now: i64,
}

/// ` 4m`, or nothing where the age is unknown or not yet a second.
fn aged(now: i64, since: Option<i64>) -> String {
    match since.map(|s| now - s) {
        Some(secs) if secs > 0 => format!(" {}", util::duration_human(secs)),
        _ => String::new(),
    }
}

/// **The one line per seat** (`wsp-552`, wsp-536's item 5): which wait a seat
/// is in, on what, since when — from [`reading`], the stuck reading, the run's
/// own marks on the row, and what its row rests on.
///
/// In this order, because each one earlier is the truer story: a wait somebody
/// owes or its own shell; then whether anything typed can reach it at all; a
/// turn in flight; and only then the stopped seat's words — ended for a
/// reseat, resting on other work, told to finish, or plain idle. `None` for a
/// seat starting up, or gone with nothing the run is doing about it.
pub fn seat_wait(f: &Facts) -> Option<SeatWait> {
    let now = f.now;
    if let Some(w) = f.waiting {
        let since = match &w.own {
            Some(o) => Some(now - o.age),
            None => f.in_state_since,
        };
        let age = aged(now, since);
        let what = match &w.why {
            Why::Prompt => "a prompt on its screen".to_string(),
            Why::Answer(id) => format!("its question {id}"),
            Why::Question(id) => format!("held in a tool call for {id}"),
            Why::Shell(_) => String::new(),
        };
        let question = match &w.why {
            Why::Answer(id) | Why::Question(id) => Some(id.clone()),
            _ => None,
        };
        return Some(match &w.on {
            On::Itself => SeatWait {
                kind: Kind::Itself,
                target: w.own.as_ref().map(|o| o.work.clone()).filter(|s| !s.is_empty()),
                since,
                quiet_secs: w.own.as_ref().map(|o| o.quiet),
                line: w.reason(),
            },
            On::Person => SeatWait {
                kind: Kind::Person,
                target: question,
                since,
                quiet_secs: None,
                line: format!("waiting on a person{age}: {what}"),
            },
            On::Seat(scope) => SeatWait {
                kind: Kind::Governor,
                target: Some(scope.clone()),
                since,
                quiet_secs: None,
                line: format!("waiting on the {scope} governor{age}: {what}"),
            },
        });
    }
    let cannot = |target: &str, since: Option<i64>, why: String| SeatWait {
        kind: Kind::CannotBeTold,
        target: Some(target.to_string()),
        since,
        quiet_secs: None,
        line: format!("cannot be told: {why}"),
    };
    if let Some((key, word, secs)) = f.stuck {
        return Some(cannot(key, Some(now - secs), format!("{word} {}", util::duration_human(secs))));
    }
    if f.state == State::Unreachable {
        let word = f.unreachable.clone().unwrap_or_else(|| "unreachable".to_string());
        return Some(cannot("unreachable", f.in_state_since, word));
    }
    if let Some(when) = f.said.undelivered.filter(|_| f.state != State::Working) {
        return Some(cannot("send-back", Some(when), format!("the send-back {} ago did not reach its pane", util::duration_human(now - when))));
    }
    if f.state == State::Working {
        return Some(SeatWait {
            kind: Kind::Working,
            target: None,
            since: f.in_state_since,
            quiet_secs: None,
            line: format!("working{}", aged(now, f.in_state_since)),
        });
    }
    if let Some((n, why)) = &f.refusing {
        return Some(cannot("refusing", f.in_state_since, format!("refusing delivery, {n} ticks running: {why}")));
    }
    if f.unsent {
        return Some(cannot("unsent", f.in_state_since, "typed, not sent".to_string()));
    }
    if f.dropped {
        return Some(cannot("first-order", f.in_state_since, "its first order was not received".to_string()));
    }
    if let (Some(when), State::Gone) = (f.said.sent_back, f.state) {
        return Some(SeatWait {
            kind: Kind::Reseating,
            target: None,
            since: Some(when),
            quiet_secs: None,
            line: format!("sent back {} ago: reseating", util::duration_human((now - when).max(0))),
        });
    }
    if f.state != State::Idle {
        return None;
    }
    if let Some((target, standing)) = &f.rests_on {
        return Some(SeatWait {
            kind: Kind::OtherWork,
            target: Some(target.clone()),
            since: f.in_state_since,
            quiet_secs: None,
            line: match aged(now, f.in_state_since) {
                a if a.is_empty() => format!("waiting on {target} ({standing})"),
                a => format!("waiting on {target} ({standing},{a})"),
            },
        });
    }
    let idle = format!("idle since turn end{}", aged(now, f.in_state_since));
    Some(match f.said.told {
        Some(when) => SeatWait {
            kind: Kind::Told,
            target: None,
            since: Some(when),
            quiet_secs: None,
            line: format!("{idle}: told to finish"),
        },
        None => SeatWait { kind: Kind::Idle, target: None, since: f.in_state_since, quiet_secs: None, line: idle },
    })
}

/// Why a pane would not take a wake, in the words `--status` and `doctor`
/// print.
///
/// **One sentence per state rather than one for all of them**, because they
/// want four different repairs and a reader who cannot tell them apart goes
/// looking in the wrong place: mid-turn clears itself and is not a fault at
/// all, `Blocked` needs a person at a keyboard, `Starting` clears itself in
/// seconds, and `Unknown` means the seat could not be read — which is a fault
/// about the *reporter* and not about the seat.
///
/// No wildcard, for [`crate::cmd_watch::Line::disposition`]'s reason: a state
/// added later has to come past here and say what a wake does about it, rather
/// than inheriting a sentence that was written before it existed.
pub fn held_because(state: crate::place::State) -> Option<&'static str> {
    use crate::place::State;
    match state {
        // The one state a wake may be typed into, and it is
        // [`crate::place::State::will_take_a_prompt`]'s whole definition. This
        // function is the only thing [`Tell::deliver`] asks, so a test that
        // enumerates it is reading the gate rather than a copy of it.
        State::Idle => None,
        State::Working => Some("the seat is mid-turn"),
        State::Blocked => Some("the seat is stopped on a prompt only a person can answer"),
        State::Starting => Some("the agent is still coming up"),
        State::Empty | State::Gone => Some("the seat is empty"),
        State::Unreachable => Some("the seat is unreachable — held until the link returns"),
        State::Unknown => Some("wsp cannot say what the seat is doing"),
    }
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
    fn a_dialog_a_hook_holds_is_a_question_on_the_person_whatever_the_seat_reads() {
        let asks = Asks::held_one("cpd-1", "wsp-1", "q-1");
        for state in [State::Working, State::Blocked, State::Idle] {
            let w = reading(Some(state), &asks, &Shells::default(), "cpd-1", "wsp-1").unwrap_or_else(|| panic!("{state:?}"));
            assert_eq!((w.why.clone(), w.on.clone()), (Why::Question("q-1".into()), On::Person), "{state:?}");
        }
        // A seat that is gone has nobody left to wait; another task's is not its.
        for state in [State::Gone, State::Empty] {
            assert!(reading(Some(state), &asks, &Shells::default(), "cpd-1", "wsp-1").is_none(), "{state:?}");
        }
        assert!(reading(Some(State::Working), &asks, &Shells::default(), "cpd-2", "wsp-2").is_none());
        // The same seat for a plain ask is still only waiting when idle.
        let plain = Asks::one("cpd-1", "wsp-1", "q-1", On::Person);
        assert!(reading(Some(State::Working), &plain, &Shells::default(), "cpd-1", "wsp-1").is_none());
        // The published shape is compound's: why, on, id.
        let json = reading(Some(State::Working), &asks, &Shells::default(), "cpd-1", "wsp-1").unwrap().json();
        assert_eq!((json["why"].as_str(), json["on"].as_str(), json["id"].as_str()), (Some("question"), Some("person"), Some("q-1")));
        assert_eq!(json["question"], "q-1", "the key an older reader had is kept");
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
    1     0 13-01:13:00     0:09.00 /sbin/launchd
56312     1    02:03:00     0:01.00 /Users/ed/.local/bin/compound-sup
56320 56312    02:02:58     1:00.00 claude --strict-mcp-config -n wsp-214-cpd-1025 --model opus
56558 56320       00:04     0:00.01 caffeinate -i -t 300
64546 56320       12:31     0:00.10 /bin/zsh -c source /Users/ed/.claude/shell-snapshots/snapshot-zsh-1.sh && eval 'wsp verify --release'
64550 64546       12:30     0:00.05 /bin/zsh -c source /Users/ed/.claude/shell-snapshots/snapshot-zsh-1.sh && eval 'wsp verify --release'
64551 64550       12:30     9:30.50 cargo test --release
70001 56320       00:01     0:00.01 /bin/sh -c ~/.claude/statusline.sh
80000     1       05:00     0:01.00 /usr/local/bin/compound-sup
80001 80000       04:58     0:02.00 claude -n wsp-9-cpd-2
";

    /// The reading at `now`, with nothing remembered.
    fn read(table: &str, roots: &[(String, u32)]) -> Shells {
        Shells::of(table, roots, 1000, &mut Memory::default(), &crate::stuck::waits_on_itself_line)
    }

    #[test]
    fn an_agent_idle_on_its_own_background_shell_is_waiting_on_itself_and_owed_nothing() {
        let shells = read(TABLE, &[("cpd-1025".into(), 56312)]);
        let w = reading(Some(State::Idle), &Asks::default(), &shells, "cpd-1025", "wsp-214").expect("its verify is running");
        assert_eq!(w.why, Why::Shell(1), "one shell, and not the subshell under it, the hook or caffeinate");
        assert_eq!(w.word(), "waiting · itself");
        assert!(!w.owed(), "nobody is needed: the shell wakes it when it ends");
        assert_eq!(w.json()["on"], "itself");
        assert_eq!(w.json()["shells"], 1);
        assert!(w.sentence().contains("a shell of its own"), "{}", w.sentence());
    }

    #[test]
    fn the_shell_waiting_on_itself_is_the_one_ended_and_not_its_subshell_or_a_real_build() {
        let table = r#"100     1 claude
101   100 /bin/zsh -c -l source /tmp/snap.sh && eval 'while kill -0 $(pgrep -f "cargo test turn_") 2>/dev/null; do sleep 5; done' < /dev/null
102   101 /bin/zsh -c -l source /tmp/snap.sh && eval 'while kill -0 $(pgrep -f "cargo test turn_") 2>/dev/null; do sleep 5; done' < /dev/null
103   102 pgrep -f cargo test turn_
110   100 /bin/zsh -c -l source /tmp/snap.sh && eval 'pid=$(pgrep -f "tauri build" | head -1); while kill -0 $pid; do sleep 15; done' < /dev/null
200     1 /bin/zsh -c -l source /tmp/snap.sh && eval 'while pgrep -f "cargo build"; do sleep 5; done' < /dev/null
"#;
        assert_eq!(self_wait_shells(table, 100, &crate::stuck::waits_on_itself_line), vec![101], "not the $(…) under it, the pid wait, or another seat's");
    }

    #[test]
    fn another_seats_shell_is_not_this_seats() {
        let shells = read(TABLE, &[("cpd-1025".into(), 56312), ("cpd-2".into(), 80000)]);
        assert!(shells.of_seat("cpd-2").is_none(), "cpd-2's agent has nothing running under it");
        assert!(shells.of_seat("cpd-9").is_none(), "nor has a seat nobody gave a root");
    }

    #[test]
    fn a_shell_of_a_moment_is_a_hook_and_one_of_hours_was_left_running() {
        let table = "\
  100     1    01:00:00  0:01.00 compound-sup
  101   100    59:00     0:30.00 claude
  102   101       00:02  0:00.01 /bin/sh -c wsp hook stop
  103   101    01:00:00  0:10.00 bash -lc npm run dev
  104   103    01:00:00  5:00.00 npm run dev
";
        let roots = [("cpd-1".into(), 100)];
        assert!(read(table, &roots).of_seat("cpd-1").is_none(), "a two-second hook and an hour-old server are not waited on");
        assert!(read(&table.replace("01:00:00  0:10.00 bash", "59:59  0:10.00 bash").replace("01:00:00  5:00.00 npm", "59:59  5:00.00 npm"), &roots).of_seat("cpd-1").is_some());
    }

    /// The seat's tree as `wsp-536` reads it: `claude` (200) under the root
    /// (100), a background shell (201) of `command`, and `kids` under it.
    fn seat_with(command: &str, kids: &str) -> String {
        format!(
            "  100     1    01:00:00  0:01.00 compound-sup\n  200   100    59:00     0:30.00 claude\n  201   200    02:00  0:00.10 /bin/zsh -c source /s.sh && eval '{command}' < /dev/null\n{kids}"
        )
    }

    fn held(table: &str, now: i64, memory: &mut Memory) -> Option<Wait> {
        Shells::of(table, &[("cpd-1".into(), 100)], now, memory, &crate::stuck::waits_on_itself_line).of_seat("cpd-1")
    }

    #[test]
    fn a_shell_with_no_child_holds_nothing_on_the_same_tick() {
        // wsp-530: "1 shell still running" and no process in its tree.
        let table = seat_with("wsp verify", "");
        assert!(held(&table, 1000, &mut Memory::default()).is_none());
    }

    #[test]
    fn a_shell_that_only_watches_holds_nothing() {
        let table = seat_with("until [ -s /tmp/out ]; do sleep 5; done", "  202   201    00:01  0:00.00 sleep 5\n");
        assert!(held(&table, 1000, &mut Memory::default()).is_none());
        let tail = seat_with("tail -f /tmp/out", "  202   201    01:00  0:00.00 tail -f /tmp/out\n");
        assert!(held(&tail, 1000, &mut Memory::default()).is_none());
    }

    #[test]
    fn a_pgrep_guard_that_matches_its_own_line_is_never_held() {
        // wsp-510: `pgrep -f` finds the shell that runs it, so it never ends.
        let looped = "until [ -s /tmp/out ] && ! pgrep -f 'cargo test'; do sleep 5; done";
        let table = seat_with(looped, "  202   201    00:01  0:00.00 sleep 5\n  203   201    01:00  3:00.00 pgrep-helper --work\n");
        assert!(held(&table, 1000, &mut Memory::default()).is_none(), "named by stuck, whatever is under it");
        // The same shell without the self-match is a wait on work.
        let fine = seat_with("until [ -s /tmp/out ]; do sleep 5; done", "  203   201    01:00  3:00.00 pgrep-helper --work\n");
        assert!(held(&fine, 1000, &mut Memory::default()).is_some());
    }

    #[test]
    fn a_busy_test_run_holds_the_seat_and_the_tick_after_it_exits_it_reads_idle() {
        let mut memory = Memory::default();
        let running = seat_with("cargo nextest run", "  202   201    01:00  2:00.00 cargo nextest run\n");
        let w = held(&running, 1000, &mut memory).expect("work under it");
        assert_eq!((w.why.clone(), w.on.clone()), (Why::Shell(1), On::Itself));
        let own = w.own.clone().expect("what it waits on");
        assert_eq!((own.work.as_str(), own.age, own.quiet), ("cargo nextest", 120, 0));
        assert_eq!(w.reason(), "waiting on itself: cargo nextest (2m, busy)");
        assert_eq!(w.json()["reason"], "waiting on itself: cargo nextest (2m, busy)");
        // The cputime moves between ticks: still held.
        let busier = running.replace("2:00.00", "2:19.50");
        assert!(held(&busier, 1020, &mut memory).is_some());
        // It exits: the next tick reads idle, with the shell still there.
        assert!(held(&seat_with("cargo nextest run", ""), 1040, &mut memory).is_none());
    }

    /// `compound-react-041`: the hold on `compound-react-024`'s seat lapsed a
    /// minute after it was written, with its `sh` still running, and the seat
    /// was told and then ended. **Work with no CPU is said as quiet and holds
    /// until the shell ends**, on a fake clock, however long it is quiet.
    #[test]
    fn work_that_has_used_no_cpu_says_quiet_and_holds_until_its_shell_ends() {
        let mut memory = Memory::default();
        let table = seat_with("cargo test", "  202   201    05:00  2:00.00 cargo test\n");
        let first = held(&table, 1000, &mut memory).expect("just seen");
        assert_eq!(first.word(), "waiting · itself", "nothing to say about 0s");
        let w = held(&table, 1045, &mut memory).expect("quiet 45s, still held");
        assert_eq!(w.word(), "waiting · itself · quiet 45s");
        assert_eq!(w.reason(), "waiting on itself: cargo test (2m, quiet 45s)");
        assert!(w.owed() == false);
        for now in (1060..=1000 + 30 * 60).step_by(60) {
            let w = held(&table, now, &mut memory).unwrap_or_else(|| panic!("the shell still runs at +{}s: held", now - 1000));
            assert_eq!(w.own.map(|o| o.quiet), Some(now - 1000), "and says how long it has been quiet");
        }
        // It wakes up: still held, and the quiet clock starts over.
        let woke = table.replace("2:00.00", "2:00.40");
        let w = held(&woke, 1000 + 31 * 60, &mut memory).expect("CPU used");
        assert_eq!(w.own.map(|o| o.quiet), Some(0));
        // The shell ends: the next pass reads idle.
        let gone = "  100     1    01:00:00  0:01.00 compound-sup\n  200   100    59:00     0:30.00 claude\n";
        assert!(held(gone, 1000 + 32 * 60, &mut memory).is_none(), "no shell, no hold");
    }

    /// `compound-react-041`: a shell seen working holds while only a watcher is
    /// under it — the `sleep` between a poll loop's calls — and a new shell of
    /// the same command, or the same pid started again, has to be seen working
    /// first.
    #[test]
    fn a_shell_seen_working_holds_while_only_a_watcher_is_under_it_until_it_ends() {
        let mut memory = Memory::default();
        let loop_ = "while ! cargo build; do sleep 5; done";
        let building = seat_with(loop_, "  202   201    01:00  0:40.00 cargo build\n");
        assert!(held(&building, 1000, &mut memory).is_some());
        let sleeping = seat_with(loop_, "  203   201    00:02  0:00.00 sleep 5\n").replace("02:00  0:00.10 /bin/zsh", "02:30  0:00.10 /bin/zsh");
        let w = held(&sleeping, 1030, &mut memory).expect("seen working: a sleep between its calls holds");
        assert_eq!(w.own.map(|o| o.work), Some("cargo build".to_string()), "named by the work it was seen doing");
        // Never seen working: a fresh memory, the same sleeping table.
        assert!(held(&sleeping, 1030, &mut Memory::default()).is_none(), "a dead wait, as before");
        // The pid came back as another shell (started later): not the one held.
        let reused = sleeping.replace("02:30  0:00.10 /bin/zsh", "00:20  0:00.10 /bin/zsh");
        assert!(held(&reused, 1060, &mut memory).is_none(), "another shell under the same pid");
    }

    #[test]
    fn the_held_shells_survive_the_file_they_are_kept_in() {
        let _env = util::isolated("waiting-held-file");
        let mut memory = Memory::default();
        let building = seat_with("make", "  202   201    01:00  0:40.00 make\n");
        assert!(held(&building, 1000, &mut memory).is_some());
        memory.save(1000);
        let mut loaded = Memory::load();
        let sleeping = seat_with("make", "  203   201    00:02  0:00.00 sleep 5\n").replace("02:00  0:00.10", "02:10  0:00.10");
        assert!(held(&sleeping, 1010, &mut loaded).is_some(), "the daemon's next pass is another process");
    }

    #[test]
    fn a_reading_from_long_ago_is_not_a_baseline() {
        let mut memory = Memory::default();
        let table = seat_with("cargo test", "  202   201    05:00  2:00.00 cargo test\n");
        assert!(held(&table, 1000, &mut memory).is_some());
        assert!(held(&table, 1000 + GAP + 1, &mut memory).is_some(), "not read since: read afresh, not quiet");
    }

    #[test]
    fn the_command_a_shell_runs_is_what_is_inside_its_eval() {
        assert_eq!(ran("/bin/zsh -c source /s.sh && eval 'wsp verify' < /dev/null && pwd -P"), "wsp verify");
        assert_eq!(ran("zsh -c eval 'echo '\\''hi'\\''; sleep 1' x"), "echo 'hi'; sleep 1");
        assert_eq!(ran("bash -c make"), "bash -c make", "no eval: the whole line");
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

    /// `wsp-300`, §26.8 D9: a seat blocked in `wsp ask --wait` is in a tool
    /// call, so the screen says `working`, and it still reads as waiting on a
    /// person — whatever the runner's word, short of a prompt or a gone seat.
    #[test]
    fn a_seat_blocked_in_ask_wait_reads_waiting_on_the_person_whatever_the_screen_says() {
        let asks = Asks::waiter("cpd-1", "wsp-1", "m-1");
        for (state, want) in [
            (State::Working, Some(Why::Question("m-1".into()))),
            (State::Idle, Some(Why::Question("m-1".into()))),
            (State::Starting, Some(Why::Question("m-1".into()))),
            (State::Blocked, Some(Why::Prompt)),
            (State::Gone, None),
        ] {
            let got = reading(Some(state), &asks, &Shells::default(), "cpd-1", "wsp-1");
            assert_eq!(got.map(|w| w.why), want, "{state:?}");
        }
        // The pane is perishable and the task is not: a respawn is still it.
        assert!(reading(Some(State::Working), &asks, &Shells::default(), "cpd-2", "wsp-1").is_some());
        assert!(reading(Some(State::Working), &asks, &Shells::default(), "cpd-2", "wsp-9").is_none(), "another row's seat is not");
        // A question with no waiter keeps the old rule: only an idle asker waits.
        let plain = Asks::one("cpd-1", "wsp-1", "m-1", On::Person);
        assert!(reading(Some(State::Working), &plain, &Shells::default(), "cpd-1", "wsp-1").is_none());

        let w = reading(Some(State::Working), &asks, &Shells::default(), "cpd-1", "wsp-1").unwrap();
        let j = w.json();
        assert_eq!((j["why"].as_str(), j["on"].as_str(), j["id"].as_str()), (Some("question"), Some("person"), Some("m-1")));
        assert!(w.owed() && w.word() == "waiting · person");
    }

    #[test]
    fn held_ask_seat_still_reads_waiting_on_the_person_never_idle() {
        let asks = Asks::waiter("w1:p1", "m-1", "q-1");
        let w = reading(Some(State::Working), &asks, &Shells::one("w9:p9", 0), "w1:p1", "m-1").expect("waiting");
        assert_eq!((w.why, w.on), (Why::Question("q-1".into()), On::Person));
    }

    /// `wsp-581`: the last paragraph of a screen, less the agent's frame.
    #[test]
    fn the_last_words_are_the_final_paragraph_less_the_frame() {
        let screen = "⏺ First thing.\n\n⏺ Ran the build.\n  ⎿  ok\n\n⏺ All green; waiting on the\n  full suite now.\n\n╭────╮\n│ >  │\n╰────╯\n  ? for shortcuts · 1 shell still running\n";
        let w = last_words(screen).unwrap();
        assert_eq!(w.text, "All green; waiting on the full suite now.");
        assert!(!w.asks);
        assert_eq!(last_words("╭──╮\n│ > │\n╰──╯\n"), None);
    }

    #[test]
    fn a_request_is_a_need_an_either_or_or_a_question() {
        for text in [
            "What I need: a permission rule.",
            "**Need** a decision on the branch.",
            "Either you despawn them,\nor tell me to.",
            "Should I go ahead and rebase?",
        ] {
            assert!(last_words(text).unwrap().asks, "{text}");
        }
        for text in ["Needed nothing; done.", "Either way it is green.", "Shipped step 1."] {
            assert!(!last_words(text).unwrap().asks, "{text}");
        }
    }

    #[test]
    fn the_turn_over_line_names_the_work_and_quotes_the_words() {
        let w = Wait { why: Why::Shell(1), on: On::Itself, own: Some(Own { n: 1, work: "cargo test".into(), age: 180, quiet: 0 }) };
        let said = last_words("What I need: a yes.").unwrap();
        let line = w.turn_over("m-1", "cpd-1", Some(&said));
        assert!(line.contains("turn over, waiting on its own `cargo test` (`3m`); its last words: \"What I need: a yes.\""), "{line}");
        assert!(line.contains("an unraised ask"), "{line}");
        let calm = w.turn_over("m-1", "cpd-1", last_words("Done.").as_ref());
        assert!(!calm.contains("unraised"), "{calm}");
        assert!(w.turn_over("m-1", "cpd-1", None).contains("nothing readable"));
    }

    /// `compounding-447`'s turn, as its transcript has it (`wsp-606`): a call
    /// denied by auto mode, then prose. **The turn reads denied, and its last
    /// words are the prose**; the next prompt starts a turn that is neither.
    #[test]
    fn a_transcripts_last_turn_carries_its_denial_and_its_last_words() {
        let line = |v: serde_json::Value| v.to_string();
        let earlier = [
            line(serde_json::json!({"type":"user","message":{"role":"user","content":"go"}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"Which file first?"}]}})),
            line(serde_json::json!({"type":"user","message":{"role":"user","content":"the chrome one"}})),
        ];
        let turn = [
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}})),
            line(serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"Permission for this action was denied. Irreversible Local Destruction"}]}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"The checkout was denied.\n\nI'm stopping here for your decision."}]}})),
        ];
        let jsonl = [earlier.as_slice(), turn.as_slice()].concat().join("\n");
        let (words, denied) = last_turn(&jsonl);
        assert!(denied, "the tool_result carries the denial");
        assert_eq!(words.as_ref().map(|w| w.text.as_str()), Some("I'm stopping here for your decision."));
        let end = TurnEnd { at: 1, words, denied };
        assert!(end.wants());

        let next = format!("{jsonl}\n{}\n{}",
            line(serde_json::json!({"type":"user","message":{"role":"user","content":"agreed, you may proceed"}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"Done and landed."}]}})));
        let (words, denied) = last_turn(&next);
        assert!(!denied, "a new prompt is a new turn");
        let end = TurnEnd { at: 1, words, denied };
        assert!(!end.wants(), "nothing asked, nothing denied");

        let asked = format!("{jsonl}\n{}\n{}",
            line(serde_json::json!({"type":"user","message":{"role":"user","content":"carry on"}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"Shall I land it now?"}]}})));
        let (words, _) = last_turn(&asked);
        assert!(words.is_some_and(|w| w.asks), "a question is a request");
    }

}
