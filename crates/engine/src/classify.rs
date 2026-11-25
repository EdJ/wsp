//! Needs-you, follow-up, or neither: one sorting of everything that could
//! involve the person (`wsp-096`).
//!
//! Jesper's rule, 2026-11-18: *if something is in "Needs you", the person has to
//! do something now; anything else is noise.* `wsp-095` worked out what a
//! barrier waits on ([`crate::owed`]); this sorts that, an ask, and a seat into
//! one of three, so every reader (the mirror's groups and asks, `wsp wip
//! --json`'s `needs_you` and `follow_up` lists) asks [`of`] and none keeps its
//! own copy of the rule.
//!
//! **Worked out on each read, never stored.** [`of`] takes what the item is
//! now (a barrier's [`Owed`] is computed against the clock of the read), so an
//! item moves from follow-up to needs-you when its time passes without anybody
//! writing anything. The mirror re-reads at the next `due` it carries
//! (`mirror::next_due`), which is the same read made later.
//!
//! - A barrier: owed to the person → needs-you, with the act; waiting until a
//!   time → follow-up, `due` that time; waiting on rows → follow-up,
//!   `waiting_on` those rows; its own check → neither.
//! - An open question for the person (a `wsp ask --person`, or the permission
//!   dialog a hook holds as one) → needs-you. A note, a flag, an answered
//!   question, or one for a seat → neither.
//! - A seat `wip` reads as needing the person (stopped on its row, a dialog
//!   up, held in `wsp ask --wait`) → needs-you.
//!
//! - A row at review whose newest verification holds, and that owes no step
//!   but the person's close (`wsp-105`) → needs-you, with **Mark done**
//!   (`wsp done <id>`). One unverified, sent back, reverted on the trunk, or
//!   still judged by a held barrier's stop → neither. Several such rows are
//!   one item, [`finished`]: "N finished rows to close", **Mark all done**,
//!   and each row beneath with its own Mark done.
//! - Mark done is offered only where `wsp done` would go through (`wsp-315`):
//!   a row whose seat finished and still claims it offers **Release and mark
//!   done**, one with open sub-tasks **Done anyway**, one whose far seat is
//!   lost **Despawn (lost)**, and one its run closes none; each offers **Send
//!   back**. A row whose seat is working is neither.
//!
//! A needs-you barrier or question carries the [`Action`] that does it
//! (`wsp-097`); a follow-up carries none. A seat carries none yet: the act on a
//! seat held on a dialog is that dialog's question, which carries one, and a
//! stopped seat's act is reading its screen.
//!
//! Not here: waiting on a named other party (`wsp-095`'s third sub-task, which
//! waits on the person's decision about the record).

use wsp_store_contract::{Ask, AskKind, AskState, Principal};

use crate::mirror::RECORD;
use crate::owed::Owed;

/// The two categories that are shown; neither is `None` from [`of`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The person has to do something now.
    NeedsYou,
    /// Nobody has to do anything yet: it is waiting on a time or on other work,
    /// and comes back on its own.
    FollowUp,
}

impl Kind {
    /// The word the mirror's `attention` field and `wip --json` carry.
    pub fn word(self) -> &'static str {
        match self {
            Kind::NeedsYou => "needs-you",
            Kind::FollowUp => "follow-up",
        }
    }
}

/// What an item asks of the person, as of the read that worked it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attention {
    pub kind: Kind,
    /// The line a reader shows: the act for needs-you, what it waits on for
    /// follow-up.
    pub text: String,
    /// Follow-up on other work: the rows it waits on.
    pub waiting_on: Vec<String>,
    /// Follow-up on a time: when it comes back.
    pub due: Option<String>,
    /// Needs-you only: the `wsp` command that does the act, and its buttons
    /// (`wsp-097`). Follow-up never carries one: nobody is to act yet.
    pub action: Option<Action>,
}

/// Whether a command wants the person's sentence as its last argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sentence {
    /// Ask the person for a sentence and pass it as the last argument.
    Needed,
    /// Run it as it is.
    None,
}

impl Sentence {
    pub fn word(self) -> &'static str {
        match self {
            Sentence::Needed => "needed",
            Sentence::None => "none",
        }
    }
}

/// One way to do a needs-you item: `wsp <verb> <args…>`, with the person's
/// sentence after `args` (joined onto `prefix`, where there is one, as one
/// argument) when `sentence` is needed, and `stdin` piped in where it is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    pub verb: String,
    pub args: Vec<String>,
    pub sentence: Sentence,
    /// What the sentence is written after, in the one argument it makes:
    /// `Question=` for an Other on an ask of several questions.
    pub prefix: Option<String>,
    /// JSON fed on stdin, for an answer only the JSON form can say (allowing
    /// for the session with the rule the prompt offered).
    pub stdin: Option<serde_json::Value>,
    /// Run the command once per entry, each with these args in place of
    /// `args`, in order: **Mark all done** (`wsp done` takes one row).
    pub each: Vec<Vec<String>>,
    /// Ask the person to confirm before running it (`wsp-315`): **Done
    /// anyway** past open sub-tasks, **Despawn (lost)**.
    pub confirm: bool,
    /// Commands run after this one, in order, each `(verb, args)`, and only
    /// where the one before succeeded: **Release and mark done** is `wsp
    /// release <id>`, then `wsp done <id>`.
    pub then: Vec<(String, Vec<String>)>,
}

impl Button {
    fn new(label: &str, verb: &str, args: Vec<String>, sentence: Sentence) -> Self {
        Button {
            label: label.into(),
            verb: verb.into(),
            args,
            sentence,
            prefix: None,
            stdin: None,
            each: Vec::new(),
            confirm: false,
            then: Vec::new(),
        }
    }

