//! A `PermissionRequest` hook that holds the call for the person's answer
//! (`wsp-301`), built to compound-562's spike §1–§3 and the measurements of
//! `docs/spikes/compound-562.md` (`wsp-298`).
//!
//! Claude Code fires `PermissionRequest` with the dialog already drawn, and runs
//! the hook beside it: whichever answers first wins, the pane's dialog or the
//! hook's printed decision. So a hook that raises a question for the person,
//! waits for `wsp answer`, and prints the decision lets compound answer an
//! agent's dialog while the pane stays live. Three dialogs reach it:
//!
//! | tool | choices | the decision |
//! |---|---|---|
//! | `AskUserQuestion` | `ask`: `tool_input.questions` | `allow` + `updatedInput` = `tool_input` + `answers` |
//! | `ExitPlanMode` | `plan`: `tool_input.plan`, `planFilePath` | `allow` + `updatedInput` (+ `setMode`), or `deny` + message |
//! | anything else | `permission`: `command`, `permission_suggestions` | `allow` (+ the suggestion offered), or `deny` + message |
//!
//! # The two ways out that are not an answer
//!
//! **A hold that ends without an answer prints nothing and exits 0**, which is
//! what makes the pane's dialog the one that decides. Never `deny` and never
//! `onFailure: block`: a compound that is closed, a person asleep, a store that
//! is gone all degrade to today's behaviour, a dialog in a pane. The hook gives
//! up [`MARGIN`] seconds before Claude Code's own `timeout` (which it is told)
//! so the question is abandoned by wsp, in words, rather than left open when
//! Claude Code kills the process.
//!
//! **The hook is not cancelled when the pane answers first** (the spike: it ran
//! to its own end, no SIGTERM, and its late decision did nothing). Two things
//! keep that from being more than a sleeping process: a question closed by
//! anyone else ends the wait, and a new request from the same session for the
//! same tool abandons the old one — the question is keyed by `session_id` and
//! tool, since `PermissionRequest` carries no `tool_use_id`. Closing on a pane
//! answer is `wsp-302`'s.
//!
//! # What the answer has to say, measured
//!
//! `ExitPlanMode` and `AskUserQuestion` accept `allow` **only with an
//! `updatedInput`**: a bare `allow` leaves the dialog up (wsp-298). `Bash` is
//! the other way, and gets none. A plan's `setMode` is only ever one of
//! [`MODES`], the two that were measured to reproduce the pane's buttons, and
//! an "always allow" is the suggestion the payload offered with its destination
//! forced to `session`: it expires with the agent and writes nothing in the
//! person's repo.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::choices::Choices;
use crate::message::{self, About, Kind, Message, Party, Waiting};
use crate::store::Store;

/// How long Claude Code is told it may wait, in seconds, when the hook is not
/// told otherwise. A day: measured (`wsp-298`) to be honoured, as were 99999999999
/// and a 700 s hold; the pane's dialog stays live the whole time, so the length
/// bounds how long compound may take and nothing else.
pub const SECS: u64 = 86_400;

/// How long before Claude Code's own timeout this hook stops waiting.
const MARGIN: u64 = 30;

/// How often the question is looked at.
const POLL: Duration = Duration::from_secs(2);

/// The permission modes a plan's approval may set: the two measured to give what
/// the pane's own buttons give (`auto` is its option 1 in Claude Code 2.1.303,
/// `acceptEdits` its older one). A mode outside this list is not applied.
pub const MODES: [&str; 2] = ["auto", "acceptEdits"];

const ASK: &str = "AskUserQuestion";
const PLAN: &str = "ExitPlanMode";

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default()
}

