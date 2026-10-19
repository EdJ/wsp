//! Closing a held question when the pane answers first, or the agent moves on
//! (`wsp-302`), built to compound-562's spike §2 ("pane first also works, but
//! the hook is not cancelled") and `docs/spikes/compound-562.md`.
//!
//! [`crate::hold`] raises an agent's dialog as a question for the person and
//! waits. Claude Code runs that hook beside the dialog and **does not signal it
//! when the pane answers**: it ran to its own end in the trial. So the question
//! would stay on the person's list, offering buttons that do nothing, until the
//! hook's day-long timeout. The agent's own hooks are what say it is over, and
//! `wsp report` hands each of them here:
//!
//! | hook | it means | the question | `by` |
//! |---|---|---|---|
//! | `PostToolUse` | the tool ran: the pane's dialog was answered | **answered**, with what the pane gave | `pane` |
//! | `Stop`, `StopFailure` | the turn ended with the dialog still up | abandoned | `pane` |
//! | `UserPromptSubmit` | a prompt was typed at the pane | abandoned | `pane` |
//! | `SessionEnd` | the session is over | abandoned | `seat` |
//!
//! A seat that vanishes with no `SessionEnd`, and a wait that runs out, are the
//! hold's own to close (`seat` and `timeout`); both are in [`crate::hold`].
//!
//! **Never a clear** (`worklist-004`). Every one of these is an answer or an
//! abandonment with a sentence, so the person's list never loses a question
//! without saying why, and `closed` says how, by whom and when. And the held
//! hook is released by the same close: its loop sees the question closed by
//! someone who is not the person, ends the wait and prints nothing.
//!
//! # Which question a hook closes
//!
//! The ones the hold raised (`choices.via` is `hook`) for the hook's own
//! `session_id`, and no others. `PostToolUse` carries a `tool_use_id`, but
//! `PermissionRequest` does not, so the call is matched by what it is: the tool,
//! and for a tool's permission its input too, because a turn can run several
//! `Bash` calls at once and one that a rule allowed must not close the one that
//! is waiting on a person. An ask and a plan are matched by tool alone: only one
//! of either can be on the screen, and a plan's text can be edited in the
//! dialog. When nothing matches the question stays open and the next `Stop`
//! closes it, which is the cost of being strict, not a loss.
//!
//! # What the pane gave
//!
//! `PostToolUse` of an `AskUserQuestion` carries the answers as the pane
//! formed them, a string per question. A single-select is the bare label; a
//! multi-select is the labels joined by `", "` with a label that contains `", "`
//! quoted (measured, `docs/spikes/compound-562.md`). That is not invertible, so
//! it is never split: the question's own labels are matched against it in
//! order, and what they do not account for is kept as the text it is. A plan
//! and a permission carry nothing more than that the tool ran, so they read
//! `{behavior: allow}`. A deny in the pane produces no `PostToolUse`; that
//! question is closed by the `Stop` or the next prompt, as abandoned.

use serde_json::{json, Map, Value};

use crate::message::{self, Act, Message, Party};
use crate::store::Store;

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default()
}

