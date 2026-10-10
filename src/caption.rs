//! Identity captions: one line of plain words for a task, a worklist and a
//! group, authored or derived — compound's `docs/ux-design.md` §17, `wsp-240`.
//!
//! **Why a field and not a cut title.** gpui-migration's titles have a median
//! of 96 characters and a sidebar row shows 26–36, so what a glance reads
//! today is whatever the column's width happens to leave. A title is written
//! to be precise; a caption is written to be read in one look, and the two
//! are different sentences often enough (13 of those 44 titles are still over
//! 60 after [`title`]) that one cannot stand in for the other.
//!
//! **Why wsp derives and the chrome only cuts.** One implementation of the
//! fallback, here, so `wsp ls`, the panel and compound's window never
//! disagree about what a row is called. The chrome gets the finished line and
//! [`Caption::from`], and never re-derives.
//!
//! **Why no model summary** (§17.8): a summary is a surface that can lie, it
//! goes stale the moment the prose under it moves, and it costs a call per
//! row. Everything here is either what somebody wrote or a rule over the
//! title they wrote, so it can be wrong only in the ways the title already is.

use serde_json::{json, Value};

use crate::model::{Group, Task, Worklist};
use crate::util;

/// Past this an authored caption is refused at the verb. Measured in the
/// mockups: the widest place a caption is drawn on one line holds about this
/// many, so anything longer is a caption no surface shows whole.
pub const LIMIT: usize = 60;

/// Past this an authored caption is kept, with a note: most places the
/// sidebar draws one cut it here, and the words that matter belong in front.
pub const AIM: usize = 40;

/// One caption, and where its words came from.
///
/// An identity caption has no `word` and no `at` (§17.6) — those belong to a
/// stage, which is `wsp-242`'s — so this is the shape's other half only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caption {
    pub text: String,
    /// `authored`, `title`, `member` or `members` here. Named in `--json` so a
    /// test, and the chrome if Ed ever wants it (E-2 says not), can tell a line
    /// somebody wrote from one wsp worked out.
    pub from: &'static str,
}

impl Caption {
    pub fn json(&self) -> Value {
        json!({ "text": self.text, "from": self.from })
    }

    pub fn authored(&self) -> bool {
        self.from == "authored"
    }
}

/// A task's caption: the authored one, else its title's first clause.
pub fn of_task(t: &Task) -> Caption {
    identity(t.caption.as_deref(), &t.title)
}

/// A worklist's caption, on the same rule as a task's.
pub fn of_worklist(w: &Worklist) -> Caption {
    identity(w.caption.as_deref(), &w.title)
}

fn identity(authored: Option<&str>, title_text: &str) -> Caption {
    match authored.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => Caption { text: c.to_string(), from: "authored" },
        None => Caption { text: title(title_text), from: "title" },
    }
}

/// A group's caption: the authored one, else its members'.
///
/// One member is that member's caption; more is the first member's and
/// `and N more`. **Never the stop condition.** A group's caption is drawn
/// beside its barrier, and a line drawn from the stop there would be a summary
/// of the stop standing next to the stop — which compound's worklist-053 rule
/// forbids, and which would also put a judgement's question where a glance
/// expects the work's name.
///
/// `member` is how a member's caption is looked up, so the walk over the store
/// is the caller's and a test needs none. A member it cannot find is drawn as
/// its id: a group is never blank, and an id is what a missing record has.
pub fn of_group(g: &Group, member: impl Fn(&str) -> Option<Caption>) -> Caption {
    let own = g.caption.trim();
    if !own.is_empty() {
        return Caption { text: own.to_string(), from: "authored" };
    }
    let Some(first) = g.members.first() else {
        // A group with no members is a line in a file somebody is still
        // composing; saying so is the honest caption and is never an id.
        return Caption { text: "No members yet".into(), from: "members" };
    };
    let head = member(first).map(|c| c.text).unwrap_or_else(|| first.clone());
    match g.members.len() {
        1 => Caption { text: head, from: "member" },
        n => Caption { text: format!("{head} and {} more", n - 1), from: "members" },
    }
}