    fn confirmed(mut self) -> Self {
        self.confirm = true;
        self
    }

    pub fn json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "label": self.label, "verb": self.verb, "args": self.args, "sentence": self.sentence.word(),
        });
        if let Some(p) = &self.prefix {
            v["prefix"] = p.as_str().into();
        }
        if let Some(i) = &self.stdin {
            v["stdin"] = i.clone();
        }
        if !self.each.is_empty() {
            v["each"] = self.each.clone().into();
        }
        if self.confirm {
            v["confirm"] = true.into();
        }
        if !self.then.is_empty() {
            v["then"] = self.then.iter().map(|(verb, args)| serde_json::json!({ "verb": verb, "args": args })).collect::<Vec<_>>().into();
        }
        v
    }
}

/// Who stands on a finished row, as `wsp done` reads its seats (`wsp-315`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Holder {
    /// No seat stands on it: `wsp done` closes it as it is.
    #[default]
    Nobody,
    /// Its seat has finished — idle at its prompt, or gone — and still
    /// claims it: `wsp release <id>` frees it, then it closes.
    Finished,
    /// A seat on it is in a turn, starting, or on a prompt: nothing is
    /// offered, and the row is in no finished group.
    Working,
    /// Its seat is on a far machine that cannot be reached: `wsp despawn
    /// <id> --lost` lets it go.
    Lost,
}

/// The next steps a refused `wsp done <id>` offers, or a finished row offers
/// in its place (`wsp-315`): by who holds it and how many sub-tasks are open.
/// Empty where the close would go through, and where a seat is working.
///
/// - Its seat finished and still claims it: **Release and mark done**
///   (`release <id>`, then `done <id>`) and **Release**.
/// - Its seat on a far machine that is gone: **Despawn (lost)** (`despawn
///   <id> --lost`), confirmed.
/// - Open sub-tasks, nobody on it: **Done anyway** (`done <id> --force`),
///   confirmed.
pub fn next_steps(id: &str, holder: Holder, open: usize) -> Vec<Button> {
    let row = vec![id.to_string()];
    let mut done = row.clone();
    if open > 0 {
        done.push("--force".into());
    }
    match holder {
        Holder::Finished => {
            let mut both = Button::new("Release and mark done", "release", row.clone(), Sentence::None);
            both.then = vec![("done".into(), done)];
            both.confirm = open > 0;
            vec![both, Button::new("Release", "release", row, Sentence::None)]
        }
        Holder::Lost => vec![Button::new("Despawn (lost)", "despawn", vec![id.to_string(), "--lost".into()], Sentence::None).confirmed()],
        Holder::Nobody if open > 0 => vec![Button::new("Done anyway", "done", done, Sentence::None).confirmed()],
        Holder::Nobody | Holder::Working => Vec::new(),
    }
}

/// The runnable act on a needs-you item (`wsp-097`): `{verb, args, sentence,
/// buttons}`. The command is the one the act's sentence names; the buttons are
/// the ways to do it, each a whole command.
///
/// - A barrier: `worklist go <list>` with a sentence (the verdict). Buttons
///   **Pass** (the same) and **Hold** (`worklist hold <list>`, with the why).
/// - A question with options: `answer <id> --choice <label>`, a button per
///   option (`Question=Label` where it has several questions, named by header)
///   and an **Other…** per question (`--other`, with the words). In words
///   only: `answer <id>`, with the sentence, and no buttons.
/// - A permission prompt (or a plan): `answer <id>` with allow or deny.
///   **Allow**, **Allow for session** where the prompt offered a suggestion
///   (the JSON form on stdin, its first suggestion as the rule, which `wsp
///   answer` scopes to the session), and **Deny**.
/// - A finished row (`wsp-105`): `done <id>`, no sentence, **Mark done**
///   where `wsp done` would go through, else [`next_steps`] (`wsp-315`);
///   and **Send back** (`reopen <id>`, with what is owed).
/// - Several finished rows: `group <ids…>`, **Mark all done** (`done` once
///   per row that offers Mark done, [`Button::each`]), and `items`, each row
///   with its own act.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub verb: String,
    pub args: Vec<String>,
    pub sentence: Sentence,
    pub buttons: Vec<Button>,
    /// A group's members, by id, each with what it asks on its own.
    pub items: Vec<(String, Attention)>,
    /// A finished row's open sub-tasks, where it has any (`wsp-315`).
    pub open: usize,
}

impl Action {
    fn barrier(list: &str) -> Self {
        let go = vec!["go".to_string(), list.to_string()];
        let hold = vec!["hold".to_string(), list.to_string()];
        Action {
            verb: "worklist".into(),
            args: go.clone(),
            sentence: Sentence::Needed,
            buttons: vec![
                Button::new("Pass", "worklist", go, Sentence::Needed),
                Button::new("Hold", "worklist", hold, Sentence::Needed),
            ],
            items: Vec::new(),
            open: 0,
        }
    }

    /// A finished row (`wsp-105`, `wsp-315`): **Mark done** only where `wsp
    /// done <id>` would go through as it is, else the steps that make it
    /// ([`next_steps`]); none where its run closes it. Then **Send back**
    /// (`reopen <id>` with what is owed), always. The action's own command
    /// is its first button's.
    fn row(r: &Row) -> Self {
        let id = vec![r.id.to_string()];
        let mut buttons = match (r.run_closes, r.holder, r.open) {
            (true, _, _) => Vec::new(),
            (false, Holder::Nobody, 0) => vec![Button::new("Mark done", "done", id.clone(), Sentence::None)],
            (false, holder, open) => next_steps(r.id, holder, open),
        };
        buttons.push(Button::new("Send back", "reopen", id, Sentence::Needed));
        let (verb, args, sentence) = (buttons[0].verb.clone(), buttons[0].args.clone(), buttons[0].sentence);
        Action {
            verb,
            args,
            sentence,
            buttons,
            items: Vec::new(),
            open: r.open,
        }
    }

