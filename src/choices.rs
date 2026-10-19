//! Structured choices on a question, and the answers that close them.
//!
//! Built to compound-562's spike (§3, "wsp records") and compound-580's §26.8.
//! A hook sees an agent's dialog as JSON — `AskUserQuestion`'s questions and
//! options, `ExitPlanMode`'s plan, a tool's permission prompt — and a question
//! for the person that carries those can be drawn as buttons instead of a text
//! field. `text` on the message stays the human sentence, so every surface that
//! knows nothing of choices still reads the question; this is only the extra.
//!
//! # Held as the JSON it arrived as
//!
//! [`Choices`] wraps the object rather than spelling every field out. The
//! fields the spike names (`kind`, `questions`, `plan`, `tool`, `command`) are
//! checked when the question is raised, and the rest (`via`, `input`, `cwd`,
//! `held`, `suggestions`, `risk`, `previous`, `mode`, …: compound's §26.8
//! D1–D11, which other rows fill) pass through untouched. Two reasons. A
//! question is written by one build and read by another, and a field this
//! build does not know is one to hand on, not one to launder on the next write
//! — the argument `message::Message` makes for its own words. And the reader
//! that matters is compound, which wants wsp's field verbatim.
//!
//! # What an answer is
//!
//! | kind | the answer | stored |
//! |---|---|---|
//! | `ask` | `{answers: {<question>: <label> \| {pick: [label…]} \| {other: text}}}` | `{answers: {<q>: {pick: […]} \| {other}}}` |
//! | `ask`, talking instead | `{behavior: deny, message}` | the same |
//! | `plan`, `permission` | `{behavior: allow\|deny, message?, rule?, mode?}` | the same, absent keys left out |
//!
//! A pick must be one of the question's labels; a bare string in `answers` is
//! its label when it is one and an Other when it is not, because a free-text
//! Other is an answer the dialog always offers. `rule` must be one of the
//! question's own `suggestions`. A refusal names the options, since the person
//! who typed the wrong one has nothing else to read them off.

use serde_json::{json, Map, Value};

/// The kinds, and the one word each answer is said in.
pub const KINDS: [&str; 3] = ["ask", "plan", "permission"];

/// The structured part of a question for the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choices(Value);

/// One question of an `ask`, as read off the object.
struct Q<'a> {
    question: &'a str,
    header: &'a str,
    multi: bool,
    labels: Vec<&'a str>,
}

impl Choices {
    /// Check what `wsp ask --choices` was given, fill the defaults, and keep
    /// every key it does not know.
    pub fn parse(v: &Value) -> Result<Choices, String> {
        let Some(o) = v.as_object() else {
            return Err("choices is a JSON object: {kind, questions, …}".into());
        };
        let kind = o.get("kind").and_then(Value::as_str).unwrap_or_default();
        if !KINDS.contains(&kind) {
            return Err(format!("choices.kind is one of {}", KINDS.join(", ")));
        }
        let mut out = o.clone();
        match o.get("via").and_then(Value::as_str) {
            None if !o.contains_key("via") => {
                out.insert("via".into(), "ask".into());
            }
            Some("hook" | "ask") => {}
            _ => return Err("choices.via is hook or ask".into()),
        }
        match kind {
            "ask" => {
                let qs = o.get("questions").and_then(Value::as_array).filter(|q| !q.is_empty());
                let Some(qs) = qs else {
                    return Err("an ask needs choices.questions, at least one".into());
                };
                let mut seen: Vec<&str> = Vec::new();
                let mut norm = Vec::new();
                for q in qs {
                    let (n, text) = question(q)?;
                    if seen.contains(&text) {
                        return Err(format!("two questions say “{text}” — answers are keyed by the question"));
                    }
                    seen.push(text);
                    norm.push(n);
                }
                out.insert("questions".into(), Value::Array(norm));
            }
            "plan" => {
                if o.get("plan").and_then(Value::as_str).is_none_or(|p| p.trim().is_empty()) {
                    return Err("a plan needs choices.plan, the text to approve".into());
                }
            }
            _ => {
                if o.get("tool").and_then(Value::as_str).is_none_or(|t| t.trim().is_empty()) {
                    return Err("a permission needs choices.tool, the tool that asks".into());
                }
            }
        }
        if o.get("suggestions").is_some_and(|s| !s.is_array()) {
            return Err("choices.suggestions is a list".into());
        }
        Ok(Choices(Value::Object(out)))
    }