/// The `choices` a payload makes: the kind, what to show, and the fields
/// compound's §26.8 names that this hook can fill (`via`, `held`, `cwd`, `mode`,
/// `session_id`, `input`, `suggestions`). Not checked here — [`Choices::parse`]
/// does that, and refuses what is not a question.
pub fn choices_of(payload: &Value, secs: u64) -> Option<Value> {
    let tool = text(payload, "tool_name");
    if tool.is_empty() {
        return None;
    }
    let input = payload.get("tool_input").cloned().unwrap_or_else(|| json!({}));
    let mut c = Map::new();
    c.insert("via".into(), "hook".into());
    c.insert("tool".into(), tool.into());
    c.insert("held".into(), json!({ "timeout": secs }));
    for (from, to) in [("session_id", "session_id"), ("cwd", "cwd"), ("permission_mode", "mode")] {
        if !text(payload, from).is_empty() {
            c.insert(to.into(), text(payload, from).into());
        }
    }
    match tool {
        ASK => {
            c.insert("kind".into(), "ask".into());
            c.insert("questions".into(), input.get("questions").cloned().unwrap_or(Value::Null));
        }
        PLAN => {
            c.insert("kind".into(), "plan".into());
            c.insert("plan".into(), input.get("plan").cloned().unwrap_or(Value::Null));
            if !text(&input, "planFilePath").is_empty() {
                c.insert("planFilePath".into(), text(&input, "planFilePath").into());
            }
            c.insert("modes".into(), json!(MODES));
        }
        _ => {
            c.insert("kind".into(), "permission".into());
            if !text(&input, "command").is_empty() {
                c.insert("command".into(), text(&input, "command").into());
            }
            c.insert("input".into(), input);
            if let Some(s) = payload.get("permission_suggestions").filter(|s| s.as_array().is_some_and(|a| !a.is_empty())) {
                c.insert("suggestions".into(), s.clone());
            }
        }
    }
    Some(Value::Object(c))
}

/// A multi-select's labels as the pane hands them over (measured, `wsp-298`):
/// joined by `", "`, a label that itself contains `", "` wrapped in double
/// quotes with any `"` inside backslash-escaped. Not invertible, and nothing
/// here reads it back. Whether the pane quotes a lone pick on a multi-select is
/// not measured; it is quoted here as any other element is.
fn joined(labels: &[&str]) -> String {
    labels
        .iter()
        .map(|l| match l.contains(", ") {
            true => format!("\"{}\"", l.replace('"', "\\\"")),
            false => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `updatedInput` of an ask: `tool_input` with `answers` — each question's
/// text to the label, the labels joined, or the person's own words — and, for a
/// single-select pick of an option that has a `preview`, the `annotations` the
/// pane would have filled itself.
fn answered(input: &Value, given: &Map<String, Value>) -> Value {
    let mut out = input.as_object().cloned().unwrap_or_default();
    let mut answers = Map::new();
    let mut notes = Map::new();
    for q in input.get("questions").and_then(Value::as_array).into_iter().flatten() {
        let question = text(q, "question");
        let multi = q.get("multiSelect").or_else(|| q.get("multi")).and_then(Value::as_bool).unwrap_or(false);
        let a = given.get(question).unwrap_or(&Value::Null);
        if let Some(words) = a.get("other").and_then(Value::as_str) {
            answers.insert(q["question"].as_str().unwrap_or(question).into(), words.into());
        } else if let Some(picks) = a.get("pick").and_then(Value::as_array) {
            let labels: Vec<&str> = picks.iter().filter_map(Value::as_str).collect();
            let said = match multi {
                true => joined(&labels),
                false => labels.join(", "),
            };
            answers.insert(q["question"].as_str().unwrap_or(question).into(), said.into());
            let preview = labels.first().filter(|_| !multi && labels.len() == 1).and_then(|l| {
                q.get("options")?.as_array()?.iter().find(|o| text(o, "label") == *l).map(|o| text(o, "preview"))
            });
            if let Some(p) = preview.filter(|p| !p.is_empty()) {
                notes.insert(q["question"].as_str().unwrap_or(question).into(), json!({ "preview": p }));
            }
        }
    }
    out.insert("answers".into(), Value::Object(answers));
    if !notes.is_empty() {
        out.insert("annotations".into(), Value::Object(notes));
    }
    Value::Object(out)
}

/// `setMode` for a mode this build knows to be safe, as `updatedPermissions`.
fn set_mode(mode: Option<&str>) -> Option<Value> {
    let m = mode.map(str::trim).filter(|m| MODES.contains(m))?;
    Some(json!({ "type": "setMode", "mode": m, "destination": "session" }))
}

/// The hook's stdout for a person's answer — the pure half. `answer` is what
/// the question stored (`{answers}` or `{behavior, message?, rule?, mode?}`),
/// `payload` the request it answers. `None` when the answer says nothing a
/// decision can be made of, which the caller leaves to the pane.
pub fn decision_of(payload: &Value, answer: &Value) -> Option<Value> {
    let tool = text(payload, "tool_name");
    let input = payload.get("tool_input").cloned().unwrap_or_else(|| json!({}));
    let decision = if let Some(given) = answer.get("answers").and_then(Value::as_object) {
        if tool != ASK {
            return None;
        }
        json!({ "behavior": "allow", "updatedInput": answered(&input, given) })
    } else {
        match answer.get("behavior").and_then(Value::as_str) {
            Some("deny") => {
                let said = text(answer, "message");
                let said = if said.is_empty() { "The person declined this in wsp." } else { said };
                json!({ "behavior": "deny", "message": said })
            }
            Some("allow") => {
                let mut d = Map::new();
                d.insert("behavior".into(), "allow".into());
                // The tools that need a person accept an allow only with an
                // input to go on with; a tool's permission takes none.
                if matches!(tool, ASK | PLAN) {
                    d.insert("updatedInput".into(), input);
                }
                let mut permissions: Vec<Value> = Vec::new();
                if let Some(rule) = answer.get("rule").filter(|r| r.is_object()) {
                    let mut rule = rule.clone();
                    rule["destination"] = "session".into();
                    permissions.push(rule);
                }
                permissions.extend(set_mode(answer.get("mode").and_then(Value::as_str)));
                if !permissions.is_empty() {
                    d.insert("updatedPermissions".into(), Value::Array(permissions));
                }
                Value::Object(d)
            }
            _ => return None,
        }
    };
    Some(json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } }))
}

