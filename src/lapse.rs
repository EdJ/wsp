//! Stale asks close themselves (`wsp-308`).
//!
//! A question or a notification is relevant while the thing it is about is
//! still going on. `wsp-302` closed the ones a hook was holding when the pane
//! answered, the agent moved on or the seat went; every other open `wsp ask`
//! (to the person or to a governor, with choices or without, a plan, a
//! permission, a barrier's) and every notification about a row stayed open
//! after the row had moved on, and went on showing in `wsp yours` and in
//! compound as something the person owed.
//!
//! # One place, and it is the store's
//!
//! Not in `wsp review`, not in the worklist's landing, not in `despawn`, and
//! not in each of the five verbs that can end a seat — those are the verbs a
//! person types and the ones a daemon tick runs, and every one added later
//! would have to remember. The store already has the narrow places everything
//! passes through, and each calls in here as it writes:
//!
//! | the store's write | is | calls | closes, as |
//! |---|---|---|---|
//! | [`Store::save_task`] | a row going **into** review, done or parked | [`row_moved`] | `lapsed`, *row went to review* |
//! | [`Store::archive_task`] | a row leaving the live store | [`row_moved`] | `lapsed`, *row was archived* |
//! | [`Store::clear_binding`] | a seat's agent being let go, by any road | [`seat_ended`] | `seat-gone`, *agent closed* |
//! | [`Store::clear_claim`] | a claim ending: release, handoff, a closed workspace | [`claim_ended`] | `seat-gone`, *agent closed* |
//! | [`Store::clear_governor`] | a custodial seat standing down | [`governor_ended`] | `seat-gone`, *agent closed* |
//! | [`Store::save_worklist`] | a list going done or parked, or a barrier passing | [`list_moved`] | `lapsed`, *list is done*, *barrier 2 was passed* |
//!
//! **A transition, not a state.** `save_task` is the write every `wsp note`
//! makes too, so it reads the status on disk first and acts only when the row
//! is *entering* a status that finishes it. That is also what makes a **reopen
//! reopen nothing**: moving a row back to `doing` writes no message at all, and
//! the next question raised on it is a new one with its own life — closed, if
//! it comes to that, by the next time the row goes to review.
//!
//! # What is closed
//!
//! Every open question and notification **raised by or about** the thing:
//!
//! - about a row: [`About::Task`] is it; and raised by it: the asker was
//!   *holding* it ([`Waiting::task`]) or sat in a pane bound to it;
//! - about a seat: the asker's pane ([`Waiting::pane`], or a [`Party::Pane`]
//!   `from`), or a governor's own post ([`Party::Seat`]);
//! - about a list: [`About::Scope`] and [`About::Group`] of it, the group
//!   ones also once the list is **past** that barrier — the same `floor` the
//!   position stands on, so a barrier ask closes the moment its verdict is
//!   written and not when somebody next looks.
//!
//! **Replies are not closed.** A reply is an answer on its way home with a
//! delivery of its own (`held_replies`, `ack`); closing it is not this row's
//! decision, and the answer is on the asker's task log whichever way.
//!
//! # By whom, and why this may close a person's question
//!
//! `lapsed` or `seat-gone`, both wsp, both by handles no verb can speak as.
//! The rule that an agent may never close a `--person` question is about a
//! *decision* — an agent saying the person's question is settled. This is the
//! fact that took the question's subject away, and it says so in the reason.
//! A `--wait` is released the way every other ending releases it: the waiter
//! polls the record, finds it closed, and returns the abandonment (exit 3).

use crate::message::{self, About, Message, Party, Shape};
use crate::model::{Status, Worklist, WorklistStatus};
use crate::store::Store;

/// Whether a message is the kind this closes: an open question or notification
/// that is not an answer in flight.
fn closable(m: &Message) -> bool {
    m.is_open() && matches!(m.shape(), Some(Shape::Question | Shape::Notification)) && !m.is_reply()
}

/// Close every open message `why` has a reason for, as `by`. Returns the ids
/// closed, which a test reads and a caller may log.
fn close_where(store: &Store, by: &Party, why: impl Fn(&Message) -> Option<String>) -> Vec<String> {
    store.locked(|| {
        let mut closed = Vec::new();
        for m in store.messages().into_values().filter(closable) {
            if let Some(reason) = why(&m) {
                if message::lapse(store, &m.id, by, &reason).is_ok() {
                    closed.push(m.id);
                }
            }
        }
        closed
    })
}

/// The status a row is entering has finished it: its work is handed to the
/// person (`review`, `done`) or put down on purpose (`parked`).
pub fn finishes(s: Status) -> bool {
    matches!(s, Status::Review | Status::Done | Status::Parked)
}