/// A title's first clause: the fallback for a task, a worklist, and so for a
/// group. A port of compound's `docs/evidence/compound-405/fallback.py`
/// `title()`, step for step, so the counts §17.4 measured are this function's.
///
/// 1. drop every parenthetical, backticks and `**`;
/// 2. drop a leading lowercase tag (`gpui: `, `host-gpui: `);
/// 3. cut before the first ` — `, `; ` or `: ` (in that order of preference)
///    that leaves three words or more — so `Cutover: the gpui window…` keeps
///    its `Cutover:`, a one-word clause being a label and not a caption;
/// 4. trim trailing punctuation, and capitalise only a word step 2 uncovered.
///    A title's own first word is its author's case: `gpui window idles…`
///    stays lowercase, because `gpui` is a name.
///
/// **Never empty.** A title that cleans to nothing — `(compound-1)` — is
/// returned whole, since the whole is still the only words there are.
pub fn title(t: &str) -> String {
    let whole = t.trim();
    let cleaned = clean(t);
    if cleaned.trim().is_empty() {
        return whole.to_string();
    }
    let (mut t, tagged) = match strip_tag(&cleaned) {
        Some(rest) => (rest.to_string(), true),
        None => (cleaned.clone(), false),
    };
    for cut in [" — ", "; ", ": "] {
        if let Some(i) = t.find(cut) {
            if i > 0 && t[..i].split_whitespace().count() >= 3 {
                t.truncate(i);
                break;
            }
        }
    }
    let t = t.trim_end_matches([' ', '.', ',', ';', ':']);
    // The prototype can return "" here (`gpui: …`); the spec's rule is that a
    // derivation never does.
    if t.trim().is_empty() {
        return whole.to_string();
    }
    if tagged {
        capital(t)
    } else {
        t.to_string()
    }
}

/// Step 1: the prototype's `clean`. Surrounding quotes and escaped quotes, as
/// a title pasted from a shell carries them; then every `(…)` with the space
/// in front of it; then the markdown a title is sometimes written in.
fn clean(t: &str) -> String {
    let t = t.trim().trim_matches('"').replace("\\\"", "\"");
    // `\s*\([^)]*\)`: from an opening paren to the first closing one after it,
    // with the whitespace run before it. An unclosed paren is kept, as the
    // regex keeps it.
    let mut out = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')') else { break };
        out.push_str(&rest[..open]);
        let kept = out.trim_end_matches(char::is_whitespace).len();
        out.truncate(kept);
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out.replace('`', "").replace("**", "")
}

/// Step 2: `^[a-z][a-z0-9-]*: `, and what follows it.
fn strip_tag(t: &str) -> Option<&str> {
    let (tag, rest) = t.split_once(": ")?;
    let mut b = tag.bytes();
    let ok = b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
    ok.then_some(rest)
}

/// Capitalise a plain lowercase word, and only that: `gpui`, `host-gpui`,
/// `parity.md` and `docs/AGENTS` are names and keep their case. The
/// prototype's `^[a-z]+(?![-._/\w])`.
fn capital(t: &str) -> String {
    let run = t.bytes().take_while(|b| b.is_ascii_lowercase()).count();
    let next = t[run..].chars().next();
    let plain = run > 0 && !next.is_some_and(|c| c.is_alphanumeric() || "_-./".contains(c));
    if !plain {
        return t.to_string();
    }
    let mut out = t[..1].to_ascii_uppercase();
    out.push_str(&t[1..]);
    out
}

/// An authored caption checked at the verb: the text to store, and a note for
/// the author when it is long enough that the sidebar will cut it.
///
/// Refused past [`LIMIT`] rather than cut, because a cut caption is one whose
/// author never saw what a glance reads; refusing says so while they are
/// still at the keyboard. Folded to one line, since a caption is a line. The
/// `Err` is the sentence to print.
pub fn check(raw: &str) -> Result<(String, Option<String>), String> {
    if let Some(why) = util::terminal_output(raw) {
        return Err(why);
    }
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("a caption is one line of words — `--clear` is how one is taken off".into());
    }
    let n = text.chars().count();
    if n > LIMIT {
        return Err(format!(
            "a caption is refused past {LIMIT} characters, and this is {n} — the words that matter, first: \"{text}\""
        ));
    }
    let note = (n > AIM).then(|| format!("{n} characters: the sidebar will cut it past about {AIM}"));
    Ok((text, note))
}