/// Hold the request in `payload` for the person, and print what they decide.
/// Prints nothing, and returns 0, whenever it cannot or should not: not a
/// seat holding a row, nothing that is a question, the wait running out, the
/// question closed by something that is not an answer.
pub fn hold(store: &Store, payload: &Value, secs: u64) -> i32 {
    // A subagent shares its parent's seat and has no business raising a
    // question under the parent's name. The shell wrapper turns these away
    // first; this is for the caller that is not the wrapper.
    if !text(payload, "agent_id").is_empty() {
        return 0;
    }
    let (me, pane) = crate::cmd_message::whoami(store);
    let Some(pane) = pane else { return 0 };
    let held = store
        .bindings()
        .get(&pane)
        .and_then(|b| b.get("task_id").and_then(Value::as_str).map(str::to_string))
        .filter(|id| store.task(id).is_some());
    let Some(task) = held else { return 0 };
    hold_at(store, me, &pane, &task, payload, secs, POLL)
}

/// [`hold`] with the asker and the row it holds already known, and the poll
/// given, so a test can answer from the same process.
pub(crate) fn hold_at(store: &Store, me: Party, pane: &str, task: &str, payload: &Value, secs: u64, poll: Duration) -> i32 {
    let Some(choices) = choices_of(payload, secs).and_then(|v| Choices::parse(&v).ok()) else { return 0 };
    let mut q = Message::question(me, Kind::Note, &choices.sentence(), Waiting::new(pane, task))
        .about(About::Task(task.to_string()))
        .for_the_person();
    q.choices = Some(choices);
    supersede(store, &q);
    if message::raise(store, &q).is_err() {
        return 0;
    }
    crate::cmd_message::asked_of_the_person(store, &q, task);

    let give_up = Instant::now() + Duration::from_secs(secs.saturating_sub(MARGIN).max(1));
    loop {
        match store.message(&q.id) {
            None => return 0,
            Some(m) if !m.is_open() => {
                if let Some(d) = m.answer_choice().and_then(|a| decision_of(payload, a)) {
                    println!("{d}");
                }
                return 0;
            }
            Some(_) => {}
        }
        if Instant::now() >= give_up {
            let _ = message::abandon(
                store,
                &q.id,
                &Party::wsp(),
                &format!("no answer in {secs} s — the dialog in the pane is still the way to answer it"),
            );
            return 0;
        }
        std::thread::sleep(poll);
    }
}

