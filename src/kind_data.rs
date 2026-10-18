//! A kind that is a file rather than a Rust arm (wsp-113, reading 1).
//!
//! `<store.root>/kinds/<name>.json` declares what wsp can honestly take as data
//! about an agent runtime: its argv, how it resumes, what empties its context,
//! whether it arrives briefed. It declares nothing wsp would have to *believe*:
//! no tier vocabulary (so [`Kind::tier`] refuses, as [`Plain`] does), no
//! address (wsp never guesses a handle), no liveness, no transcript, no
//! environment. Delivery is typing at the pane and may stay `Unconfirmed`. A
//! data kind adds no transport; an agent declaring its own is a later row.
//!
//! The file is a compatibility surface and is held to a version: `kind_version`
//! must be present and `1`, unknown keys are refused rather than ignored, and
//! every refusal carries its reason. Unlike `store::read_json` nothing here is
//! swallowed — a file wsp cannot read is reported, not treated as absent.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_commands::{Kind, Plain, Spawn};
use crate::place::{Delivery, Place, Result, Seat};

/// The only shape of a kind file wsp reads.
pub const KIND_VERSION: u64 = 1;

/// Names the code owns. A file may not shadow one: built-ins win.
const BUILT_IN: &[&str] = &["claude", "opencode", "pi"];

const KEYS: &[&str] = &[
    "kind_version",
    "name",
    "args",
    "resume_flag",
    "clear",
    "briefed",
    "order_in_args",
    "brief_file",
    "queue_is_the_agents",
];

/// What a kind file declares. Strings are leaked once: a kind lives for the
/// process, and [`Kind`] answers `&'static`.
pub struct DataKind {
    pub name: &'static str,
    args: Vec<String>,
    resume_flag: Option<&'static str>,
    clear: Option<(&'static str, bool)>,
    briefed: bool,
    order_in_args: bool,
    brief_file: bool,
    queue_is_the_agents: bool,
}

impl Kind for DataKind {
    /// The declared argv, then the resume flag and session when both exist,
    /// then — only for a kind declaring `order_in_args` — the order as the last
    /// word. No templating: nothing declared is interpolated.
    fn args(&self, spawn: &Spawn) -> Vec<String> {
        let mut argv = self.args.clone();
        if let (Some(flag), Some(session)) = (self.resume_flag, spawn.resume) {
            argv.push(flag.into());
            argv.push(session.to_string());
        }
        if self.order_in_args {
            if let Some(order) = spawn.order {
                argv.push(order.to_string());
            }
        }
        argv
    }

    fn tier(&self, model: Option<&str>, effort: Option<&str>) -> std::result::Result<(), String> {
        Plain.tier(model, effort)
    }

    fn address(&self, _place: &dyn Place, _spawn: &Spawn) -> Option<crate::agent_commands::Address> {
        None
    }

    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<Delivery> {
        crate::place::submit(place, seat, text, &crate::place::Pressing::default())
    }

    fn running(&self, _spawn: &Spawn) -> Option<bool> {
        None
    }

    fn ran(&self, _session: &str, _cwd: &str, _since: i64) -> Option<crate::agent_commands::Ran> {
        None
    }

    fn briefed(&self) -> bool {
        self.briefed
    }

    fn order_in_args(&self) -> bool {
        self.order_in_args
    }

    fn brief_file(&self) -> bool {
        self.brief_file
    }

    fn queue_is_the_agents(&self) -> bool {
        self.queue_is_the_agents
    }

    fn resume_flag(&self) -> Option<&'static str> {
        self.resume_flag
    }

    fn clear_command(&self) -> Option<&'static str> {
        self.clear.map(|(c, _)| c)
    }

    fn clear_replaces_session(&self) -> bool {
        self.clear.map_or(true, |(_, r)| r)
    }
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn name_ok(n: &str) -> bool {
    (1..=32).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn flag<'a>(obj: &'a serde_json::Map<String, Value>, key: &str, default: bool) -> std::result::Result<bool, String> {
    match obj.get(key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("`{key}` must be true or false")),
    }
}

fn word<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> std::result::Result<Option<&'a str>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s)),
        Some(_) => Err(format!("`{key}` must be a non-empty string")),
    }
}