/// How a hook ends a question.
enum End {
    /// The tool ran: the pane's dialog was answered.
    Answered,
    /// The agent went on with the dialog unanswered, for this reason.
    Moved(&'static str),
    /// The session is over, for this reason.
    Gone(&'static str),
}

fn end_of(hook: &str) -> Option<End> {
    Some(match hook.trim() {
        "PostToolUse" => End::Answered,
        "Stop" => End::Moved("the agent's turn ended with this still open — the dialog was dismissed in the pane"),
        "StopFailure" => End::Moved("the agent's turn ended in an error with this still open"),
        "UserPromptSubmit" => End::Moved("a prompt was typed in the pane — the dialog was dismissed"),
        "SessionEnd" => End::Gone("the session ended with this still open"),
        _ => return None,
    })
}

/// Whether this hook is one that can end a question. `wsp report` asks before
/// it reads the store.
pub fn closes(hook: &str) -> bool {
    end_of(hook).is_some()
}

/// The questions the hold raised for `session` that are still open.
fn open_holds(store: &Store, session: &str) -> Vec<Message> {
    message::for_the_person(store)
        .into_iter()
        .filter(|m| {
            m.choices.as_ref().is_some_and(|c| {
                let c = c.json();
                c.get("via").and_then(Value::as_str) == Some("hook") && text(c, "session_id") == session
            })
        })
        .collect()
}

/// Whether a `PostToolUse` is the call this question is about — see the module
/// note on matching.
fn is_the_call(choices: &Value, payload: &Value) -> bool {
    if text(choices, "tool") != text(payload, "tool_name") {
        return false;
    }
    match text(choices, "kind") {
        "ask" | "plan" => true,
        _ => choices.get("input").is_none_or(|i| payload.get("tool_input") == Some(i)),
    }
}

/// The labels a multi-select's string is made of, in order, or `None` when the
/// question's labels do not account for all of it. At each place the longest
/// label that fits and is followed by the end or `", "` is taken.
fn labels_in<'a>(given: &str, labels: &[&'a str]) -> Option<Vec<&'a str>> {
    let mut rest = given;
    let mut out = Vec::new();
    loop {
        let (label, n) = labels
            .iter()
            .filter_map(|l| {
                let r = crate::hold::rendered(l);
                let after = rest.strip_prefix(r.as_str())?;
                (after.is_empty() || after.starts_with(", ")).then_some((*l, r.len()))
            })
            .max_by_key(|(_, n)| *n)?;
        out.push(label);
        rest = &rest[n..];
        match rest.strip_prefix(", ") {
            Some(more) => rest = more,
            None => return rest.is_empty().then_some(out),
        }
    }
}

/// One pane-given string as an answer is stored: `{pick: […]}` when the
/// question's labels account for it, else `{other: text}`.
fn stored_of(question: &Value, given: &str) -> Value {
    let labels: Vec<&str> = question
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|o| text(o, "label"))
        .collect();
    let multi = question.get("multi").and_then(Value::as_bool).unwrap_or(false);
    let picked = match multi {
        true => labels_in(given, &labels),
        false => labels.iter().find(|l| **l == given).map(|l| vec![*l]),
    };
    match picked {
        Some(p) => json!({ "pick": p }),
        None => json!({ "other": given }),
    }
}

/// What the pane's answer was, as the sentence on the hop and the structure
/// beside it. `None` for the structure when the payload does not carry it.
fn pane_answer(choices: &Value, payload: &Value) -> (String, Option<Value>) {
    match text(choices, "kind") {
        "ask" => {
            let given = payload
                .get("tool_response")
                .and_then(|r| r.get("answers"))
                .or_else(|| payload.get("tool_input").and_then(|i| i.get("answers")))
                .and_then(Value::as_object);
            let Some(given) = given else {
                return ("answered in pane".into(), None);
            };
            let mut stored = Map::new();
            let mut said = Vec::new();
            for q in choices.get("questions").and_then(Value::as_array).into_iter().flatten() {
                let question = text(q, "question");
                let Some(a) = given.get(question).and_then(Value::as_str) else { continue };
                let s = stored_of(q, a);
                let readable = match s.get("pick").and_then(Value::as_array) {
                    Some(p) => p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
                    None => a.to_string(),
                };
                let name = if text(q, "header").is_empty() { question } else { text(q, "header") };
                said.push(format!("{name}: {readable}"));
                stored.insert(question.into(), s);
            }
            match stored.is_empty() {
                true => ("answered in pane".into(), None),
                false => (format!("answered in pane — {}", said.join("; ")), Some(json!({ "answers": stored }))),
            }
        }
        "plan" => ("approved in pane".into(), Some(json!({ "behavior": "allow" }))),
        _ => ("allowed in pane".into(), Some(json!({ "behavior": "allow" }))),
    }
}

