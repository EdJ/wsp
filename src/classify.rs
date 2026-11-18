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
//! Not here: waiting on a named other party (`wsp-095`'s third sub-task, which
//! waits on the person's decision about the record), and the `action` an item
//! offers (the next row): [`Attention`] is a struct so that field joins it.

use wsp_store_contract::{Ask, AskKind, AskState, Principal};

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
}

impl Attention {
    fn needs_you(text: String) -> Self {
        Attention { kind: Kind::NeedsYou, text, waiting_on: Vec::new(), due: None }
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

/// Something that could involve the person.
#[derive(Clone, Copy, Debug)]
pub enum Item<'a> {
    /// A held or ready barrier, by what it waits on as of this read.
    Barrier(&'a Owed),
    Ask(&'a Ask),
    Seat(Seat<'a>),
}

/// Which of needs-you or follow-up `item` is, and what to say beside it;
/// `None` for neither.
pub fn of(item: Item) -> Option<Attention> {
    match item {
        Item::Barrier(owed) => match owed {
            Owed::Person(act) => Some(Attention::needs_you(act.clone())),
            Owed::Until(when) => Some(Attention {
                kind: Kind::FollowUp,
                text: owed.text(),
                waiting_on: Vec::new(),
                due: Some(when.clone()),
            }),
            Owed::Rows(rows) => {
                Some(Attention { kind: Kind::FollowUp, text: owed.text(), waiting_on: rows.clone(), due: None })
            }
            Owed::Check => None,
        },
        Item::Ask(a) => {
            let for_person = matches!(a.to, Some(Principal::Person(_)));
            let open = a.state != AskState::Answered;
            (for_person && open && a.kind == AskKind::Ask)
                .then(|| Attention::needs_you(format!("answer it: `wsp answer {} \"…\"`", a.id.0)))
        }
        Item::Seat(s) => s.needs_you.then(|| {
            Attention::needs_you(match (s.waiting, s.task) {
                (Some(why), _) => why.to_string(),
                (None, "") => "stopped: read its screen and tell it what is next, or end it".to_string(),
                (None, task) => format!("stopped on {task}: read its screen and tell it what is next, or end it"),
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_store::model::{Group, Worklist, WorklistStatus};
    use wsp_store_contract::{AskId, ListState, SeatId};

    fn ask(kind: AskKind, to: Option<Principal>, state: AskState) -> Ask {
        Ask {
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

    #[test]
    fn a_barrier_owed_to_the_person_needs_you_and_says_the_act() {
        let a = of(Item::Barrier(&Owed::Person("read the stop".into()))).unwrap();
        assert_eq!((a.kind, a.text.as_str()), (Kind::NeedsYou, "read the stop"));
        assert_eq!(a.fields(), vec![("attention", "needs-you".into()), ("attention_text", "read the stop".into())]);
    }

    #[test]
    fn a_barrier_waiting_on_a_time_is_follow_up_due_then() {
        let a = of(Item::Barrier(&Owed::Until("2026-11-19T09:00:00Z".into()))).unwrap();
        assert_eq!(a.kind, Kind::FollowUp);
        assert_eq!(a.text, "waiting until 2026-11-19T09:00:00Z");
        assert_eq!(a.due.as_deref(), Some("2026-11-19T09:00:00Z"));
        assert!(a.fields().contains(&("due", "2026-11-19T09:00:00Z".into())));
    }

    #[test]
    fn a_barrier_waiting_on_rows_is_follow_up_waiting_on_them() {
        let a = of(Item::Barrier(&Owed::Rows(vec!["wsp-093".into(), "m1-g8".into()]))).unwrap();
        assert_eq!(a.kind, Kind::FollowUp);
        assert_eq!(a.text, "waiting on wsp-093 m1-g8");
        assert_eq!(a.waiting_on, vec!["wsp-093".to_string(), "m1-g8".to_string()]);
        assert!(a.fields().contains(&("waiting_on", "wsp-093 m1-g8".into())));
        assert_eq!(a.json()["waiting_on"], serde_json::json!(["wsp-093", "m1-g8"]));
    }

    #[test]
    fn a_barrier_whose_own_check_reads_it_is_neither() {
        assert_eq!(of(Item::Barrier(&Owed::Check)), None);
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
            of(Item::Barrier(&owed))
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
}