/// Read one kind file. `stem` is the file name without `.json` and must equal
/// the declared `name`. Every refusal says why.
pub fn parse(stem: &str, text: &str) -> std::result::Result<DataKind, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let obj = v.as_object().ok_or("must be a JSON object")?;
    match obj.get("kind_version") {
        None => return Err(format!("missing `kind_version` (wsp reads version {KIND_VERSION})")),
        Some(Value::Number(n)) if n.as_u64() == Some(KIND_VERSION) => {}
        Some(other) => return Err(format!("`kind_version` is {other}, wsp reads only {KIND_VERSION}")),
    }
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown key `{k}`"));
    }
    let name = word(obj, "name")?.ok_or("missing `name`")?;
    if !name_ok(name) {
        return Err(format!("bad name `{name}`: 1-32 of a-z, 0-9, `-`"));
    }
    if BUILT_IN.contains(&name) {
        return Err(format!("`{name}` is a built-in kind and cannot be shadowed"));
    }
    if name != stem {
        return Err(format!("name `{name}` does not match the file name `{stem}`"));
    }
    let args = match obj.get("args") {
        None => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().map(str::to_string).ok_or("`args` must be an array of strings"))
            .collect::<std::result::Result<_, _>>()?,
        Some(_) => return Err("`args` must be an array of strings".into()),
    };
    let clear = match obj.get("clear") {
        None | Some(Value::Null) => None,
        Some(Value::Object(c)) => {
            if let Some(k) = c.keys().find(|k| *k != "cmd" && *k != "replaces_session") {
                return Err(format!("unknown key `clear.{k}`"));
            }
            let cmd = word(c, "cmd")?.ok_or("`clear.cmd` is required")?;
            Some((leak(cmd), flag(c, "replaces_session", true)?))
        }
        Some(_) => return Err("`clear` must be an object {cmd, replaces_session}".into()),
    };
    Ok(DataKind {
        name: leak(name),
        args,
        resume_flag: word(obj, "resume_flag")?.map(leak),
        clear,
        briefed: flag(obj, "briefed", false)?,
        order_in_args: flag(obj, "order_in_args", false)?,
        brief_file: flag(obj, "brief_file", false)?,
        queue_is_the_agents: flag(obj, "queue_is_the_agents", true)?,
    })
}