    /// What was stored, as it was stored. Not checked: a record from a newer
    /// build is shown, not refused.
    pub fn stored(v: Option<&Value>) -> Option<Choices> {
        v.filter(|v| v.get("kind").and_then(Value::as_str).is_some()).map(|v| Choices(v.clone()))
    }

    pub fn json(&self) -> &Value {
        &self.0
    }

    pub fn kind(&self) -> &str {
        self.0.get("kind").and_then(Value::as_str).unwrap_or_default()
    }

    fn questions(&self) -> Vec<Q<'_>> {
        self.0
            .get("questions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|q| Q {
                        question: q.get("question").and_then(Value::as_str).unwrap_or_default(),
                        header: q.get("header").and_then(Value::as_str).unwrap_or_default(),
                        multi: q.get("multi").and_then(Value::as_bool).unwrap_or(false),
                        labels: q
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|o| o.iter().filter_map(|o| o.get("label").and_then(Value::as_str)).collect())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The sentence a question with these choices is asked in, for a caller
    /// that gave none: the questions themselves for an ask, and for the others
    /// the thing to approve.
    pub fn sentence(&self) -> String {
        match self.kind() {
            "ask" => self.questions().iter().map(|q| q.question).collect::<Vec<_>>().join(" "),
            "plan" => "Approve this plan?".into(),
            _ => {
                let tool = self.0.get("tool").and_then(Value::as_str).unwrap_or_default();
                match self.0.get("command").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
                    Some(c) => format!("Allow {tool}: {}?", c.trim()),
                    None => format!("Allow {tool}?"),
                }
            }
        }
    }

    /// What may be answered, in a sentence: the refusal's second half.
    pub fn options_line(&self) -> String {
        match self.kind() {
            "ask" => self
                .questions()
                .iter()
                .map(|q| format!("“{}” — {}", q.question, q.labels.join(" | ")))
                .collect::<Vec<_>>()
                .join("; "),
            _ => "allow | deny".into(),
        }
    }

    /// Answer in plain words: `wsp answer <mid> "…"` on a question with
    /// choices. Only a single-question ask can take it, since the words say
    /// nothing of which question they answer.
    pub fn accept_words(&self, words: &str) -> Result<(Value, String), String> {
        let qs = self.questions();
        match (self.kind(), qs.as_slice()) {
            ("ask", [q]) => self.accept(&json!({ "answers": { q.question: words.trim() } }), ""),
            _ => Err(format!(
                "that question has choices, and words cannot say which they answer — use `--choice`: {}",
                self.options_line()
            )),
        }
    }

    /// Check an answer against the choices. Returns the answer as stored and
    /// as read: the sentence the decision log and the asker get.
    ///
    /// `typed` is the sentence beside `--choice`, if any; it is the message of
    /// a deny that gave none, and refused beside an ask's picks, where it would
    /// be a second answer.
    pub fn accept(&self, input: &Value, typed: &str) -> Result<(Value, String), String> {
        let typed = typed.trim();
        let Some(o) = input.as_object() else {
            return Err(format!("an answer is a JSON object — {}", self.shape()));
        };
        match self.kind() {
            "ask" => self.accept_ask(o, typed),
            _ => self.accept_decision(o, typed),
        }
    }

    fn shape(&self) -> String {
        match self.kind() {
            "ask" => "{answers: {<question>: <label>}} or {behavior: deny, message}".into(),
            _ => "{behavior: allow|deny, message?, rule?, mode?}".into(),
        }
    }

    fn accept_ask(&self, o: &Map<String, Value>, typed: &str) -> Result<(Value, String), String> {
        if o.get("behavior").is_some() {
            let (v, _) = decision(o, typed, true)?;
            return match v["behavior"] == "deny" {
                true => {
                    let said = v["message"].as_str().unwrap_or_default().to_string();
                    Ok((v, format!("did not choose; said: {said}")))
                }
                false => Err(format!("an ask is answered with its options, or declined with deny — {}", self.options_line())),
            };
        }
        if !typed.is_empty() {
            return Err("a sentence and --choice are two answers — send one".into());
        }
        let Some(given) = o.get("answers").and_then(Value::as_object) else {
            return Err(format!("an answer is {}", self.shape()));
        };
        let qs = self.questions();
        if let Some(stray) = given.keys().find(|k| !qs.iter().any(|q| q.question == k.as_str())) {
            return Err(format!("“{stray}” is not one of the questions — {}", self.options_line()));
        }
        let mut out = Map::new();
        let mut said = Vec::new();
        for q in &qs {
            let Some(a) = given.get(q.question) else {
                return Err(format!(
                    "“{}” is not answered — every question is, or none: {}",
                    q.question,
                    self.options_line()
                ));
            };
            let (stored, readable) = pick_of(q, a)?;
            out.insert(q.question.into(), stored);
            let name = if q.header.is_empty() { q.question } else { q.header };
            said.push(format!("{name}: {readable}"));
        }
        Ok((json!({ "answers": out }), said.join("; ")))
    }

    fn accept_decision(&self, o: &Map<String, Value>, typed: &str) -> Result<(Value, String), String> {
        let (v, text) = decision(o, typed, self.kind() == "plan")?;
        if v["behavior"] == "allow" {
            if let Some(rule) = v.get("rule") {
                let offered = self.0.get("suggestions").and_then(Value::as_array);
                match offered {
                    Some(list) if list.contains(rule) => {}
                    Some(list) => {
                        return Err(format!("rule is one of this question's suggestions, and it offers {}", list.len()))
                    }
                    None => return Err("this question offers no rule to allow".into()),
                }
            }
        }
        Ok((v, text))
    }
}