    /// Several finished rows as one: Mark all done runs `wsp done` once per
    /// row whose own action offers Mark done — the rest want a step of their
    /// own first, and Mark all done is left off where none does; `items`
    /// holds each row and its own buttons.
    fn group(items: Vec<(String, Attention)>) -> Self {
        let ids: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();
        let closable: Vec<Vec<String>> = items
            .iter()
            .filter(|(_, a)| a.action.as_ref().is_some_and(|a| a.buttons.iter().any(|b| b.label == "Mark done")))
            .map(|(id, _)| vec![id.clone()])
            .collect();
        let mut buttons = Vec::new();
        if !closable.is_empty() {
            let mut all = Button::new("Mark all done", "done", Vec::new(), Sentence::None);
            all.each = closable;
            buttons.push(all);
        }
        Action { verb: "group".into(), args: ids, sentence: Sentence::None, buttons, items, open: 0 }
    }

    /// An open question for the person, by the choices its record carries.
    fn ask(a: &Ask) -> Self {
        let id = a.id.0.clone();
        let choices = a
            .fields
            .get(RECORD)
            .and_then(|r| serde_json::from_str::<serde_json::Value>(r).ok())
            .and_then(|r| r.get("choices").cloned())
            .filter(|c| c.is_object());
        let Some(c) = choices else {
            return Action { verb: "answer".into(), args: vec![id], sentence: Sentence::Needed, buttons: Vec::new(), items: Vec::new(), open: 0 };
        };
        let with = |more: &[&str]| -> Vec<String> {
            std::iter::once(id.clone()).chain(more.iter().map(|s| s.to_string())).collect()
        };
        let mut buttons = Vec::new();
        if c.get("kind").and_then(|k| k.as_str()) == Some("ask") {
            let qs = c.get("questions").and_then(|q| q.as_array()).cloned().unwrap_or_default();
            let several = qs.len() > 1;
            for q in &qs {
                let s = |k: &str| q.get(k).and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
                let name = if s("header").is_empty() { s("question") } else { s("header") };
                for o in q.get("options").and_then(|o| o.as_array()).into_iter().flatten() {
                    let label = o.get("label").and_then(|l| l.as_str()).unwrap_or_default().trim();
                    if label.is_empty() {
                        continue;
                    }
                    let (shown, given) = match several {
                        true => (format!("{name}: {label}"), format!("{name}={label}")),
                        false => (label.to_string(), label.to_string()),
                    };
                    buttons.push(Button::new(&shown, "answer", with(&["--choice", &given]), Sentence::None));
                }
                let mut other = Button::new(
                    &if several { format!("{name}: Other…") } else { "Other…".into() },
                    "answer",
                    with(&["--other"]),
                    Sentence::Needed,
                );
                other.prefix = several.then(|| format!("{name}="));
                buttons.push(other);
            }
            return Action { verb: "answer".into(), args: with(&["--choice"]), sentence: Sentence::None, buttons, items: Vec::new(), open: 0 };
        }
        buttons.push(Button::new("Allow", "answer", with(&["--choice", "allow"]), Sentence::None));
        let suggestion = c.get("suggestions").and_then(|s| s.as_array()).and_then(|s| s.first()).filter(|s| s.is_object());
        if let Some(rule) = suggestion {
            let mut session = Button::new("Allow for session", "answer", with(&["--choice", "-"]), Sentence::None);
            session.stdin = Some(serde_json::json!({ "behavior": "allow", "rule": rule }));
            buttons.push(session);
        }
        buttons.push(Button::new("Deny", "answer", with(&["--choice", "deny"]), Sentence::None));
        Action { verb: "answer".into(), args: vec![id], sentence: Sentence::None, buttons, items: Vec::new(), open: 0 }
    }

    pub fn json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "verb": self.verb, "args": self.args, "sentence": self.sentence.word(),
            "buttons": self.buttons.iter().map(Button::json).collect::<Vec<_>>(),
        });
        if self.open > 0 {
            v["open_subtasks"] = self.open.into();
        }
        if !self.items.is_empty() {
            v["items"] = self
                .items
                .iter()
                .map(|(id, a)| {
                    let mut i = a.json();
                    i["id"] = id.as_str().into();
                    i
                })
                .collect::<Vec<_>>()
                .into();
        }
        v
    }
}

impl Attention {
    fn needs_you(text: String) -> Self {
        Attention { kind: Kind::NeedsYou, text, waiting_on: Vec::new(), due: None, action: None }
    }

    fn with(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }

    /// The fields the mirror puts on a group or an ask: `attention`,
    /// `attention_text`, and `waiting_on` (rows, space-separated) or `due`.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut f = vec![("attention", self.kind.word().to_string()), ("attention_text", self.text.clone())];
        if !self.waiting_on.is_empty() {
            f.push(("waiting_on", self.waiting_on.join(" ")));
        }
        if let Some(due) = &self.due {
            f.push(("due", due.clone()));
        }
        if let Some(a) = &self.action {
            f.push(("action", a.json().to_string()));
        }
        f
    }

    /// The published shape of one entry in `wip --json`'s lists, beside the
    /// item's own keys: `attention`, `text`, and `waiting_on` or `due`.
    pub fn json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({ "attention": self.kind.word(), "text": self.text });
        if !self.waiting_on.is_empty() {
            v["waiting_on"] = self.waiting_on.clone().into();
        }
        if let Some(due) = &self.due {
            v["due"] = due.as_str().into();
        }
        if let Some(a) = &self.action {
            v["action"] = a.json();
        }
        v
    }
}