/// What the agent's hook `hook` says about the questions its session holds:
/// close each one it ends. Returns how many it closed. Silent, and it never
/// fails the hook: a question somebody else closed first is simply left.
pub fn heard(store: &Store, hook: &str, payload: &Value) -> usize {
    // A subagent shares its parent's session id and its pane; its Stop is not
    // the parent's, and the hold never raises a question for one.
    if !text(payload, "agent_id").is_empty() {
        return 0;
    }
    let session = text(payload, "session_id");
    let Some(end) = end_of(hook).filter(|_| !session.is_empty()) else { return 0 };
    let mut closed = 0;
    for q in open_holds(store, session) {
        let Some(choices) = q.choices.as_ref().map(|c| c.json().clone()) else { continue };
        let pane = q.waiting.as_ref().map(|w| w.pane.clone()).unwrap_or_default();
        let done = match &end {
            End::Answered => {
                if !is_the_call(&choices, payload) {
                    continue;
                }
                let (said, stored) = pane_answer(&choices, payload);
                message::close_from_pane(store, &q.id, &Party::pane(&pane, ""), Act::Answered, &said, stored)
            }
            End::Moved(why) => message::close_from_pane(store, &q.id, &Party::pane(&pane, ""), Act::Abandoned, why, None),
            End::Gone(why) => message::close_from_pane(store, &q.id, &Party::seat_gone(), Act::Abandoned, why, None),
        };
        closed += usize::from(done.is_ok());
    }
    closed
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::hold::tests::{ask_payload, at, bash_payload, held_in, plan_payload, raised, scratch};

    /// Hold `payload` in a scratch store, run `then` against the question once
    /// it is raised, and return what the hook printed and the question as it
    /// ended. A question `then` left open is abandoned afterwards so the hook
    /// can end; read it inside `then` to say it stayed open.
    fn driven(
        tag: &str,
        payload: &Value,
        gone: &(dyn Fn() -> bool + Sync),
        then: impl FnOnce(&Store, &Message) + Send,
    ) -> (Option<Value>, Message) {
        let root = scratch(tag);
        std::thread::scope(|s| {
            let hook = s.spawn(|| held_in(&root, payload, 600, gone));
            let store = at(&root);
            let q = raised(&store);
            then(&store, &q);
            let _ = message::abandon(&store, &q.id, &Party::wsp(), "the test is over");
            let printed = hook.join().unwrap();
            (printed, store.message(&q.id).unwrap())
        })
    }

    fn no() -> bool {
        false
    }

    /// A hook payload for `session`, from the dialog's own request.
    fn post(request: &Value, extra: Value) -> Value {
        let mut p = json!({ "hook_event_name": "PostToolUse", "session_id": "s1" });
        p["tool_name"] = request["tool_name"].clone();
        p["tool_input"] = request["tool_input"].clone();
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    }

    /// Whatever ended the question, it ended with a sentence: never a clear.
    fn assert_ended_in_words(m: &Message) {
        let hop = m.closing().expect("the question is closed by a hop, not removed");
        assert!(matches!(hop.act, Act::Answered | Act::Abandoned), "{:?}", hop.act);
        assert!(!hop.note.trim().is_empty(), "a close says why");
    }

    #[test]
    fn the_hooks_that_can_end_a_question() {
        for h in ["PostToolUse", "Stop", "StopFailure", "UserPromptSubmit", "SessionEnd"] {
            assert!(closes(h), "{h}");
        }
        for h in ["SessionStart", "PermissionRequest", "Elicitation", "PreToolUse", ""] {
            assert!(!closes(h), "{h}");
        }
    }

    #[test]
    fn a_multi_selects_string_is_matched_against_the_labels_never_split() {
        // (the options, the string the pane gave, the picks or None for "kept as text")
        let table: [(&[&str], &str, Option<&[&str]>); 9] = [
            (&["Lint", "Test, and more, ok", "Format"], "Lint, \"Test, and more, ok\"", Some(&["Lint", "Test, and more, ok"])),
            (&["Say \"hi\", ok", "A,B", "C"], "\"Say \\\"hi\\\", ok\", A,B", Some(&["Say \"hi\", ok", "A,B"])),
            (&["A", "B", "A,B"], "A,B", Some(&["A,B"])),
            (&["A", "B", "A,B"], "A, B", Some(&["A", "B"])),
            (&["A", "B", "A,B"], "A,B, B", Some(&["A,B", "B"])),
            (&["Card", "Rail"], "Card", Some(&["Card"])),
            // Something the labels do not account for is the person's own words.
            (&["Card", "Rail"], "Card, and a table", None),
            (&["Card", "Rail"], "neither", None),
            (&["Card", "Rail"], "Card, ", None),
        ];
        for (labels, given, want) in table {
            let q = json!({ "question": "Which?", "multi": true, "options": labels.iter().map(|l| json!({ "label": l })).collect::<Vec<_>>() });
            let want = match want {
                Some(p) => json!({ "pick": p }),
                None => json!({ "other": given }),
            };
            assert_eq!(stored_of(&q, given), want, "{labels:?} / {given}");
        }
        // A single-select is the bare label, a comma and all.
        let one = json!({ "question": "Which?", "options": [{ "label": "Yes, please" }, { "label": "No" }] });
        assert_eq!(stored_of(&one, "Yes, please"), json!({ "pick": ["Yes, please"] }));
        assert_eq!(stored_of(&one, "Maybe"), json!({ "other": "Maybe" }));
    }

    #[test]
    fn a_pane_answer_closes_an_ask_and_the_held_hook_prints_nothing() {
        let request = ask_payload();
        let answered = post(
            &request,
            json!({ "tool_response": { "answers": { "Which figure?": "USD", "Which views?": "Card, \"Rail, wide\"" } } }),
        );
        let (printed, ended) = driven("pane-ask", &request, &no, |store, q| {
            assert_eq!(heard(store, "PostToolUse", &answered), 1);
            assert!(!store.message(&q.id).unwrap().is_open());
        });
        assert_eq!(printed, None, "the pane answered; the dialog is gone and nothing is sent back to it");
        assert_ended_in_words(&ended);
        let closed = ended.closed_json().unwrap();
        assert_eq!((closed["how"].as_str(), closed["by"].as_str()), (Some("answered"), Some("pane")));
        assert_eq!(
            closed["answer"],
            json!({ "answers": { "Which figure?": { "pick": ["USD"] }, "Which views?": { "pick": ["Card", "Rail, wide"] } } })
        );
        assert_eq!(ended.closing().unwrap().note, "answered in pane — Figure: USD; Views: Card, Rail, wide");
        assert!(ended.answer_choice().is_some(), "an answer, not an abandonment");
    }

    #[test]
    fn the_answers_may_ride_in_tool_input_and_may_be_free_text() {
        let request = ask_payload();
        let mut answered = post(&request, json!({}));
        answered["tool_input"]["answers"] = json!({ "Which figure?": "neither, tokens/turn", "Which views?": "Card" });
        let (_, ended) = driven("pane-ask-other", &request, &no, |store, _| {
            assert_eq!(heard(store, "PostToolUse", &answered), 1);
        });
        assert_eq!(
            ended.closed_json().unwrap()["answer"],
            json!({ "answers": { "Which figure?": { "other": "neither, tokens/turn" }, "Which views?": { "pick": ["Card"] } } })
        );
        // The answers missing from the payload still close it: the dialog was answered.
        let bare = post(&request, json!({}));
        let (_, ended) = driven("pane-ask-bare", &request, &no, |store, _| {
            assert_eq!(heard(store, "PostToolUse", &bare), 1);
        });
        let closed = ended.closed_json().unwrap();
        assert_eq!((closed["how"].as_str(), closed["by"].as_str()), (Some("answered"), Some("pane")));
        assert_eq!(closed["answer"], json!({ "text": "answered in pane" }));
    }

    #[test]
    fn a_plan_approved_and_a_permission_allowed_in_the_pane() {
        for (request, say) in [(plan_payload(), "approved in pane"), (bash_payload(), "allowed in pane")] {
            let ran = post(&request, json!({ "tool_response": { "stdout": "11" } }));
            let (printed, ended) = driven("pane-allow", &request, &no, |store, _| {
                assert_eq!(heard(store, "PostToolUse", &ran), 1);
            });
            assert_eq!(printed, None);
            assert_ended_in_words(&ended);
            let closed = ended.closed_json().unwrap();
            assert_eq!((closed["how"].as_str(), closed["by"].as_str()), (Some("answered"), Some("pane")));
            assert_eq!(closed["answer"], json!({ "behavior": "allow" }));
            assert_eq!(ended.closing().unwrap().note, say);
        }
    }

    #[test]
    fn a_post_tool_use_that_is_not_the_waiting_call_closes_nothing() {
        let request = bash_payload();
        let mut other_input = post(&request, json!({}));
        other_input["tool_input"]["command"] = "python3 -c \"print(12)\"".into();
        let mut other_tool = post(&request, json!({}));
        other_tool["tool_name"] = "Write".into();
        let mut other_session = post(&request, json!({}));
        other_session["session_id"] = "s2".into();
        let mut no_session = post(&request, json!({}));
        no_session["session_id"] = "".into();
        let mut subagent = post(&request, json!({}));
        subagent["agent_id"] = "a1".into();
        for (what, p) in [
            ("another call of the same tool, another input (a rule allowed it)", other_input),
            ("another tool", other_tool),
            ("another session", other_session),
            ("no session", no_session),
            ("a subagent", subagent),
        ] {
            let (_, ended) = driven("not-the-call", &request, &no, |store, q| {
                assert_eq!(heard(store, "PostToolUse", &p), 0, "{what}");
                assert!(store.message(&q.id).unwrap().is_open(), "{what}: still waiting on the person");
            });
            // Only the test's own clean-up closed it.
            assert_eq!(ended.closed_json().unwrap()["by"], "timeout", "{what}");
        }
        // An ask and a plan are one at a time: the tool is enough, input or no.
        let plan = plan_payload();
        let mut edited = post(&plan, json!({}));
        edited["tool_input"]["plan"] = "# steps, edited in the dialog".into();
        let (_, ended) = driven("plan-edited", &plan, &no, |store, _| {
            assert_eq!(heard(store, "PostToolUse", &edited), 1);
        });
        assert_eq!(ended.closed_json().unwrap()["how"], "answered");
    }

    #[test]
    fn a_turn_that_ends_a_prompt_or_a_session_abandons_what_is_open() {
        // (the hook, who it is written as, what the sentence says)
        let table = [
            ("Stop", "pane", "turn ended"),
            ("StopFailure", "pane", "error"),
            ("UserPromptSubmit", "pane", "prompt was typed"),
            ("SessionEnd", "seat", "session ended"),
        ];
        for request in [ask_payload(), plan_payload(), bash_payload()] {
            for (hook, by, says) in table {
                let mut ends = json!({ "hook_event_name": hook, "session_id": "s1" });
                ends["tool_name"] = "ignored".into();
                let (printed, ended) = driven("ends", &request, &no, |store, q| {
                    assert_eq!(heard(store, hook, &ends), 1, "{hook}");
                    assert!(!store.message(&q.id).unwrap().is_open());
                    assert!(message::for_the_person(store).is_empty(), "off the person's list");
                });
                assert_eq!(printed, None, "{hook}: the hook is released with nothing to say");
                assert_ended_in_words(&ended);
                let closed = ended.closed_json().unwrap();
                assert_eq!((closed["how"].as_str(), closed["by"].as_str()), (Some("abandoned"), Some(by)), "{hook}");
                assert!(closed["answer"]["text"].as_str().unwrap().contains(says), "{hook}: {closed}");
            }
        }
        // Another session's turn ending is nothing to this one; neither is a subagent's.
        let request = bash_payload();
        for (what, p) in [
            ("another session", json!({ "session_id": "s2" })),
            ("a subagent", json!({ "session_id": "s1", "agent_id": "a1" })),
            ("no session", json!({})),
        ] {
            driven("ends-elsewhere", &request, &no, |store, q| {
                assert_eq!(heard(store, "Stop", &p), 0, "{what}");
                assert!(store.message(&q.id).unwrap().is_open(), "{what}");
            });
        }
    }

    #[test]
    fn the_person_answering_first_is_not_undone_by_the_agent_carrying_on() {
        let request = bash_payload();
        let ran = post(&request, json!({}));
        let (printed, ended) = driven("person-first", &request, &no, |store, q| {
            let (stored, _) = q.choices.as_ref().unwrap().accept(&json!({ "behavior": "allow" }), "").unwrap();
            message::answer_choice(store, &q.id, &Party::Human, "", &stored).unwrap();
            // Claude Code then runs the tool, and the hooks fire for it.
            for hook in ["PostToolUse", "Stop", "SessionEnd"] {
                assert_eq!(heard(store, hook, &if hook == "PostToolUse" { ran.clone() } else { json!({ "session_id": "s1" }) }), 0, "{hook}");
            }
        });
        assert_eq!(printed, Some(json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } })));
        let closed = ended.closed_json().unwrap();
        assert_eq!((closed["how"].as_str(), closed["by"].as_str()), (Some("answered"), Some("person")));
    }

    #[test]
    fn the_hooks_own_timeout_abandons_it_left_to_the_pane() {
        let root = scratch("timeout-reason");
        assert_eq!(held_in(&root, &plan_payload(), 1, &no), None);
        let store = at(&root);
        let ended = store.messages().into_values().find(|m| !m.is_reply()).unwrap();
        assert_ended_in_words(&ended);
        let closed = ended.closed_json().unwrap();
        assert_eq!((closed["how"].as_str(), closed["by"].as_str()), (Some("abandoned"), Some("timeout")));
        assert_eq!(closed["answer"]["text"], "timed out after 1 s, left to the pane");
    }

    #[test]
    fn a_seat_that_vanishes_abandons_it_and_releases_the_hold() {
        let gone = AtomicBool::new(false);
        let request = bash_payload();
        let (printed, ended) = driven("seat-gone", &request, &|| gone.load(Ordering::SeqCst), |store, q| {
            std::thread::sleep(Duration::from_millis(50));
            assert!(store.message(&q.id).unwrap().is_open(), "a live seat keeps its question");
            gone.store(true, Ordering::SeqCst);
            for _ in 0..500 {
                if !store.message(&q.id).unwrap().is_open() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the hold did not notice the seat had gone");
        });
        assert_eq!(printed, None);
        assert_ended_in_words(&ended);
        let closed = ended.closed_json().unwrap();
        assert_eq!((closed["how"].as_str(), closed["by"].as_str()), (Some("abandoned"), Some("seat")));
        assert!(closed["answer"]["text"].as_str().unwrap().contains("seat that asked has gone"));
    }

    #[test]
    fn a_pane_closes_only_what_the_hold_raised_and_never_as_a_person() {
        let root = scratch("narrow");
        let store = at(&root);
        // A question an agent asked with `wsp ask`, not one the hold raised.
        let q = Message::question(Party::pane("cpd-1", ""), message::Kind::Note, "which one?", message::Waiting::new("cpd-1", "hold-1"))
            .about(message::About::Task("hold-1".into()))
            .for_the_person();
        message::raise(&store, &q).unwrap();
        let pane = Party::pane("cpd-1", "");
        assert!(matches!(
            message::close_from_pane(&store, &q.id, &pane, Act::Abandoned, "moved on", None),
            Err(message::Refused::BadChoice(_))
        ));
        assert_eq!(heard(&store, "Stop", &json!({ "session_id": "s1" })), 0);
        assert!(store.message(&q.id).unwrap().is_open());
        // And the road is not a way to answer for the person.
        driven("narrow-held", &bash_payload(), &no, |store, q| {
            for by in [Party::Human, Party::seat("ux")] {
                assert!(message::close_from_pane(store, &q.id, &by, Act::Answered, "yes", None).is_err(), "{by:?}");
            }
            assert!(message::close_from_pane(store, &q.id, &pane, Act::Noted, "looked", None).is_err());
            assert!(message::close_from_pane(store, &q.id, &pane, Act::Abandoned, "  ", None).is_err(), "a sentence is owed");
            assert!(store.message(&q.id).unwrap().is_open());
        });
    }

    #[test]
    fn the_reply_a_pane_close_writes_is_read_already() {
        let request = bash_payload();
        let root = scratch("ack");
        std::thread::scope(|s| {
            let hook = s.spawn(|| held_in(&root, &request, 600, &no));
            let store = at(&root);
            raised(&store);
            assert_eq!(heard(&store, "PostToolUse", &post(&request, json!({}))), 1);
            hook.join().unwrap();
            let replies: Vec<Message> = store.messages().into_values().filter(Message::is_reply).collect();
            assert_eq!((replies.len(), replies.iter().filter(|r| r.is_open()).count()), (1, 0));
        });
    }
}