/// `--caption` on a verb that makes or edits a thing: `None` when it was not
/// given, `Some("")` for `--caption none` (taken off), else the checked text.
///
/// Reads `-` and `--from FILE` as every other prose intake does. `--from` is
/// this flag's only when `from_is_ours` — on `worklist group` it already
/// belongs to `--stop`, and the caller decides who gets it. The note past
/// [`AIM`] goes to stderr here, so every verb says it in the same words.
pub fn flag(args: &crate::Args, from_is_ours: bool) -> Result<Option<String>, i32> {
    let Some(raw) = args.get("caption") else { return Ok(None) };
    let streams = matches!(raw.trim(), "-" | "true");
    let from = if from_is_ours { args.get("from") } else { None };
    let raw = match (streams, from) {
        (false, Some(_)) => {
            eprintln!("wsp: a caption and --from are two captions — give one");
            return Err(2);
        }
        (false, None) if raw.trim() == "none" => return Ok(Some(String::new())),
        (false, None) => raw,
        (true, from) => {
            let src = match from.as_deref() {
                None | Some("true") | Some("-") => "-".to_string(),
                Some(path) => path.to_string(),
            };
            if src == "-" && util::stdin_is_tty() {
                eprintln!("wsp: nothing is piped in — `--caption -` reads the caption from a stream");
                return Err(2);
            }
            match crate::cmd_task::read_source(&src) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("wsp: cannot read {}: {e}", if src == "-" { "stdin".into() } else { util::contract(&util::expand(&src)) });
                    return Err(1);
                }
            }
        }
    };
    match check(&raw) {
        Ok((text, note)) => {
            if let Some(note) = note {
                eprintln!("  {}", util::Paint::new().dim(&note));
            }
            Ok(Some(text))
        }
        Err(why) => {
            eprintln!("wsp: {why}");
            Err(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cases wsp-240's Done when names, from gpui-migration's own titles:
    /// a tag and a parenthetical dropped and the dash cut; a one-word clause
    /// that is a label and not a caption, so nothing is cut; a parenthetical
    /// id; a colon cut that keeps the author's lowercase name; and a title
    /// that is nothing but a parenthetical, which comes back whole.
    #[test]
    fn a_title_falls_back_to_its_first_clause_on_gpui_migrations_own_titles() {
        let cases = [
            (
                "gpui: IME preedit (marked text) is not drawn in a pane — the wire has no preedit message",
                "IME preedit is not drawn in a pane",
            ),
            (
                "Cutover: the gpui window becomes host, and the imgui window, its dependencies, its goldens and its tests leave the tree",
                "Cutover: the gpui window becomes host, and the imgui window, its dependencies, its goldens and its tests leave the tree",
            ),
            ("The chrome leaves imgui for gpui (compound-332)", "The chrome leaves imgui for gpui"),
            (
                "gpui window idles at 1.1% of a core against imgui's 0.2%: …",
                "gpui window idles at 1.1% of a core against imgui's 0.2%",
            ),
            ("(compound-1)", "(compound-1)"),
        ];
        for (input, want) in cases {
            assert_eq!(title(input), want, "from {input:?}");
        }
    }

    /// Step 4's other half: a plain word a tag uncovered is capitalised, a
    /// name it uncovered is not, and a tag with nothing after it gives the
    /// whole title rather than nothing.
    #[test]
    fn only_a_plain_word_a_tag_uncovered_is_capitalised() {
        assert_eq!(title("gpui: the IME window sits at the top-left"), "The IME window sits at the top-left");
        assert_eq!(title("host: host-gpui draws the pane"), "host-gpui draws the pane");
        assert_eq!(title("docs: parity.md lists every gap"), "parity.md lists every gap");
        assert_eq!(title("gpui: ..."), "gpui: ...");
    }

    /// The dash is preferred to the semicolon and the semicolon to the colon,
    /// as the prototype tries them, and a short clause in front of one is
    /// skipped for the next kind rather than taken.
    #[test]
    fn the_cut_tries_the_dash_then_the_semicolon_then_the_colon() {
        assert_eq!(title("Spike: three spikes decide gpui — go or no-go"), "Spike: three spikes decide gpui");
        assert_eq!(title("Two words — then a long clause; and the rest of it here"), "Two words — then a long clause");
        assert_eq!(title("Backticks `wsp ls` and **bold** go away"), "Backticks wsp ls and bold go away");
    }

    #[test]
    fn a_caption_is_refused_past_sixty_and_noted_past_forty() {
        assert!(check(&"x".repeat(61)).is_err());
        let (text, note) = check(&"x".repeat(60)).unwrap();
        assert_eq!(text.len(), 60);
        assert!(note.unwrap().contains("the sidebar will cut it"));
        assert_eq!(check("  Close the five gaps\n blocking cutover ").unwrap(), ("Close the five gaps blocking cutover".into(), None));
        assert!(check("   ").is_err(), "an empty caption is not a caption");
    }

    fn group(members: &[&str], caption: &str) -> Group {
        Group { members: members.iter().map(|m| m.to_string()).collect(), caption: caption.into(), ..Default::default() }
    }

    /// The group fallback reads its members and never its stop condition: a
    /// group whose stop is a paragraph still captions as its first member.
    #[test]
    fn a_group_with_no_caption_is_named_by_its_members_and_never_its_stop() {
        let look = |id: &str| (id == "c-1").then(|| Caption { text: "Three spikes".into(), from: "title" });
        let mut g = group(&["c-1"], "");
        g.stop = "if any of the three goes badly, flag and stop".into();
        assert_eq!(of_group(&g, look), Caption { text: "Three spikes".into(), from: "member" });
        assert_eq!(
            of_group(&group(&["c-1", "c-2", "c-3"], ""), look),
            Caption { text: "Three spikes and 2 more".into(), from: "members" }
        );
        assert_eq!(of_group(&group(&["c-9"], ""), look).text, "c-9", "a member nobody can find is its id");
        assert_eq!(of_group(&group(&["c-1"], "Go or no-go on gpui"), look).from, "authored");
        assert!(!of_group(&group(&[], ""), look).text.is_empty());
    }
}