/// A row went to review, done, parked or the archive: everything raised by or
/// about it is closed with `why` — *row went to review*.
pub fn row_moved(store: &Store, task: &str, why: &str) -> Vec<String> {
    let panes = store.panes_for_task(task);
    close_where(store, &Party::lapsed(), |m| {
        let about = m.about.task() == Some(task);
        let held = m.waiting.as_ref().is_some_and(|w| w.task == task);
        let from_there = matches!(&m.from, Party::Pane { pane, .. } if panes.contains(pane));
        (about || held || from_there).then(|| why.to_string())
    })
}

/// The agent in `pane` is gone — closed, despawned, or its seat vanished:
/// everything it asked or raised from there is closed. *Agent closed.*
pub fn seat_ended(store: &Store, pane: &str) -> Vec<String> {
    close_where(store, &Party::seat_gone(), |m| {
        let asked = m.waiting.as_ref().is_some_and(|w| w.pane == pane);
        let raised = matches!(&m.from, Party::Pane { pane: p, .. } if p == pane);
        (asked || raised).then(|| AGENT_CLOSED.to_string())
    })
}

/// A claim on `task` ended: whoever held it is not on it any more, so what was
/// asked while holding it is closed. What others raised *about* the row is not
/// — a governor's note on the row is still true; only a row *finishing* closes
/// those, and that is [`row_moved`].
pub fn claim_ended(store: &Store, task: &str) -> Vec<String> {
    close_where(store, &Party::seat_gone(), |m| {
        m.waiting.as_ref().is_some_and(|w| w.task == task).then(|| AGENT_CLOSED.to_string())
    })
}

/// A custodial seat stood down: what its occupant asked, or raised from the
/// post, is closed. `pane` is the one its record named, when it named one.
pub fn governor_ended(store: &Store, scope: &str, pane: Option<&str>) -> Vec<String> {
    close_where(store, &Party::seat_gone(), |m| {
        let post = matches!(&m.from, Party::Seat(s) if s == scope);
        let asked = pane.is_some_and(|p| !p.is_empty() && m.waiting.as_ref().is_some_and(|w| w.pane == p));
        (post || asked).then(|| AGENT_CLOSED.to_string())
    })
}

/// The reason every seat-shaped ending gives.
pub const AGENT_CLOSED: &str = "agent closed";