/// Every `*.json` in `dir`, sorted by file name: the kinds that parsed, and the
/// files that did not with the reason. A missing directory is empty, not an
/// error; an unreadable file is a refusal.
pub fn load_dir(dir: &Path) -> (Vec<DataKind>, Vec<(PathBuf, String)>) {
    let mut kinds = Vec::new();
    let mut refused = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return (kinds, refused) };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for p in files {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| parse(stem, &t)) {
            Ok(k) => kinds.push(k),
            Err(why) => refused.push((p, why)),
        }
    }
    (kinds, refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place::{Agent, Event, Order, Seated, State};

    const GOOD: &str = r#"{"kind_version":1,"name":"zed","args":["--yes"],"resume_flag":"--continue","clear":{"cmd":"/new","replaces_session":false},"briefed":true}"#;

    fn spawn<'a>(seat: &'a Seat, order: Option<&'a str>, resume: Option<&'a str>) -> Spawn<'a> {
        Spawn { full: false, subagents: false, name: "t-1", seat, model: None, effort: None, order, resume }
    }

    #[test]
    fn a_declared_kind_is_read_as_declared() {
        let k = parse("zed", GOOD).expect("parses");
        let seat = Seat::new("w1:p1");
        assert_eq!(k.args(&spawn(&seat, Some("go"), Some("s1"))), ["--yes", "--continue", "s1"]);
        assert_eq!(k.clear_command(), Some("/new"));
        assert!(!k.clear_replaces_session());
        assert!(k.briefed() && !k.order_in_args() && !k.brief_file());
        assert!(k.queue_is_the_agents(), "a per-kind fact that defaults to true");
        assert!(parse("zed", &GOOD.replace("\"briefed\":true", "\"queue_is_the_agents\":false")).unwrap().queue_is_the_agents() == false);
    }

    #[test]
    fn the_order_is_in_argv_only_when_declared() {
        let k = parse("zed", r#"{"kind_version":1,"name":"zed","order_in_args":true}"#).unwrap();
        let seat = Seat::new("w1:p1");
        assert_eq!(k.args(&spawn(&seat, Some("go"), None)), ["go"]);
    }

    #[test]
    fn a_data_kind_never_guesses_and_refuses_a_tier() {
        let k = parse("zed", GOOD).unwrap();
        let seat = Seat::new("w1:p1");
        let sp = spawn(&seat, None, None);
        assert!(k.address(&Typed::new(), &sp).is_none());
        assert_eq!(k.running(&sp), None);
        assert!(k.ran("s", "/", 0).is_none());
        assert!(k.tier(Some("opus"), None).is_err());
        assert!(k.tier(None, Some("max")).is_err());
        assert!(k.tier(None, None).is_ok());
    }

    /// `docs/kinds.md`'s worked example is the first ```json block on the page.
    #[test]
    fn the_documented_example_parses() {
        let page = include_str!("../docs/kinds.md");
        let body = page.split("```json\n").nth(1).and_then(|r| r.split("\n```").next()).expect("a json block");
        let k = parse("mytool", body).unwrap_or_else(|e| panic!("docs/kinds.md example refused: {e}"));
        assert_eq!(k.name, "mytool");
        assert_eq!(k.clear_command(), Some("/new"));
    }

    #[test]
    fn refusals_say_why() {
        for (stem, text, why) in [
            ("zed", "{", "not valid JSON"),
            ("zed", r#"{"name":"zed"}"#, "missing `kind_version`"),
            ("zed", r#"{"kind_version":2,"name":"zed"}"#, "only 1"),
            ("zed", r#"{"kind_version":"1","name":"zed"}"#, "only 1"),
            ("zed", r#"{"kind_version":1,"name":"zed","models":["a"]}"#, "unknown key `models`"),
            ("zed", r#"{"kind_version":1,"name":"zed","clear":{"cmd":"x","wait":1}}"#, "unknown key `clear.wait`"),
            ("Zed", r#"{"kind_version":1,"name":"Zed"}"#, "bad name"),
            ("claude", r#"{"kind_version":1,"name":"claude"}"#, "built-in"),
            ("pi", r#"{"kind_version":1,"name":"pi"}"#, "built-in"),
            ("other", r#"{"kind_version":1,"name":"zed"}"#, "does not match"),
            ("zed", r#"{"kind_version":1,"name":"zed","args":[1]}"#, "array of strings"),
            ("zed", r#"{"kind_version":1,"name":"zed","briefed":"yes"}"#, "true or false"),
            ("zed", r#"[]"#, "JSON object"),
        ] {
            let e = parse(stem, text).err().unwrap_or_else(|| panic!("{text} was accepted"));
            assert!(e.contains(why), "{text}: `{e}` lacks `{why}`");
        }
    }

    #[test]
    fn a_directory_is_read_sorted_and_refusals_are_kept() {
        let dir = std::env::temp_dir().join(format!("wsp-kinds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_dir(&dir).0.is_empty() && load_dir(&dir).1.is_empty(), "missing dir is empty");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.json"), r#"{"kind_version":1,"name":"b"}"#).unwrap();
        std::fs::write(dir.join("a.json"), r#"{"kind_version":1,"name":"a"}"#).unwrap();
        std::fs::write(dir.join("bad.json"), "nope").unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        let (kinds, refused) = load_dir(&dir);
        assert_eq!(kinds.iter().map(|k| k.name).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(refused.len(), 1);
        assert!(refused[0].0.ends_with("bad.json") && refused[0].1.contains("JSON"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A backend whose delivery it could not watch: `Unconfirmed` stays reachable.
    struct Typed;
    impl Typed {
        fn new() -> Typed {
            Typed
        }
    }
    impl Place for Typed {
        fn tell(&self, _: &Seat, _: &str) -> Result<Delivery> {
            Ok(Delivery::Unconfirmed)
        }
        fn census(&self) -> Result<crate::place::Census> {
            Ok(crate::place::Census::heard("", Vec::<Seated>::new()))
        }
        fn open(&self, _: &Order) -> Result<Seat> {
            panic!("no")
        }
        fn start(&self, _: &Seat, _: &Agent) -> Result<()> {
            panic!("no")
        }
        fn stop(&self, _: &Seat) -> Result<()> {
            panic!("no")
        }
        fn state(&self, _: &Seat) -> Result<State> {
            panic!("no")
        }
        fn watch(&self, _: &mut dyn FnMut(Event) -> bool) -> Result<()> {
            panic!("no")
        }
        fn here(&self) -> Option<Seat> {
            None
        }
    }

    #[test]
    fn unconfirmed_is_still_reachable_through_a_data_kind() {
        let k = parse("zed", GOOD).unwrap();
        let got = k.tell(&Typed, &Seat::new("w1:p1"), "go");
        assert_eq!(got, Ok(Delivery::Unconfirmed));
    }
}