/// A seat as `wip` reads it, for [`Item::Seat`].
#[derive(Clone, Copy, Debug)]
pub struct Seat<'a> {
    /// `wip`'s own `needs_you`: stopped on its row, a dialog up, or held in
    /// `wsp ask --wait`. Worked out by `cmd_agent::wip_rows` and not again here.
    pub needs_you: bool,
    /// The row it is on, where it holds one.
    pub task: &'a str,
    /// Its wait's sentence, where it is waiting on somebody.
    pub waiting: Option<&'a str>,
}

/// A row as [`Item::Row`] reads it (`wsp-105`). The caller works out each
/// fact from the store; this only sorts them.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub id: &'a str,
    /// At `review`.
    pub review: bool,
    /// Its newest verification holds: not none, not one still reading, not
    /// one that sent it back.
    pub verified: bool,
    /// It owes a step besides the person's close: its change was reverted on
    /// the trunk, or a held barrier's stop still judges it.
    pub owes: bool,
    /// Who stands on it (`wsp-315`). A working seat leaves it out.
    pub holder: Holder,
    /// Its open sub-tasks, which `wsp done` refuses without `--force`.
    pub open: usize,
    /// A member of a running list whose run verifies and closes it: the
    /// person's close is not offered.
    pub run_closes: bool,
}

/// What a finished row says (`wsp-105`).
pub const FINISHED: &str = "finished and verified: close it or send it back";

/// What a finished row says when its run closes it (`wsp-315`).
pub const RUN_CLOSES: &str = "finished and verified: its run closes it — send it back if it is not done";

/// What a finished row says, by what stands between it and `wsp done`.
fn finished_text(r: &Row) -> String {
    if r.run_closes {
        return RUN_CLOSES.to_string();
    }
    match (r.holder, r.open) {
        (Holder::Finished, _) => {
            "finished and verified, its seat done and still holding the claim: release it and close it, or send it back".into()
        }
        (Holder::Lost, _) => "finished and verified, its seat on a machine that is gone: let the seat go, or send it back".into(),
        (_, 0) => FINISHED.to_string(),
        (_, n) => format!("finished and verified, with {n} open sub-task(s): close it anyway, or send it back"),
    }
}