/// One question of an ask, with its defaults filled; and its text.
fn question(q: &Value) -> Result<(Value, &str), String> {
    let Some(o) = q.as_object() else {
        return Err("each of choices.questions is an object: {question, header, multi, options}".into());
    };
    let text = o.get("question").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err("a question has no `question` text".into());
    }
    let Some(options) = o.get("options").and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return Err(format!("“{text}” has no options"));
    };
    let mut labels: Vec<&str> = Vec::new();
    let mut norm = Vec::new();
    for opt in options {
        let Some(oo) = opt.as_object() else {
            return Err(format!("an option of “{text}” is an object: {{label, description, preview?}}"));
        };
        let label = oo.get("label").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if label.is_empty() {
            return Err(format!("an option of “{text}” has no label"));
        }
        if labels.contains(&label) {
            return Err(format!("“{text}” has the option {label} twice"));
        }
        labels.push(label);
        let mut n = oo.clone();
        n.insert("label".into(), label.into());
        n.entry("description").or_insert_with(|| "".into());
        norm.push(Value::Object(n));
    }
    let mut n = o.clone();
    n.insert("question".into(), text.into());
    n.entry("header").or_insert_with(|| "".into());
    // The payload the hook sees says `multiSelect`; both are taken.
    let multi = o.get("multi").or_else(|| o.get("multiSelect")).and_then(Value::as_bool).unwrap_or(false);
    n.remove("multiSelect");
    n.insert("multi".into(), multi.into());
    n.insert("options".into(), Value::Array(norm));
    Ok((Value::Object(n), text))
}