/// One question per session and tool: a held hook for the same dialog asked
/// again is a hook whose dialog is gone, and its answer would land on the new
/// one. Abandoned in words, so the person's list does not keep the old.
fn supersede(store: &Store, new: &Message) {
    let key = |m: &Message| -> Option<(String, String)> {
        let c = m.choices.as_ref()?.json();
        let session = c.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty())?;
        (c.get("via").and_then(Value::as_str) == Some("hook"))
            .then(|| (session.to_string(), c.get("tool").and_then(Value::as_str).unwrap_or_default().to_string()))
    };
    let Some(ours) = key(new) else { return };
    for old in message::for_the_person(store) {
        if old.id != new.id && key(&old) == Some(ours.clone()) {
            let _ = message::abandon(store, &old.id, &Party::wsp(), "the same session asked again — this one is gone");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask_payload() -> Value {
        json!({
            "hook_event_name": "PermissionRequest", "session_id": "s1", "tool_name": "AskUserQuestion", "permission_mode": "default",
            "tool_input": { "questions": [
                { "question": "Which figure?", "header": "Figure", "multiSelect": false, "options": [
                    { "label": "Output tokens", "description": "", "preview": "# tokens" }, { "label": "USD", "description": "" } ] },
                { "question": "Which views?", "header": "Views", "multiSelect": true, "options": [
                    { "label": "Card", "description": "" }, { "label": "Rail, wide", "description": "" }, { "label": "Say \"hi\", ok", "description": "" } ] },
            ] },
        })
    }

    fn bash_payload() -> Value {
        json!({
            "hook_event_name": "PermissionRequest", "session_id": "s1", "tool_name": "Bash",
            "tool_input": { "command": "python3 -c \"print(11)\"", "description": "print" },
            "permission_suggestions": [{ "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "python3 -c \"print(11)\"" }],
                                          "behavior": "allow", "destination": "localSettings" }],
        })
    }

    fn plan_payload() -> Value {
        json!({ "session_id": "s1", "tool_name": "ExitPlanMode", "tool_input": { "plan": "# steps", "planFilePath": "/p/plan.md" } })
    }

    fn decision(out: &Value) -> &Value {
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        &out["hookSpecificOutput"]["decision"]
    }

    #[test]
    fn each_dialog_becomes_choices_wsp_accepts() {
        for (what, p, kind) in [("ask", ask_payload(), "ask"), ("plan", plan_payload(), "plan"), ("permission", bash_payload(), "permission")] {
            let c = Choices::parse(&choices_of(&p, 600).unwrap()).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(c.kind(), kind, "{what}");
            let v = c.json();
            assert_eq!((v["via"].as_str(), v["held"]["timeout"].as_u64(), v["session_id"].as_str()), (Some("hook"), Some(600), Some("s1")), "{what}");
        }
        let plan = choices_of(&plan_payload(), 1).unwrap();
        assert_eq!((plan["plan"].as_str(), plan["planFilePath"].as_str(), plan["modes"].clone()), (Some("# steps"), Some("/p/plan.md"), json!(["auto", "acceptEdits"])));
        let perm = choices_of(&bash_payload(), 1).unwrap();
        assert_eq!(perm["command"], "python3 -c \"print(11)\"");
        assert_eq!(perm["suggestions"][0]["ruleContent"], Value::Null, "suggestions are kept whole, rules inside");
        assert_eq!(perm["suggestions"][0]["rules"][0]["ruleContent"], "python3 -c \"print(11)\"");
        // Nothing to ask is nothing raised: no tool, no questions, no plan.
        assert!(choices_of(&json!({}), 1).is_none());
        for p in [json!({ "tool_name": "AskUserQuestion" }), json!({ "tool_name": "ExitPlanMode", "tool_input": {} })] {
            assert!(Choices::parse(&choices_of(&p, 1).unwrap()).is_err(), "{p}");
        }
        let none = choices_of(&json!({ "tool_name": "Write", "tool_input": {}, "permission_suggestions": [] }), 1).unwrap();
        assert!(none.get("suggestions").is_none(), "an empty list offers nothing");
    }

    #[test]
    fn an_ask_answer_is_the_input_with_answers() {
        let p = ask_payload();
        let q1 = "Which figure?";
        let q2 = "Which views?";
        // (what, the stored answer, answers sent, annotations sent)
        for (what, a, answers, notes) in [
            (
                "a pick, and a multi's picks joined",
                json!({ "answers": { q1: { "pick": ["USD"] }, q2: { "pick": ["Card", "Rail, wide"] } } }),
                json!({ q1: "USD", q2: "Card, \"Rail, wide\"" }),
                Value::Null,
            ),
            (
                "a label with a quote and a comma-space is quoted and escaped",
                json!({ "answers": { q1: { "pick": ["USD"] }, q2: { "pick": ["Say \"hi\", ok", "Card"] } } }),
                json!({ q1: "USD", q2: "\"Say \\\"hi\\\", ok\", Card" }),
                Value::Null,
            ),
            (
                "free text is the text, whatever it contains",
                json!({ "answers": { q1: { "other": "neither, tokens/turn" }, q2: { "other": "Lint, Format" } } }),
                json!({ q1: "neither, tokens/turn", q2: "Lint, Format" }),
                Value::Null,
            ),
            (
                "a single pick of an option with a preview sends it as the pane does",
                json!({ "answers": { q1: { "pick": ["Output tokens"] }, q2: { "pick": ["Card"] } } }),
                json!({ q1: "Output tokens", q2: "Card" }),
                json!({ q1: { "preview": "# tokens" } }),
            ),
        ] {
            let out = decision_of(&p, &a).expect(what);
            let d = decision(&out);
            assert_eq!(d["behavior"], "allow", "{what}");
            let sent = &d["updatedInput"];
            assert_eq!(sent["answers"], answers, "{what}");
            assert_eq!(sent["annotations"], notes, "{what}");
            assert_eq!(sent["questions"], p["tool_input"]["questions"], "{what}: the input goes on as it came");
            assert!(d.get("updatedPermissions").is_none(), "{what}");
        }
        // Talking instead of choosing is a refusal with the person's words.
        let out = decision_of(&p, &json!({ "behavior": "deny", "message": "ask me in person" })).unwrap();
        assert_eq!(decision(&out), &json!({ "behavior": "deny", "message": "ask me in person" }));
        // And answers to a tool that took no questions are not a decision.
        assert!(decision_of(&bash_payload(), &json!({ "answers": {} })).is_none());
    }

    #[test]
    fn a_plan_or_permission_is_allowed_or_denied_as_measured() {
        let plan = plan_payload();
        let input = plan["tool_input"].clone();
        let set = |m: &str| json!([{ "type": "setMode", "mode": m, "destination": "session" }]);
        // (the stored answer, the decision)
        for (a, want) in [
            (json!({ "behavior": "allow" }), json!({ "behavior": "allow", "updatedInput": input })),
            (json!({ "behavior": "allow", "mode": "auto" }), json!({ "behavior": "allow", "updatedInput": input, "updatedPermissions": set("auto") })),
            (json!({ "behavior": "allow", "mode": "acceptEdits" }), json!({ "behavior": "allow", "updatedInput": input, "updatedPermissions": set("acceptEdits") })),
            (json!({ "behavior": "allow", "mode": "bypassPermissions" }), json!({ "behavior": "allow", "updatedInput": input })),
            (json!({ "behavior": "deny", "message": "add a rollback step" }), json!({ "behavior": "deny", "message": "add a rollback step" })),
        ] {
            let out = decision_of(&plan, &a).unwrap();
            assert_eq!(decision(&out), &want, "{a}");
        }
        let bash = bash_payload();
        let offered = bash["permission_suggestions"][0].clone();
        let out = decision_of(&bash, &json!({ "behavior": "allow" })).unwrap();
        assert_eq!(decision(&out), &json!({ "behavior": "allow" }), "a tool's allow carries no input");
        let out = decision_of(&bash, &json!({ "behavior": "allow", "rule": offered })).unwrap();
        let rules = &decision(&out)["updatedPermissions"];
        assert_eq!(rules[0]["destination"], "session", "an always-allow expires with the agent");
        assert_eq!(rules[0]["rules"], offered["rules"], "and is exactly the rule offered");
        let out = decision_of(&bash, &json!({ "behavior": "deny" })).unwrap();
        assert_eq!(decision(&out)["behavior"], "deny");
        assert!(decision(&out)["message"].as_str().is_some_and(|m| !m.is_empty()), "a deny always says something");
        assert!(decision_of(&bash, &json!({ "behavior": "maybe" })).is_none());
        assert!(decision_of(&bash, &json!({})).is_none());
    }

    fn at(root: &std::path::Path) -> Store {
        Store::at(root.to_path_buf(), root.join("state"))
    }

    /// A store holding the one row the hook's seat holds. A `Store` caches in
    /// `RefCell`s and is not `Sync`, so each thread opens its own on the path.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("wsp-hold-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = at(&root);
        store.ensure_dirs().unwrap();
        let mut t = crate::model::Task::new("the work", "hold-1");
        t.project = Some("hold".into());
        t.body = String::from("## Overview\nthe brief\n\n## Log\n- 2026-10-19 claimed\n");
        store.save_task(&t).unwrap();
        root
    }

    fn hold_in(root: &std::path::Path, payload: &Value, secs: u64) -> i32 {
        hold_at(&at(root), Party::pane("cpd-1", ""), "cpd-1", "hold-1", payload, secs, Duration::from_millis(10))
    }

    /// The first question on the person's list, once it is raised.
    fn raised(store: &Store) -> Message {
        for _ in 0..500 {
            if let Some(m) = message::for_the_person(store).into_iter().next() {
                return m;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the hook raised nothing");
    }

    /// The question, however it ended; the reply a close writes is not it.
    fn only_message(store: &Store) -> Message {
        let mut all: Vec<Message> = store.messages().into_values().filter(|m| !m.is_reply()).collect();
        assert_eq!(all.len(), 1, "one question, however it ended");
        all.remove(0)
    }

    #[test]
    fn a_held_request_raises_a_question_and_ends_on_its_answer() {
        let root = scratch("answer");
        let payload = bash_payload();
        std::thread::scope(|s| {
            let held = s.spawn(|| hold_in(&root, &payload, 600));
            let store = at(&root);
            let q = raised(&store);
            let c = q.choices.as_ref().unwrap();
            assert_eq!((c.kind(), c.json()["via"].as_str(), q.for_person), ("permission", Some("hook"), true));
            assert_eq!(q.about, About::Task("hold-1".into()));
            let (stored, _) = c.accept(&json!({ "behavior": "deny", "message": "not that" }), "").unwrap();
            message::answer_choice(&store, &q.id, &Party::Human, "", &stored).unwrap();
            assert_eq!(held.join().unwrap(), 0);
        });
        // An answered question is left answered, not abandoned by the hook.
        assert_eq!(only_message(&at(&root)).closed_json().unwrap()["how"], "answered");
    }

    #[test]
    fn the_wait_running_out_abandons_the_question_and_prints_nothing() {
        let root = scratch("timeout");
        // One second to live: the margin takes it to the floor of one.
        assert_eq!(hold_in(&root, &plan_payload(), 1), 0);
        let store = at(&root);
        let closed = only_message(&store).closed_json().unwrap();
        assert_eq!((closed["how"].as_str(), closed["by"].as_str()), (Some("abandoned"), Some("timeout")));
        assert!(message::for_the_person(&store).is_empty(), "nothing is left on the person's list");
    }

    #[test]
    fn a_second_request_for_the_same_dialog_abandons_the_first() {
        let root = scratch("again");
        let payload = ask_payload();
        std::thread::scope(|s| {
            let first = s.spawn(|| hold_in(&root, &payload, 600));
            let store = at(&root);
            let old = raised(&store);
            let second = s.spawn(|| hold_in(&root, &payload, 600));
            // The first hook sees its question closed and lets go on its own.
            assert_eq!(first.join().unwrap(), 0);
            assert_eq!(store.message(&old.id).unwrap().closed_json().unwrap()["how"], "abandoned");
            let new = raised(&store);
            assert_ne!(new.id, old.id);
            let answer = json!({ "answers": { "Which figure?": { "pick": ["USD"] }, "Which views?": { "pick": ["Card"] } } });
            message::answer_choice(&store, &new.id, &Party::Human, "", &answer).unwrap();
            assert_eq!(second.join().unwrap(), 0);
        });
    }

    #[test]
    fn what_is_not_a_question_raises_nothing() {
        let root = scratch("nothing");
        for p in [json!({}), json!({ "tool_name": "AskUserQuestion", "tool_input": {} })] {
            assert_eq!(hold_in(&root, &p, 1), 0);
        }
        let store = at(&root);
        assert!(store.messages().is_empty());
        // A subagent never asks under its parent's name.
        let mut sub = bash_payload();
        sub["agent_id"] = "a1".into();
        assert_eq!(hold(&store, &sub, 1), 0);
        assert!(store.messages().is_empty());
    }
}