/// A worklist was written: closes what is raised about it when it is done or
/// parked, and a barrier's when the run is past that barrier.
///
/// Asked on every write of a list, so it **looks before it reads**: a list with
/// nothing open about it costs one read of `messages.json`, and the position is
/// never computed for a list that is merely having a log line written to it.
pub fn list_moved(store: &Store, w: &Worklist) -> Vec<String> {
    let of_it = |m: &Message| m.about.scope() == Some(w.id.as_str());
    let any = store.messages().into_values().any(|m| closable(&m) && of_it(&m));
    if !any {
        return Vec::new();
    }
    let ended = match w.status() {
        WorklistStatus::Done => Some("list is done"),
        WorklistStatus::Parked => Some("list was parked"),
        _ => None,
    };
    let crossed = crate::worklist::crossed(&w.groups());
    close_where(store, &Party::lapsed(), |m| {
        if !of_it(m) {
            return None;
        }
        if let Some(why) = ended {
            return Some(why.to_string());
        }
        match &m.about {
            About::Group { group, .. } if *group <= crossed => Some(format!("barrier {group} was passed")),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::choices::Choices;
    use crate::message::{Kind, Waiting};
    use crate::model::{Group, Task};
    use serde_json::{json, Value};

    fn scratch(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-lapse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    fn row(store: &Store, id: &str, status: Status) -> Task {
        let mut t = Task::new("the work", id);
        t.project = Some("wsp".into());
        t.body = String::from("## Overview\nthe brief\n\n## Log\n- 2026-10-22 claimed\n");
        t.set_status(status);
        store.save_task(&t).unwrap();
        t
    }

    fn list(store: &Store) -> Worklist {
        let mut w = Worklist::new("wl", "the run");
        let group = |m: &str| Group { members: vec![m.to_string()], ..Group::default() };
        w.set_groups(&[group("t-1"), group("t-2")]);
        store.save_worklist(&w).unwrap();
        w
    }

    /// Every kind of ask there is, each raised about or by `t-1` unless it says
    /// otherwise, and a control that is neither. Keyed by what each is.
    fn raised(store: &Store) -> Vec<(&'static str, String)> {
        let ask = |from: Party, pane: &str, task: &str, about: About| {
            Message::question(from, Kind::Note, "keep it?", Waiting::new(pane, task)).about(about)
        };
        let on_row = || About::Task("t-1".into());
        let p1 = || Party::pane("p1", "");
        let mut out: Vec<(&'static str, Message)> = Vec::new();
        out.push(("person", ask(p1(), "p1", "t-1", on_row()).for_the_person()));
        out.push(("governor", {
            let mut q = ask(p1(), "p1", "t-1", on_row());
            q.for_seat = Some("gov".into());
            q
        }));
        out.push(("choices", {
            let mut q = ask(p1(), "p1", "t-1", on_row()).for_the_person();
            q.choices = Some(
                Choices::parse(&json!({ "kind": "ask", "questions": [{ "question": "Land now?", "header": "Land",
                    "options": [{ "label": "Land now" }, { "label": "Wait" }] }] }))
                .unwrap(),
            );
            q
        }));
        out.push(("held", {
            let mut q = ask(p1(), "p1", "t-1", on_row()).for_the_person();
            q.awaited = Some(message::Awaited::here());
            q
        }));
        out.push(("note", Message::new(p1(), Kind::Note, "heads up").about(on_row())));
        out.push(("post", ask(Party::seat("gov"), "gp", "", About::Task("t-9".into())).for_the_person()));
        out.push(("barrier-1", {
            let mut q = ask(Party::pane("pb", ""), "pb", "", About::Group { scope: "wl".into(), group: 1 });
            q.for_seat = Some("gov".into());
            q
        }));
        out.push(("barrier-2", ask(Party::pane("pb", ""), "pb", "", About::Group { scope: "wl".into(), group: 2 }).for_the_person()));
        out.push(("list", Message::new(Party::pane("pb", ""), Kind::Note, "about the run").about(About::Scope("wl".into()))));
        out.push(("control", ask(Party::pane("p2", ""), "p2", "t-2", About::Task("t-2".into())).for_the_person()));
        out.into_iter()
            .map(|(name, m)| {
                message::raise(store, &m).unwrap();
                (name, m.id)
            })
            .collect()
    }

    /// Which asks a step closed, with how, by whom and why — read off the store.
    fn closed(store: &Store, ids: &[(&'static str, String)]) -> Vec<(&'static str, String, String, String)> {
        ids.iter()
            .filter_map(|(name, id)| {
                let m = store.message(id).unwrap();
                let c: Value = m.closed_json()?;
                let get = |k: &str| c[k].as_str().unwrap_or_default().to_string();
                Some((*name, get("kind"), get("by"), get("reason")))
            })
            .collect()
    }

    type Step = Box<dyn Fn(&Store)>;

    /// **Every transition × every kind of ask**, in one table, so a rule that
    /// holds for the case somebody remembered cannot pass for the rest. Each
    /// row says what closed and why; what is not named stayed open, which the
    /// control and the barrier asks are there to prove from both sides.
    #[test]
    fn every_transition_closes_what_it_took_away_and_nothing_else() {
        let moved = |to: Status| -> Step {
            Box::new(move |s: &Store| {
                let mut t = s.task("t-1").unwrap();
                t.set_status(to);
                s.save_task(&t).unwrap();
            })
        };
        let list_to = |to: WorklistStatus| -> Step {
            Box::new(move |s: &Store| {
                let mut w = s.worklist("wl").unwrap();
                w.set_status(to);
                s.save_worklist(&w).unwrap();
            })
        };
        let verdicts = |on: usize| -> Step {
            Box::new(move |s: &Store| {
                let mut w = s.worklist("wl").unwrap();
                let mut groups = w.groups();
                for g in groups.iter_mut().take(on) {
                    g.verdict = "read it; go".into();
                }
                w.set_groups(&groups);
                s.save_worklist(&w).unwrap();
            })
        };
        let on_the_row = ["person", "governor", "choices", "held", "note"];
        let by_the_agent = ["person", "governor", "choices", "held"];
        let table: Vec<(&str, Step, Vec<&str>, &str, &str)> = vec![
            ("row to review", moved(Status::Review), on_the_row.to_vec(), "lapsed", "row went to review"),
            ("row to done", moved(Status::Done), on_the_row.to_vec(), "lapsed", "row went to done"),
            ("row to parked", moved(Status::Parked), on_the_row.to_vec(), "lapsed", "row went to parked"),
            (
                "row archived",
                Box::new(|s: &Store| {
                    s.archive_task(&s.task("t-1").unwrap()).unwrap();
                }),
                on_the_row.to_vec(),
                "lapsed",
                "row was archived",
            ),
            (
                "seat ended (despawn, release, a pane gone)",
                Box::new(|s: &Store| assert!(s.clear_binding("p1"))),
                on_the_row.to_vec(),
                "seat-gone",
                AGENT_CLOSED,
            ),
            (
                "claim ended (release, handoff, a workspace closed)",
                Box::new(|s: &Store| assert!(s.clear_claim("t-1"))),
                by_the_agent.to_vec(),
                "seat-gone",
                AGENT_CLOSED,
            ),
            (
                "governor stood down",
                Box::new(|s: &Store| assert!(s.clear_governor("gov"))),
                vec!["post"],
                "seat-gone",
                AGENT_CLOSED,
            ),
            ("list done", list_to(WorklistStatus::Done), vec!["barrier-1", "barrier-2", "list"], "lapsed", "list is done"),
            ("list parked", list_to(WorklistStatus::Parked), vec!["barrier-1", "barrier-2", "list"], "lapsed", "list was parked"),
            ("barrier 1 passed", verdicts(1), vec!["barrier-1"], "lapsed", "barrier 1 was passed"),
            ("past the last barrier", verdicts(2), vec!["barrier-1", "barrier-2"], "lapsed", "barrier "),
            ("a list running on", list_to(WorklistStatus::Running), vec![], "", ""),
            ("a row going on", moved(Status::Doing), vec![], "", ""),
            ("a row blocked", moved(Status::Blocked), vec![], "", ""),
        ];
        for (what, step, closes, kind, why) in table {
            let store = scratch("table");
            row(&store, "t-1", Status::Doing);
            row(&store, "t-2", Status::Doing);
            list(&store);
            store.set_binding("p1", json!({ "task_id": "t-1" }));
            store.set_claim("t-1", json!({ "workspace_id": "w1" }));
            store.set_governor("gov", json!({ "pane": "gp" }));
            let ids = raised(&store);

            step(&store);

            let got = closed(&store, &ids);
            let names: Vec<&str> = got.iter().map(|(n, ..)| *n).collect();
            assert_eq!(names, closes, "{what}: closed {got:?}");
            for (name, k, by, reason) in &got {
                assert_eq!(k, kind, "{what}: {name}");
                assert!(reason.starts_with(why), "{what}: {name}: `{reason}`");
                let want = if kind == "lapsed" { "wsp" } else { "seat" };
                assert_eq!(by, want, "{what}: {name}");
                let m = store.message(&ids.iter().find(|(n, _)| n == name).unwrap().1).unwrap();
                assert_eq!(m.state_raw, "abandoned", "{what}: {name}");
                assert!(!m.is_open(), "{what}: {name}");
            }
            for (name, id) in &ids {
                assert_eq!(store.message(id).unwrap().is_open(), !closes.contains(name), "{what}: {name}");
            }
            // A person's question that was closed is in `yours --json`'s
            // recently-closed set with the kind and the reason, and is not
            // among what `wsp yours` still asks for.
            let still: Vec<String> = message::for_the_person(&store).into_iter().map(|m| m.id).collect();
            for (name, id) in &ids {
                if closes.contains(name) {
                    assert!(!still.contains(id), "{what}: {name} is still on the person's list");
                }
            }
        }
    }

    /// The reason is in the question's own log, in the words the row gave — and
    /// is what `wsp ask <id> --json` and `wsp yours --json` print.
    #[test]
    fn the_closing_kind_and_reason_are_in_the_json_a_surface_reads() {
        let store = scratch("json");
        row(&store, "t-1", Status::Doing);
        let ids = raised(&store);
        let mut t = store.task("t-1").unwrap();
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();

        let person = store.message(&ids[0].1).unwrap();
        let c = person.closed_json().unwrap();
        assert_eq!((c["how"].as_str(), c["by"].as_str(), c["kind"].as_str()), (Some("abandoned"), Some("wsp"), Some("lapsed")));
        assert_eq!(c["reason"], "row went to review");
        assert_eq!(person.to_json()["closed"], c, "`wsp ask <id> --json` says it too");
        assert_eq!(person.closing().unwrap().note, "row went to review", "and it is in the hop, which is the record");

        let item = crate::cmd_message::recently_closed(&store);
        assert!(item.iter().any(|(m, closed)| m.id == person.id && closed["kind"] == "lapsed"), "{item:?}");

        let open = store.message(&ids.iter().find(|(n, _)| *n == "control").unwrap().1).unwrap();
        assert!(open.to_json().get("closed").is_none(), "an open question carries no `closed`");
    }

    /// **A reopen does not reopen old questions** — nothing writes one back, so
    /// the row going on again leaves them closed, a question raised after is
    /// its own, and the next time the row finishes closes that one and touches
    /// nothing already closed.
    #[test]
    fn a_reopen_resurrects_nothing_and_the_next_review_closes_only_the_new() {
        let store = scratch("reopen");
        row(&store, "t-1", Status::Doing);
        let ids = raised(&store);
        let set = |to: Status| {
            let mut t = store.task("t-1").unwrap();
            t.set_status(to);
            store.save_task(&t).unwrap();
        };
        set(Status::Review);
        let old = closed(&store, &ids);
        assert!(!old.is_empty());
        let hops = |id: &str| store.message(id).unwrap().via.len();
        let before: Vec<usize> = ids.iter().map(|(_, id)| hops(id)).collect();

        set(Status::Doing);
        assert_eq!(closed(&store, &ids), old, "reopened: still exactly what was closed");

        let fresh = Message::question(Party::pane("p1", ""), Kind::Note, "and now?", Waiting::new("p1", "t-1"))
            .about(About::Task("t-1".into()))
            .for_the_person();
        message::raise(&store, &fresh).unwrap();
        set(Status::Doing);
        assert!(store.message(&fresh.id).unwrap().is_open(), "a question raised on the reopened row is open");

        set(Status::Review);
        assert!(!store.message(&fresh.id).unwrap().is_open(), "and the next review closes it");
        let after: Vec<usize> = ids.iter().map(|(_, id)| hops(id)).collect();
        assert_eq!(before, after, "the old ones were not touched a second time");
    }

    /// The write every `wsp note` makes is not a transition: a row already at
    /// review — where a verifier raises its questions — closes nothing on being
    /// saved again.
    #[test]
    fn saving_a_row_already_at_review_closes_nothing() {
        let store = scratch("steady");
        row(&store, "t-1", Status::Review);
        let q = Message::question(Party::pane("v1", ""), Kind::Note, "is this right?", Waiting::new("v1", ""))
            .about(About::Task("t-1".into()))
            .for_the_person();
        message::raise(&store, &q).unwrap();
        let mut t = store.task("t-1").unwrap();
        t.log("a verifier read it");
        store.save_task(&t).unwrap();
        assert!(store.message(&q.id).unwrap().is_open());
        // …until the person is finished with it.
        t.set_status(Status::Done);
        store.save_task(&t).unwrap();
        assert!(!store.message(&q.id).unwrap().is_open(), "done closes it, as review would have");
    }

    /// An answer in flight is not a stale ask: a reply keeps its own delivery
    /// (held, acknowledged) whatever happens to the row.
    #[test]
    fn a_reply_going_home_is_not_closed() {
        let store = scratch("reply");
        row(&store, "t-1", Status::Doing);
        let mut q = Message::question(Party::pane("p1", ""), Kind::Note, "keep it?", Waiting::new("p1", "t-1"))
            .about(About::Task("t-1".into()));
        q.for_seat = Some("gov".into());
        message::raise(&store, &q).unwrap();
        let reply = message::answer(&store, &q.id, &Party::seat("gov"), "yes").unwrap().reply;
        let mut t = store.task("t-1").unwrap();
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        assert!(store.message(&reply.id).unwrap().is_open());
    }

    /// The agent's own verbs never close a person's question; wsp's bookkeeping
    /// is the only road, and it is by a handle no verb can be.
    #[test]
    fn no_agent_closes_a_person_question_and_wsp_does_by_its_own_handle() {
        let store = scratch("agents");
        row(&store, "t-1", Status::Doing);
        let q = Message::question(Party::pane("p1", ""), Kind::Note, "keep it?", Waiting::new("p1", "t-1"))
            .about(About::Task("t-1".into()))
            .for_the_person();
        message::raise(&store, &q).unwrap();
        for by in [Party::pane("p1", ""), Party::seat("gov"), Party::Agent("someone".into())] {
            assert!(matches!(message::abandon(&store, &q.id, &by, "done with it"), Err(message::Refused::NotThePersons(_))), "{by:?}");
            assert!(matches!(message::answer(&store, &q.id, &by, "yes"), Err(message::Refused::NotThePersons(_))), "{by:?}");
        }
        assert!(store.message(&q.id).unwrap().is_open());
        assert!(message::lapse(&store, &q.id, &Party::lapsed(), "").is_err(), "a reason is owed");
        message::lapse(&store, &q.id, &Party::lapsed(), "row went to review").unwrap();
        assert!(!store.message(&q.id).unwrap().is_open());
        assert!(message::lapse(&store, &q.id, &Party::lapsed(), "again").is_err(), "and it closes once");
    }
}