/// One answer to one question: stored as `{pick}` or `{other}`, and read.
fn pick_of(q: &Q<'_>, a: &Value) -> Result<(Value, String), String> {
    let not_option = |l: &str| format!("“{l}” is not an option of “{}” — one of: {}", q.question, q.labels.join(" | "));
    let picks: Vec<String> = match a {
        // A string is a label, or the labels of a multi joined by commas (the
        // way the pane's own dialog hands them over), or else an Other.
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Err(format!("“{}” is answered with nothing", q.question));
            }
            if q.labels.contains(&s) {
                vec![s.to_string()]
            } else if q.multi && s.split(", ").count() > 1 && s.split(", ").all(|p| q.labels.contains(&p)) {
                s.split(", ").map(str::to_string).collect()
            } else {
                return Ok((json!({ "other": s }), s.to_string()));
            }
        }
        Value::Object(o) => match (o.get("pick"), o.get("other")) {
            (Some(Value::Array(p)), None) => {
                let mut v: Vec<String> = Vec::new();
                for l in p {
                    let l = l.as_str().unwrap_or_default().trim();
                    if !q.labels.contains(&l) {
                        return Err(not_option(l));
                    }
                    if !v.iter().any(|x| x == l) {
                        v.push(l.to_string());
                    }
                }
                v
            }
            (Some(Value::String(l)), None) => {
                if !q.labels.contains(&l.trim()) {
                    return Err(not_option(l));
                }
                vec![l.trim().to_string()]
            }
            (None, Some(Value::String(t))) if !t.trim().is_empty() => {
                return Ok((json!({ "other": t.trim() }), t.trim().to_string()));
            }
            _ => return Err(format!("“{}” is answered by {{pick: [label…]}} or {{other: text}}", q.question)),
        },
        _ => return Err(format!("“{}” is answered by a label, {{pick}} or {{other}}", q.question)),
    };
    if picks.is_empty() {
        return Err(format!("“{}” is answered with no pick — one of: {}", q.question, q.labels.join(" | ")));
    }
    if picks.len() > 1 && !q.multi {
        return Err(format!("“{}” takes one answer, not {}", q.question, picks.len()));
    }
    let readable = picks.join(", ");
    Ok((json!({ "pick": picks }), readable))
}