/// Something that could involve the person.
#[derive(Clone, Copy, Debug)]
pub enum Item<'a> {
    /// A held or ready barrier: its list's slug, and what it waits on as of
    /// this read.
    Barrier(&'a str, &'a Owed),
    Ask(&'a Ask),
    Seat(Seat<'a>),
    Row(Row<'a>),
}

/// The finished rows (each a row [`of`] calls needs-you, with its id) as the
/// one item the person reads (`wsp-105`): none is `None`, one is that row
/// alone, more are "N finished rows to close" with the group action.
pub fn finished(rows: Vec<(String, Attention)>) -> Option<Attention> {
    match rows.len() {
        0 => None,
        1 => rows.into_iter().next().map(|(_, a)| a),
        n => Some(Attention::needs_you(format!("{n} finished rows to close")).with(Action::group(rows))),
    }
}

/// Which of needs-you or follow-up `item` is, and what to say beside it;
/// `None` for neither.
pub fn of(item: Item) -> Option<Attention> {
    match item {
        Item::Barrier(list, owed) => match owed {
            Owed::Person(act) => Some(Attention::needs_you(act.clone()).with(Action::barrier(list))),
            Owed::Until(when) => Some(Attention {
                kind: Kind::FollowUp,
                text: owed.text(),
                waiting_on: Vec::new(),
                due: Some(when.clone()),
                action: None,
            }),
            Owed::Rows(rows) => {
                Some(Attention { kind: Kind::FollowUp, text: owed.text(), waiting_on: rows.clone(), due: None, action: None })
            }
            Owed::Check => None,
        },
        Item::Ask(a) if a.fields.get("of").is_some_and(|o| !o.is_empty()) => {
            // `wsp-098`: an ask from the person to a named party (`--of`).
            let of = a.fields.get("of").map(String::as_str).unwrap_or_default();
            let name: String = of.chars().next().map(|c| c.to_uppercase().chain(of.chars().skip(1)).collect()).unwrap_or_default();
            let field = |k: &str| a.fields.get(k).map(String::as_str).unwrap_or_default();
            if a.kind != AskKind::Ask {
                None
            } else if a.state != AskState::Answered {
                Some(Attention {
                    kind: Kind::FollowUp,
                    text: format!("waiting on {name}: {}", a.text),
                    waiting_on: Vec::new(),
                    due: None,
                    action: None,
                })
            } else if field("answered_by").is_empty() || field("step_done") == "true" {
                None
            } else {
                let args = vec![a.id.0.clone(), "--step-done".to_string()];
                let then = field("then");
                let then = if then.is_empty() { "do the step" } else { then };
                Some(Attention::needs_you(then.to_string()).with(Action {
                    verb: "answer".into(),
                    args: args.clone(),
                    sentence: Sentence::None,
                    buttons: vec![Button::new("Done", "answer", args, Sentence::None)],
                    items: Vec::new(),
                    open: 0,
                }))
            }
        }
        Item::Ask(a) => {
            let for_person = matches!(a.to, Some(Principal::Person(_)));
            let open = a.state != AskState::Answered;
            (for_person && open && a.kind == AskKind::Ask).then(|| {
                Attention::needs_you(format!("answer it: `wsp answer {} \"…\"`", a.id.0)).with(Action::ask(a))
            })
        }
        Item::Seat(s) => s.needs_you.then(|| {
            Attention::needs_you(match (s.waiting, s.task) {
                (Some(why), _) => why.to_string(),
                (None, "") => "stopped: read its screen and tell it what is next, or end it".to_string(),
                (None, task) => format!("stopped on {task}: read its screen and tell it what is next, or end it"),
            })
        }),
        Item::Row(r) => (r.review && r.verified && !r.owes && r.holder != Holder::Working)
            .then(|| Attention::needs_you(finished_text(&r)).with(Action::row(&r))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_store::model::{Group, Worklist, WorklistStatus};
    use wsp_store_contract::{AskId, ListState, SeatId};

    fn ask(kind: AskKind, to: Option<Principal>, state: AskState) -> Ask {
        Ask {
            workspace: Default::default(),
            id: AskId("m-1".into()),
            kind,
            from: Principal::Seat(SeatId("w1:p1".into())),
            to,
            about: None,
            text: "may I?".into(),
            state,
            answer: None,
            fields: Default::default(),
        }
    }

    fn person() -> Option<Principal> {
        Some(Principal::Person("person".into()))
    }

    fn ask_of(state: AskState, extra: &[(&str, &str)]) -> Ask {
        let mut a = ask(AskKind::Ask, None, state);
        a.text = "ok to ship?".into();
        a.fields.insert("of".into(), "ed".into());
        a.fields.insert("then".into(), "tell the customer".into());
        for (k, v) in extra {
            a.fields.insert(k.to_string(), v.to_string());
        }
        a
    }

    #[test]
    fn an_open_ask_of_a_named_party_is_follow_up_waiting_on_them() {
        for state in [AskState::Raised, AskState::Acked] {
            let a = of(Item::Ask(&ask_of(state, &[]))).unwrap();
            assert_eq!((a.kind, a.text.as_str()), (Kind::FollowUp, "waiting on Ed: ok to ship?"));
            assert!(a.action.is_none());
        }
    }

    #[test]
    fn an_answered_ask_of_a_named_party_needs_you_with_the_then_step_and_done() {
        let a = of(Item::Ask(&ask_of(AskState::Answered, &[("answered_by", "ed")]))).unwrap();
        assert_eq!((a.kind, a.text.as_str()), (Kind::NeedsYou, "tell the customer"));
        let j = a.action.unwrap().json();
        assert_eq!(j["verb"], "answer");
        assert_eq!(j["args"], serde_json::json!(["m-1", "--step-done"]));
        assert_eq!(j["sentence"], "none");
        let labels: Vec<_> = j["buttons"].as_array().unwrap().iter().map(|b| b["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["Done"]);
    }

    #[test]
    fn an_ask_of_a_named_party_whose_step_is_done_is_neither() {
        let a = ask_of(AskState::Answered, &[("answered_by", "ed"), ("step_done", "true")]);
        assert_eq!(of(Item::Ask(&a)), None);
    }

    #[test]
    fn a_barrier_owed_to_the_person_needs_you_and_says_the_act() {
        let a = of(Item::Barrier("m1", &Owed::Person("read the stop".into()))).unwrap();
        assert_eq!((a.kind, a.text.as_str()), (Kind::NeedsYou, "read the stop"));
        let f = a.fields();
        assert_eq!(f[..2], [("attention", "needs-you".into()), ("attention_text", "read the stop".into())]);
        assert_eq!(f[2].0, "action", "wsp-097: and the act to run");
        assert_eq!(f.len(), 3);
    }

    #[test]
    fn a_barrier_waiting_on_a_time_is_follow_up_due_then() {
        let a = of(Item::Barrier("m1", &Owed::Until("2026-11-19T09:00:00Z".into()))).unwrap();
        assert_eq!(a.kind, Kind::FollowUp);
        assert_eq!(a.text, "waiting until 2026-11-19T09:00:00Z");
        assert_eq!(a.due.as_deref(), Some("2026-11-19T09:00:00Z"));
        assert!(a.fields().contains(&("due", "2026-11-19T09:00:00Z".into())));
    }

    #[test]
    fn a_barrier_waiting_on_rows_is_follow_up_waiting_on_them() {
        let a = of(Item::Barrier("m1", &Owed::Rows(vec!["wsp-093".into(), "m1-g8".into()]))).unwrap();
        assert_eq!(a.kind, Kind::FollowUp);
        assert_eq!(a.text, "waiting on wsp-093 m1-g8");
        assert_eq!(a.waiting_on, vec!["wsp-093".to_string(), "m1-g8".to_string()]);
        assert!(a.fields().contains(&("waiting_on", "wsp-093 m1-g8".into())));
        assert_eq!(a.json()["waiting_on"], serde_json::json!(["wsp-093", "m1-g8"]));
    }

    #[test]
    fn a_barrier_whose_own_check_reads_it_is_neither() {
        assert_eq!(of(Item::Barrier("m1", &Owed::Check)), None);
    }

    #[test]
    fn an_open_question_for_the_person_needs_you_and_anything_else_asked_is_neither() {
        for state in [AskState::Raised, AskState::Acked] {
            let a = of(Item::Ask(&ask(AskKind::Ask, person(), state))).unwrap();
            assert_eq!(a.kind, Kind::NeedsYou, "{state:?}");
            assert!(a.text.contains("wsp answer m-1"), "{}", a.text);
        }
        assert_eq!(of(Item::Ask(&ask(AskKind::Ask, person(), AskState::Answered))), None, "answered");
        assert_eq!(of(Item::Ask(&ask(AskKind::Note, person(), AskState::Raised))), None, "a note is noise");
        assert_eq!(of(Item::Ask(&ask(AskKind::Flag, None, AskState::Raised))), None, "a flag");
        let to_seat = Some(Principal::Seat(SeatId("governor:wsp".into())));
        assert_eq!(of(Item::Ask(&ask(AskKind::Ask, to_seat, AskState::Raised))), None, "a governor's to answer");
        assert_eq!(of(Item::Ask(&ask(AskKind::Ask, None, AskState::Raised))), None, "for nobody named");
    }

    #[test]
    fn a_seat_wip_reads_as_needing_the_person_needs_you_and_others_are_neither() {
        let stopped = of(Item::Seat(Seat { needs_you: true, task: "wsp-7", waiting: None })).unwrap();
        assert_eq!(stopped.kind, Kind::NeedsYou);
        assert!(stopped.text.starts_with("stopped on wsp-7"), "{}", stopped.text);
        let dialog = Seat { needs_you: true, task: "wsp-7", waiting: Some("held in a tool call for a person to answer m-9") };
        assert_eq!(of(Item::Seat(dialog)).unwrap().text, "held in a tool call for a person to answer m-9");
        assert_eq!(of(Item::Seat(Seat { needs_you: false, task: "wsp-7", waiting: None })), None);
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// An open question for the person whose record carries `choices`.
    fn asked(choices: serde_json::Value) -> Ask {
        let mut a = ask(AskKind::Ask, person(), AskState::Raised);
        a.fields.insert(RECORD.into(), serde_json::json!({ "id": "m-1", "choices": choices }).to_string());
        a
    }

    fn action(a: &Ask) -> Action {
        of(Item::Ask(a)).unwrap().action.expect("a needs-you question carries its action")
    }

    /// `wsp-097`. A barrier owed to the person: `worklist go <list>` with the
    /// verdict, and Pass and Hold, each with a sentence. A follow-up barrier
    /// carries no action, and the mirror's fields carry none for it either.
    #[test]
    fn a_barrier_owed_to_the_person_acts_with_go_and_offers_pass_and_hold() {
        let a = of(Item::Barrier("batch", &Owed::Person("read the stop".into()))).unwrap().action.unwrap();
        assert_eq!((a.verb.as_str(), a.args.clone(), a.sentence), ("worklist", args(&["go", "batch"]), Sentence::Needed));
        let shown: Vec<_> = a.buttons.iter().map(|b| (b.label.as_str(), b.verb.as_str(), b.args.clone(), b.sentence)).collect();
        assert_eq!(
            shown,
            vec![
                ("Pass", "worklist", args(&["go", "batch"]), Sentence::Needed),
                ("Hold", "worklist", args(&["hold", "batch"]), Sentence::Needed),
            ]
        );
        assert_eq!(
            a.json(),
            serde_json::json!({ "verb": "worklist", "args": ["go", "batch"], "sentence": "needed", "buttons": [
                { "label": "Pass", "verb": "worklist", "args": ["go", "batch"], "sentence": "needed" },
                { "label": "Hold", "verb": "worklist", "args": ["hold", "batch"], "sentence": "needed" },
            ] })
        );
        for owed in [Owed::Until("2026-11-19T09:00:00Z".into()), Owed::Rows(vec!["wsp-093".into()])] {
            let f = of(Item::Barrier("batch", &owed)).unwrap();
            assert_eq!((f.kind, &f.action), (Kind::FollowUp, &None), "{owed:?}");
            assert!(!f.fields().iter().any(|(k, _)| *k == "action") && f.json().get("action").is_none());
        }
    }

    /// `wsp-097`. A question with options: a button per option, `--choice
    /// <label>`, and an Other… that takes the words; with several questions
    /// each pick names its question by header, and its Other writes the words
    /// after `Header=`. A question in words takes the sentence and no buttons.
    #[test]
    fn a_question_with_options_offers_a_button_per_option_and_an_other() {
        let one = asked(serde_json::json!({ "kind": "ask", "questions": [{
            "question": "Which way?", "header": "Way",
            "options": [{ "label": "Left", "description": "" }, { "label": "Right", "description": "" }],
        }] }));
        let a = action(&one);
        assert_eq!((a.verb.as_str(), a.args.clone(), a.sentence), ("answer", args(&["m-1", "--choice"]), Sentence::None));
        let shown: Vec<_> = a.buttons.iter().map(|b| (b.label.as_str(), b.args.clone(), b.sentence, b.prefix.clone())).collect();
        assert_eq!(
            shown,
            vec![
                ("Left", args(&["m-1", "--choice", "Left"]), Sentence::None, None),
                ("Right", args(&["m-1", "--choice", "Right"]), Sentence::None, None),
                ("Other…", args(&["m-1", "--other"]), Sentence::Needed, None),
            ]
        );
        assert!(a.buttons.iter().all(|b| b.verb == "answer" && b.stdin.is_none()));

        let two = asked(serde_json::json!({ "kind": "ask", "questions": [
            { "question": "Which way?", "header": "Way", "options": [{ "label": "Left" }] },
            { "question": "How fast?", "header": "", "options": [{ "label": "Slow" }] },
        ] }));
        let labels: Vec<_> = action(&two).buttons.iter().map(|b| (b.label.clone(), b.args.clone(), b.prefix.clone())).collect();
        assert_eq!(
            labels,
            vec![
                ("Way: Left".into(), args(&["m-1", "--choice", "Way=Left"]), None),
                ("Way: Other…".into(), args(&["m-1", "--other"]), Some("Way=".into())),
                ("How fast?: Slow".into(), args(&["m-1", "--choice", "How fast?=Slow"]), None),
                ("How fast?: Other…".into(), args(&["m-1", "--other"]), Some("How fast?=".into())),
            ]
        );

        let words = action(&ask(AskKind::Ask, person(), AskState::Raised));
        assert_eq!((words.args, words.sentence, words.buttons.len()), (args(&["m-1"]), Sentence::Needed, 0));
    }

    /// `wsp-097`. A permission prompt: Allow and Deny, and between them Allow
    /// for session only where the prompt offered a suggestion — the JSON form
    /// on stdin with that suggestion as its rule, the shape `wsp answer
    /// --choice -` takes.
    #[test]
    fn a_permission_prompt_offers_allow_and_deny_and_allow_for_session_only_with_a_suggestion() {
        let plain = action(&asked(serde_json::json!({ "kind": "permission", "tool": "Bash", "command": "ls" })));
        assert_eq!((plain.verb.as_str(), plain.args.clone(), plain.sentence), ("answer", args(&["m-1"]), Sentence::None));
        let shown: Vec<_> = plain.buttons.iter().map(|b| (b.label.as_str(), b.args.clone())).collect();
        assert_eq!(
            shown,
            vec![("Allow", args(&["m-1", "--choice", "allow"])), ("Deny", args(&["m-1", "--choice", "deny"]))]
        );

        let rule = serde_json::json!({ "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "ls:*" }],
            "behavior": "allow", "destination": "localSettings" });
        let offered = action(&asked(serde_json::json!({ "kind": "permission", "tool": "Bash", "command": "ls",
            "suggestions": [rule.clone()] })));
        let labels: Vec<_> = offered.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, vec!["Allow", "Allow for session", "Deny"]);
        let session = &offered.buttons[1];
        assert_eq!(session.args, args(&["m-1", "--choice", "-"]));
        assert_eq!(session.stdin, Some(serde_json::json!({ "behavior": "allow", "rule": rule })));
        assert_eq!(session.json()["stdin"]["rule"], rule);
        assert!(offered.buttons.iter().all(|b| b.verb == "answer" && b.sentence == Sentence::None));

        // An empty list of suggestions offers nothing to allow for the session.
        let none = action(&asked(serde_json::json!({ "kind": "permission", "tool": "Bash", "suggestions": [] })));
        assert_eq!(none.buttons.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), vec!["Allow", "Deny"]);
    }

    /// The done-when: one barrier, two reads, no write between them. Before
    /// its time it is follow-up; after, with nothing else left, it needs you.
    #[test]
    fn a_time_wait_barrier_is_follow_up_before_its_time_and_needs_you_after_with_no_write_between() {
        const NOW: i64 = 1_800_000_000;
        let mut w = Worklist::new("m1", "m1");
        w.set_status(WorklistStatus::Held);
        w.held_until = Some(crate::util::iso_at(NOW + 3_600));
        w.held_for = Some("read the hour's logs".into());
        let g = Group { members: vec!["a-1".into()], agent: "claude".into(), ..Group::default() };
        let read = |now: i64| {
            let owed = crate::owed::of(&w, &g, ListState::Held, &|_| true, now).unwrap();
            of(Item::Barrier("m1", &owed))
        };
        let before = read(NOW).unwrap();
        assert_eq!(before.kind, Kind::FollowUp);
        assert_eq!(before.due, Some(crate::util::iso_at(NOW + 3_600)));
        let after = read(NOW + 3_601).unwrap();
        assert_eq!(after.kind, Kind::NeedsYou);
        assert!(after.text.starts_with("read the hour's logs"), "{}", after.text);
        // And the other way: read late first, then early. Nothing was kept.
        assert_eq!(read(NOW + 3_601).unwrap().kind, Kind::NeedsYou);
        assert_eq!(read(NOW).unwrap().kind, Kind::FollowUp);
    }
    fn row(id: &str, verified: bool, owes: bool) -> Row<'_> {
        Row { id, review: true, verified, owes, holder: Holder::Nobody, open: 0, run_closes: false }
    }

    /// `wsp-105`. A row at review whose newest verification holds needs you,
    /// with `wsp done <id>` and Mark done, in wsp-097's shape. One unverified
    /// or sent back (the caller reads both as not verified), one not at
    /// review, and one that still owes a step are neither.
    #[test]
    fn a_verified_row_at_review_needs_you_with_mark_done_and_an_unverified_or_blocked_one_is_neither() {
        let a = of(Item::Row(row("wsp-93", true, false))).unwrap();
        assert_eq!((a.kind, a.text.as_str()), (Kind::NeedsYou, "finished and verified: close it or send it back"));
        assert_eq!(
            a.action.unwrap().json(),
            serde_json::json!({ "verb": "done", "args": ["wsp-93"], "sentence": "none", "buttons": [
                { "label": "Mark done", "verb": "done", "args": ["wsp-93"], "sentence": "none" },
                { "label": "Send back", "verb": "reopen", "args": ["wsp-93"], "sentence": "needed" },
            ] })
        );
        assert_eq!(of(Item::Row(row("wsp-93", false, false))), None, "unverified, or its verifier sent it back");
        assert_eq!(of(Item::Row(row("wsp-93", true, true))), None, "reverted, or a held barrier still judges it");
        assert_eq!(of(Item::Row(Row { review: false, ..row("wsp-93", true, false) })), None, "not at review");
    }

    /// `wsp-105`, Jesper's scope: several finished rows are one item, "N
    /// finished rows to close", whose Mark all done runs `wsp done` once per
    /// row and whose items are each row with its own Mark done; one is that
    /// row alone; none is nothing.
    #[test]
    fn finished_rows_are_one_group_with_mark_all_done_and_a_single_one_shows_alone() {
        let each = |ids: &[&str]| -> Vec<(String, Attention)> {
            ids.iter().map(|id| (id.to_string(), of(Item::Row(row(id, true, false))).unwrap())).collect()
        };
        assert_eq!(finished(Vec::new()), None);
        assert_eq!(finished(each(&["wsp-1"])), of(Item::Row(row("wsp-1", true, false))), "alone");

        let g = finished(each(&["wsp-1", "wsp-2"])).unwrap();
        assert_eq!((g.kind, g.text.as_str()), (Kind::NeedsYou, "2 finished rows to close"));
        let mark = |id: &str| serde_json::json!({ "verb": "done", "args": [id], "sentence": "none", "buttons": [
            { "label": "Mark done", "verb": "done", "args": [id], "sentence": "none" },
            { "label": "Send back", "verb": "reopen", "args": [id], "sentence": "needed" },
        ] });
        assert_eq!(
            g.action.unwrap().json(),
            serde_json::json!({ "verb": "group", "args": ["wsp-1", "wsp-2"], "sentence": "none",
                "buttons": [{ "label": "Mark all done", "verb": "done", "args": [], "sentence": "none",
                              "each": [["wsp-1"], ["wsp-2"]] }],
                "items": [
                    { "id": "wsp-1", "attention": "needs-you", "text": FINISHED, "action": mark("wsp-1") },
                    { "id": "wsp-2", "attention": "needs-you", "text": FINISHED, "action": mark("wsp-2") },
                ] })
        );
    }

    fn held(id: &str, holder: Holder, open: usize, run_closes: bool) -> Row<'_> {
        Row { holder, open, run_closes, ..row(id, true, false) }
    }

    fn buttons(a: &Attention) -> Vec<serde_json::Value> {
        a.action.as_ref().unwrap().json()["buttons"].as_array().unwrap().clone()
    }

    fn send_back(id: &str) -> serde_json::Value {
        serde_json::json!({ "label": "Send back", "verb": "reopen", "args": [id], "sentence": "needed" })
    }

    /// `wsp-315`. A finished row whose seat finished and still claims it
    /// offers Release and mark done (`release`, then `done`) and Release in
    /// place of Mark done; one whose seat is working is neither, so in no
    /// group; Send back is on every one offered.
    #[test]
    fn a_finished_row_still_claimed_offers_release_and_one_whose_seat_works_is_left_out() {
        let a = of(Item::Row(held("wsp-1", Holder::Finished, 0, false))).unwrap();
        assert_eq!(a.kind, Kind::NeedsYou);
        assert!(a.text.contains("still holding the claim"), "{}", a.text);
        assert_eq!(
            buttons(&a),
            vec![
                serde_json::json!({ "label": "Release and mark done", "verb": "release", "args": ["wsp-1"], "sentence": "none",
                    "then": [{ "verb": "done", "args": ["wsp-1"] }] }),
                serde_json::json!({ "label": "Release", "verb": "release", "args": ["wsp-1"], "sentence": "none" }),
                send_back("wsp-1"),
            ]
        );
        let j = a.action.as_ref().unwrap().json();
        assert_eq!((j["verb"].as_str(), &j["args"]), (Some("release"), &serde_json::json!(["wsp-1"])));

        assert_eq!(of(Item::Row(held("wsp-2", Holder::Working, 0, false))), None, "a working seat's row is not offered");
        let g = finished(vec![
            ("wsp-1".into(), a.clone()),
            ("wsp-3".into(), of(Item::Row(row("wsp-3", true, false))).unwrap()),
        ])
        .unwrap();
        let j = g.action.unwrap().json();
        assert_eq!(j["buttons"][0]["each"], serde_json::json!([["wsp-3"]]), "Mark all done runs only where done goes through");
        assert_eq!(j["items"][0]["action"]["buttons"][0]["label"], "Release and mark done");
    }

    /// `wsp-315`. Open sub-tasks: the count, and Done anyway (`done <id>
    /// --force`) wanting a confirm, in place of Mark done. A far seat that is
    /// lost: Despawn (lost) (`despawn <id> --lost`), confirmed. Both still
    /// offer Send back.
    #[test]
    fn open_sub_tasks_offer_done_anyway_and_a_lost_seat_offers_despawn_lost_both_confirmed() {
        let a = of(Item::Row(held("wsp-4", Holder::Nobody, 2, false))).unwrap();
        assert!(a.text.contains("2 open sub-task(s)"), "{}", a.text);
        let j = a.action.as_ref().unwrap().json();
        assert_eq!(j["open_subtasks"], 2);
        assert_eq!(
            buttons(&a),
            vec![
                serde_json::json!({ "label": "Done anyway", "verb": "done", "args": ["wsp-4", "--force"], "sentence": "none", "confirm": true }),
                send_back("wsp-4"),
            ]
        );

        let lost = of(Item::Row(held("wsp-5", Holder::Lost, 0, false))).unwrap();
        assert_eq!(
            buttons(&lost),
            vec![
                serde_json::json!({ "label": "Despawn (lost)", "verb": "despawn", "args": ["wsp-5", "--lost"], "sentence": "none", "confirm": true }),
                send_back("wsp-5"),
            ]
        );
        assert!(next_steps("wsp-5", Holder::Working, 3).is_empty(), "a working seat: nothing to press");
        assert!(next_steps("wsp-5", Holder::Nobody, 0).is_empty(), "nothing in the way: no step");
    }

    /// `wsp-315`. A member of a running list whose run closes it: no Mark
    /// done, the card says the run closes it, and Send back alone.
    #[test]
    fn a_row_its_run_closes_offers_no_mark_done_and_says_the_run_closes_it() {
        let a = of(Item::Row(held("wsp-309", Holder::Finished, 0, true))).unwrap();
        assert_eq!(a.text, RUN_CLOSES);
        assert_eq!(buttons(&a), vec![send_back("wsp-309")]);
        let j = a.action.unwrap().json();
        assert_eq!((j["verb"].as_str(), j["sentence"].as_str()), (Some("reopen"), Some("needed")));
        assert_eq!(of(Item::Row(held("wsp-309", Holder::Working, 0, true))), None, "working: not even Send back");
    }
}