/// `{behavior, message?, rule?, mode?}`, checked and read. `needs_words` is a
/// plan's rejection, which is no use to the agent without a reason.
fn decision(o: &Map<String, Value>, typed: &str, needs_words: bool) -> Result<(Value, String), String> {
    let behavior = o.get("behavior").and_then(Value::as_str).unwrap_or_default();
    let message = o.get("message").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()).unwrap_or(typed);
    let mut out = Map::new();
    out.insert("behavior".into(), behavior.into());
    match behavior {
        "allow" => {
            if !message.is_empty() {
                out.insert("message".into(), message.into());
            }
            for k in ["rule", "mode"] {
                match o.get(k) {
                    None | Some(Value::Null) => {}
                    Some(v) if k == "mode" && v.as_str().is_none_or(|m| m.trim().is_empty()) => {
                        return Err("mode is a permission mode, such as acceptEdits".into())
                    }
                    Some(v) => {
                        out.insert(k.into(), v.clone());
                    }
                }
            }
            let mut text = String::from("allowed");
            if out.contains_key("rule") {
                text.push_str(", and always by the rule offered");
            }
            if let Some(m) = out.get("mode").and_then(Value::as_str) {
                text.push_str(&format!(", then {m}"));
            }
            if !message.is_empty() {
                text.push_str(&format!(": {message}"));
            }
            Ok((Value::Object(out), text))
        }
        "deny" => {
            if o.contains_key("rule") || o.contains_key("mode") {
                return Err("rule and mode go with allow".into());
            }
            if needs_words && message.is_empty() {
                return Err("a rejection takes a sentence — what to change".into());
            }
            if !message.is_empty() {
                out.insert("message".into(), message.into());
            }
            let text = match message.is_empty() {
                true => "denied".to_string(),
                false => format!("denied: {message}"),
            };
            Ok((Value::Object(out), text))
        }
        _ => Err("behavior is allow or deny".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> Choices {
        Choices::parse(&json!({
            "kind": "ask",
            "questions": [
                { "question": "Which figure is the headline?", "header": "Headline", "options": [
                    { "label": "Output tokens", "description": "what the model wrote" },
                    { "label": "USD" } ] },
                { "question": "Which views show the split?", "header": "Views", "multiSelect": true, "options": [
                    { "label": "Card" }, { "label": "Inspector" }, { "label": "Rail, wide" } ] },
            ],
        }))
        .unwrap()
    }

    fn perm() -> Choices {
        Choices::parse(&json!({
            "kind": "permission", "tool": "Bash", "command": "scripts/seed.sh save",
            "suggestions": [{ "type": "setMode", "mode": "acceptEdits" }],
            "input": { "command": "scripts/seed.sh save" },
        }))
        .unwrap()
    }

    #[test]
    fn a_choices_object_is_checked_when_it_is_raised() {
        let q = json!([{ "question": "Q?", "options": [{ "label": "A" }] }]);
        for (what, v, says) in [
            ("not an object", json!([1]), "JSON object"),
            ("no kind", json!({}), "kind is one of"),
            ("a kind it does not know", json!({ "kind": "poll" }), "kind is one of"),
            ("an ask with nothing to ask", json!({ "kind": "ask" }), "at least one"),
            ("no questions listed", json!({ "kind": "ask", "questions": [] }), "at least one"),
            ("a question with no text", json!({ "kind": "ask", "questions": [{ "options": [{ "label": "A" }] }] }), "no `question` text"),
            ("a question with no options", json!({ "kind": "ask", "questions": [{ "question": "Q?" }] }), "has no options"),
            ("an option with no label", json!({ "kind": "ask", "questions": [{ "question": "Q?", "options": [{}] }] }), "has no label"),
            (
                "a label twice",
                json!({ "kind": "ask", "questions": [{ "question": "Q?", "options": [{ "label": "A" }, { "label": "A" }] }] }),
                "twice",
            ),
            (
                "the same question twice",
                json!({ "kind": "ask", "questions": [q[0].clone(), q[0].clone()] }),
                "keyed by the question",
            ),
            ("a plan with no plan", json!({ "kind": "plan" }), "choices.plan"),
            ("a permission with no tool", json!({ "kind": "permission" }), "choices.tool"),
            ("a via it does not know", json!({ "kind": "plan", "plan": "p", "via": "mail" }), "hook or ask"),
            ("suggestions that are not a list", json!({ "kind": "permission", "tool": "Bash", "suggestions": {} }), "a list"),
        ] {
            let got = Choices::parse(&v).expect_err(what);
            assert!(got.contains(says), "{what}: {got}");
        }
    }

    #[test]
    fn defaults_are_filled_and_what_wsp_does_not_know_is_kept() {
        let c = ask();
        let v = c.json();
        assert_eq!(v["via"], "ask", "raised from the CLI unless a hook says it is one");
        let q = &v["questions"][1];
        assert_eq!((q["multi"].as_bool(), q.get("multiSelect")), (Some(true), None), "the hook's spelling is read as multi");
        assert_eq!(v["questions"][0]["multi"], false);
        assert_eq!(v["questions"][0]["options"][1]["description"], "", "an option with no description has an empty one");
        let p = perm();
        assert_eq!(p.json()["input"], json!({ "command": "scripts/seed.sh save" }), "D1 passes through");
        let hook = Choices::parse(&json!({ "kind": "plan", "plan": "# p", "via": "hook", "held": { "timeout": 28800 } })).unwrap();
        assert_eq!((hook.json()["via"].as_str(), hook.json()["held"]["timeout"].as_u64()), (Some("hook"), Some(28800)));
        // And what is stored reads back as it was, unchecked.
        assert_eq!(Choices::stored(Some(c.json())), Some(c));
        assert_eq!(Choices::stored(Some(&json!({ "no": "kind" }))), None);
        assert_eq!(Choices::stored(None), None);
    }

    #[test]
    fn an_ask_is_answered_by_its_labels_or_an_other() {
        let c = ask();
        let both = |first: Value, second: Value| json!({ "answers": {
            "Which figure is the headline?": first, "Which views show the split?": second } });
        // (what, the answer, what is stored, what is read)
        for (what, given, stored, said) in [
            (
                "a pick and a multi pick",
                both(json!({ "pick": ["USD"] }), json!({ "pick": ["Card", "Inspector"] })),
                both(json!({ "pick": ["USD"] }), json!({ "pick": ["Card", "Inspector"] })),
                "Headline: USD; Views: Card, Inspector",
            ),
            (
                "bare strings are labels, and a multi's commas are its labels",
                both(json!("Output tokens"), json!("Card, Inspector")),
                both(json!({ "pick": ["Output tokens"] }), json!({ "pick": ["Card", "Inspector"] })),
                "Headline: Output tokens; Views: Card, Inspector",
            ),
            (
                "a label with a comma in it is still the label",
                both(json!("USD"), json!("Rail, wide")),
                both(json!({ "pick": ["USD"] }), json!({ "pick": ["Rail, wide"] })),
                "Headline: USD; Views: Rail, wide",
            ),
            (
                "words that are no label are an Other, either spelling",
                both(json!("neither, tokens per turn"), json!({ "other": "all of them" })),
                both(json!({ "other": "neither, tokens per turn" }), json!({ "other": "all of them" })),
                "Headline: neither, tokens per turn; Views: all of them",
            ),
        ] {
            let (got, text) = c.accept(&given, "").expect(what);
            assert_eq!((got, text.as_str()), (stored, said), "{what}");
        }
    }

    #[test]
    fn an_answer_that_is_not_one_of_the_options_is_refused_naming_them() {
        let c = ask();
        let q1 = "Which figure is the headline?";
        let q2 = "Which views show the split?";
        for (what, given, says) in [
            ("a pick that is no label", json!({ "answers": { q1: { "pick": ["Euro"] }, q2: "Card" } }), "“Euro” is not an option of “Which figure is the headline?” — one of: Output tokens | USD"),
            ("two picks on a single", json!({ "answers": { q1: { "pick": ["USD", "Output tokens"] }, q2: "Card" } }), "takes one answer, not 2"),
            ("no pick at all", json!({ "answers": { q1: { "pick": [] }, q2: "Card" } }), "no pick — one of: Output tokens | USD"),
            ("a question left out", json!({ "answers": { q1: "USD" } }), "“Which views show the split?” is not answered"),
            ("a question it was not asked", json!({ "answers": { q1: "USD", q2: "Card", "Why?": "x" } }), "“Why?” is not one of the questions"),
            ("an empty Other", json!({ "answers": { q1: { "other": " " }, q2: "Card" } }), "pick: [label…]} or {other: text}"),
            ("an empty string", json!({ "answers": { q1: " ", q2: "Card" } }), "answered with nothing"),
            ("no answers key", json!({ "pick": ["USD"] }), "{answers: {<question>: <label>}}"),
            ("an allow on an ask", json!({ "behavior": "allow" }), "answered with its options, or declined with deny"),
            ("not an object", json!("USD"), "an answer is a JSON object"),
        ] {
            let got = c.accept(&given, "").expect_err(what);
            assert!(got.contains(says), "{what}: {got}");
        }
        assert!(c.accept(&json!({ "answers": {} }), "and a sentence").unwrap_err().contains("two answers"));
    }

    #[test]
    fn talking_instead_declines_an_ask_with_a_sentence() {
        let c = ask();
        let (v, text) = c.accept(&json!({ "behavior": "deny", "message": "ask me in person" }), "").unwrap();
        assert_eq!(v, json!({ "behavior": "deny", "message": "ask me in person" }));
        assert_eq!(text, "did not choose; said: ask me in person");
        let (_, via_typed) = c.accept(&json!({ "behavior": "deny" }), "ask me in person").unwrap();
        assert_eq!(via_typed, text, "the sentence beside --choice is the message");
    }

    #[test]
    fn plain_words_answer_a_single_question_and_nothing_wider() {
        let one = Choices::parse(&json!({ "kind": "ask", "questions": [
            { "question": "Land now?", "header": "Land", "options": [{ "label": "Land now" }, { "label": "Wait" }] }] }))
        .unwrap();
        assert_eq!(one.accept_words("Wait").unwrap(), (json!({ "answers": { "Land now?": { "pick": ["Wait"] } } }), "Land: Wait".into()));
        assert_eq!(one.accept_words("not yet, I want to read it").unwrap().1, "Land: not yet, I want to read it");
        for what in [ask(), perm()] {
            let got = what.accept_words("yes").unwrap_err();
            assert!(got.contains("--choice") && got.contains(&what.options_line()), "{got}");
        }
    }

    #[test]
    fn a_plan_or_permission_is_allowed_or_denied() {
        let plan = Choices::parse(&json!({ "kind": "plan", "plan": "# steps" })).unwrap();
        let p = perm();
        // (the question, the answer, the typed sentence, what is stored, what is read)
        for (c, given, typed, stored, said) in [
            (&plan, json!({ "behavior": "allow" }), "", json!({ "behavior": "allow" }), "allowed"),
            (&plan, json!({ "behavior": "allow", "mode": "acceptEdits" }), "", json!({ "behavior": "allow", "mode": "acceptEdits" }), "allowed, then acceptEdits"),
            (&plan, json!({ "behavior": "deny", "message": "add a rollback step" }), "", json!({ "behavior": "deny", "message": "add a rollback step" }), "denied: add a rollback step"),
            (&plan, json!({ "behavior": "deny" }), "add a rollback step", json!({ "behavior": "deny", "message": "add a rollback step" }), "denied: add a rollback step"),
            (&p, json!({ "behavior": "deny" }), "", json!({ "behavior": "deny" }), "denied"),
            (
                &p,
                json!({ "behavior": "allow", "rule": { "type": "setMode", "mode": "acceptEdits" } }),
                "",
                json!({ "behavior": "allow", "rule": { "type": "setMode", "mode": "acceptEdits" } }),
                "allowed, and always by the rule offered",
            ),
            (&p, json!({ "behavior": "allow", "rule": null, "extra": 1 }), "", json!({ "behavior": "allow" }), "allowed"),
        ] {
            let (v, text) = c.accept(&given, typed).unwrap_or_else(|e| panic!("{given}: {e}"));
            assert_eq!((v, text.as_str()), (stored, said), "{given}");
        }
        for (what, c, given, says) in [
            ("a rejection with no reason", &plan, json!({ "behavior": "deny" }), "takes a sentence"),
            ("a behavior it does not know", &p, json!({ "behavior": "maybe" }), "allow or deny"),
            ("no behavior", &p, json!({}), "allow or deny"),
            ("a rule it did not offer", &p, json!({ "behavior": "allow", "rule": { "type": "addRules" } }), "offers 1"),
            ("a rule when none is offered", &plan, json!({ "behavior": "allow", "rule": { "type": "addRules" } }), "offers no rule"),
            ("a rule on a deny", &p, json!({ "behavior": "deny", "rule": {} }), "go with allow"),
            ("an empty mode", &p, json!({ "behavior": "allow", "mode": "" }), "permission mode"),
            ("an ask's answers", &p, json!({ "answers": {} }), "allow or deny"),
        ] {
            let got = c.accept(&given, "").expect_err(what);
            assert!(got.contains(says), "{what}: {got}");
        }
    }
}
