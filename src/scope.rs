//! What a barrier runs, scoped to what its group touched (`wsp-431`).
//!
//! Ed, 2026-11-06: *"if we're doing components, we likely don't need to run
//! full-suite tests so often, even at barriers (assuming no files are touched
//! that alter multiple components)."* See `docs/scoped-barriers.md`.
//!
//! This module decides; [`crate::barrier_run`] applies it. The decision is a
//! function of the group's **kind**, whether it is the list's last, the files
//! its members landed (observed from the landed commits, never declared), the
//! project's `shared:` and `global:` paths, and how many scoped barriers have
//! run since the last full one. [`decide`] is pure; [`narrow`] needs the barrier
//! tree, because the targets a diff reaches come from the crate graph and the
//! module graph ([`crate::affected::derived`]).

use std::path::Path;

use serde_json::{json, Value};

use crate::barrier_run::Item;
use crate::check::{captures, Step};
use crate::store::Store;

/// What a group is for, `wsp worklist group <list> N --kind …`. The barrier's
/// policy follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupKind {
    /// The contract and types every other group builds on: fans out to everything.
    Contract,
    /// Component and service units: the barrier runs the units touched.
    Components,
    /// Screens wired from components: the screens touched and their dependents.
    Wiring,
    /// The batch's in-situ step: the full suite plus the drive.
    Insitu,
}

impl GroupKind {
    pub(crate) fn parse(word: &str) -> Option<GroupKind> {
        match word.trim() {
            "contract" => Some(GroupKind::Contract),
            "components" => Some(GroupKind::Components),
            "wiring" => Some(GroupKind::Wiring),
            "insitu" => Some(GroupKind::Insitu),
            _ => None,
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            GroupKind::Contract => "contract",
            GroupKind::Components => "components",
            GroupKind::Wiring => "wiring",
            GroupKind::Insitu => "insitu",
        }
    }
}

/// The scoping a project declares under `## Barrier` in its body.
///
/// ```text
/// shared: src/types/**, src/services/**   (fan out: run their dependents)
/// global: scripts/ci.sh                   (beside the built-in lockfile and build config: the full suite)
/// full-every: 5                           (the trunk safety net: a full run every Nth scoped barrier; 0 = never)
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Config {
    pub shared: Vec<String>,
    pub global: Vec<String>,
    pub full_every: Option<usize>,
}

/// How many scoped barriers a project goes between full runs when it says nothing.
pub(crate) const FULL_EVERY: usize = 5;

impl Config {
    pub(crate) fn of(store: &Store, project: &str) -> Config {
        store.project(project).and_then(|p| p.section("Barrier")).map(|s| Config::parse(&s)).unwrap_or_default()
    }

    pub(crate) fn parse(text: &str) -> Config {
        let mut c = Config::default();
        let list = |v: &str| v.split(',').map(|x| x.trim().trim_matches('`').to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>();
        for l in text.lines() {
            let l = l.trim().trim_start_matches("- ").trim_start_matches("* ");
            if let Some(v) = l.strip_prefix("shared:") {
                c.shared.extend(list(v));
            } else if let Some(v) = l.strip_prefix("global:") {
                c.global.extend(list(v));
            } else if let Some(v) = l.strip_prefix("full-every:") {
                c.full_every = v.trim().parse().ok();
            }
        }
        c
    }
}

/// Paths whose change reaches everything, whatever the project says: the
/// lockfiles, the manifests at the root, and build configuration.
pub(crate) fn built_in_global(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let root = !path.contains('/');
    matches!(
        name,
        "Cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "rust-toolchain" | "rust-toolchain.toml" | "build.rs"
    ) || (root && matches!(name, "Cargo.toml" | "package.json"))
        || path.starts_with(".cargo/")
        || name.starts_with("tsconfig")
        || name.starts_with("vite.config.")
        || name.starts_with("vitest.config.")
        || name.starts_with("webpack.config.")
}

fn matches_any(globs: &[String], path: &str) -> bool {
    globs.iter().any(|g| captures(g, path).is_some())
}

/// A path that is documentation and reaches no test.
fn prose(path: &str) -> bool {
    path.ends_with(".md") || path.starts_with("docs/")
}

/// How much of the suite a barrier runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every declared step, as before; the reason it is not narrower.
    Full(String),
    /// A full-suite step narrows to the targets the changed paths reach.
    /// `widened` names shared paths that fanned the run out to their dependents.
    Narrow { widened: Vec<String> },
}

impl Scope {
    pub(crate) fn is_full(&self) -> bool {
        matches!(self, Scope::Full(_))
    }
}

/// Everything the decision reads.
pub(crate) struct Ask<'a> {
    pub kind: Option<GroupKind>,
    /// The group is the list's last.
    pub last: bool,
    /// The paths the members landed.
    pub changed: &'a [String],
    pub config: &'a Config,
    /// Scoped barriers run on the project since its last full one.
    pub since_full: usize,
}

/// The scope of a barrier. A full run is the default and every doubt resolves to
/// it; a narrow one needs the kind to say so and nothing in the files to object.
pub(crate) fn decide(a: &Ask) -> Scope {
    if a.last {
        return Scope::Full("the list's last group always runs the full suite".into());
    }
    match a.kind {
        None => return Scope::Full("the group has no kind, so nothing says its barrier may be narrower".into()),
        Some(GroupKind::Insitu) => return Scope::Full("an in-situ group runs the full suite and the drive".into()),
        Some(GroupKind::Contract) => return Scope::Full("a contract group's change fans out to everything".into()),
        Some(GroupKind::Components | GroupKind::Wiring) => {}
    }
    let files: Vec<&String> = a.changed.iter().filter(|p| !prose(p)).collect();
    if a.changed.is_empty() {
        return Scope::Full("no landed files were observed to scope by".into());
    }
    if let Some(p) = files.iter().find(|p| built_in_global(p) || matches_any(&a.config.global, p)) {
        return Scope::Full(format!("a global path changed: {p}"));
    }
    let every = a.config.full_every.unwrap_or(FULL_EVERY);
    if every > 0 && a.since_full >= every {
        return Scope::Full(format!("safety net: {} scoped barrier(s) since the last full run", a.since_full));
    }
    let widened: Vec<String> = files.iter().filter(|p| matches_any(&a.config.shared, p)).map(|p| p.to_string()).collect();
    Scope::Narrow { widened }
}

/// Whether a command is the whole suite of a stack: `cargo test` with no
/// filter, `npx vitest run`, `npm test`. Anything chained or filtered is
/// somebody's own choice and stays as written.
pub(crate) fn full_suite_command(command: &str) -> bool {
    let c = command.trim();
    if c.chars().any(|ch| matches!(ch, ';' | '|' | '&' | '>' | '<' | '`' | '$' | '(' | '\n')) {
        return false;
    }
    let words: Vec<String> = c.split_whitespace().map(str::to_string).collect();
    let Some(tool) = words.first() else { return false };
    if tool == "cargo" {
        return crate::throttle::is_full_suite("cargo", &words[1..]);
    }
    matches!(
        words.iter().map(String::as_str).collect::<Vec<_>>().as_slice(),
        ["npm", "test"] | ["npm", "run", "test"] | ["npx", "vitest"] | ["npx", "vitest", "run"] | ["npx", "vitest", "--run"] | ["vitest", "run"]
    )
}

/// The React whole suite: `npm run check`, or `npm run check:box` with no box,
/// optionally after one leading `cd <dir> &&`. Returns the `cd <dir> && ` prefix
/// (empty without one) and the directory (`""` without one). A box, a path, or
/// anything else chained is somebody's own choice and stays as written (`wsp-468`).
pub(crate) fn react_suite_command(command: &str) -> Option<(String, String)> {
    let c = command.trim();
    if c.chars().any(|ch| matches!(ch, ';' | '|' | '>' | '<' | '`' | '$' | '(' | '\n')) {
        return None;
    }
    let (dir, inner) = match c.strip_prefix("cd ") {
        Some(rest) => {
            let (d, i) = rest.split_once("&&")?;
            let d = d.trim();
            if d.is_empty() || d.contains(char::is_whitespace) || d.starts_with('/') || d.split('/').any(|p| p == "..") {
                return None;
            }
            (d.trim_end_matches('/').to_string(), i.trim())
        }
        None => (String::new(), c),
    };
    if inner.contains('&') {
        return None;
    }
    let words: Vec<&str> = inner.split_whitespace().collect();
    match words.as_slice() {
        ["npm", "run", "check"] | ["npm", "run", "check:box"] | ["npm", "run", "check:box", "--"] => {}
        _ => return None,
    }
    let prefix = if dir.is_empty() { String::new() } else { format!("cd {dir} && ") };
    Some((prefix, dir))
}

/// Directories under a React package's `src/` that fan out to every box.
const SHARED_BOXES: [&str; 3] = ["contract", "services", "shell"];

/// The boxes of the package at `dir` that `changed` touched, or `None` when the
/// run cannot be proved to stay in them: a shared box, the package's own
/// manifest, a file in the package outside `src/<box>/`, or nothing in the
/// package to place. (Paths the project declares `shared:` arrive as the
/// scope's `widened`.)
fn boxes_touched(dir: &str, changed: &[String]) -> Option<Vec<String>> {
    let root = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    let mut boxes: Vec<String> = Vec::new();
    for p in changed.iter().filter(|p| !prose(p)) {
        let Some(rel) = p.strip_prefix(root.as_str()) else { continue };
        let mut parts = rel.split('/');
        let (Some("src"), Some(b), Some(_)) = (parts.next(), parts.next(), parts.next()) else { return None };
        if SHARED_BOXES.contains(&b) {
            return None;
        }
        if !boxes.iter().any(|x| x == b) {
            boxes.push(b.to_string());
        }
    }
    if boxes.is_empty() {
        None
    } else {
        boxes.sort();
        Some(boxes)
    }
}

/// What narrowing did to a unit's steps.
pub(crate) struct Narrowed {
    pub items: Vec<Item>,
    /// The scope as applied: a full scope, or a narrow one that held, or a
    /// narrow one the graph could not honour and went back to full.
    pub scope: Scope,
    /// The targets that replaced a full-suite step, by name.
    pub targets: Vec<String>,
    /// Whether a whole-suite step was removed or replaced. A narrow scope on a
    /// unit with none (only a member's own script) narrowed nothing (`wsp-439`).
    pub narrowed: bool,
}

/// Apply a narrow scope to a unit's items in `tree`, at the commit it stands at.
///
/// Each full-suite step that is not a stop check is replaced by the targets the
/// changed paths reach ([`crate::affected::derived`]), each keeping the step's
/// owners; a target two steps would run is run once. **When the graph reaches
/// nothing for files that are not prose, the full steps stay**: a change the
/// graph cannot place is not proved covered by running nothing.
pub(crate) fn narrow(tree: &Path, items: &[Item], scope: &Scope, changed: &[String]) -> Narrowed {
    let keep = |scope: Scope| Narrowed { items: items.to_vec(), scope, targets: Vec::new(), narrowed: false };
    let whole = |i: &Item| i.kind != crate::barrier_run::Kind::Stop && full_suite_command(&i.step.command);
    let react = |i: &Item| react_suite_command(&i.step.command).is_some();
    if scope.is_full() || !items.iter().any(|i| whole(i) || react(i)) {
        return keep(scope.clone());
    }
    let widened = matches!(scope, Scope::Narrow { widened } if !widened.is_empty());
    let mut derived = Vec::new();
    if items.iter().any(whole) {
        derived = crate::affected::derived(tree, changed);
        let real = changed.iter().any(|p| !prose(p));
        if derived.is_empty() && real {
            return keep(Scope::Full("the crate and module graphs reach nothing for the changed files".into()));
        }
    }
    let mut out: Vec<Item> = Vec::new();
    let mut targets: Vec<String> = Vec::new();
    let mut narrowed = false;
    for item in items {
        if let Some((prefix, dir)) = react_suite_command(&item.step.command) {
            // The React suite in a middle group: the boxes touched, unless a
            // shared path changed (`wsp-468`).
            match boxes_touched(&dir, changed).filter(|_| !widened) {
                Some(boxes) => {
                    narrowed = true;
                    for b in boxes {
                        let name = format!("{}:{b}", item.step.name);
                        if !targets.contains(&name) {
                            targets.push(name.clone());
                        }
                        let step = Step { name, command: format!("{prefix}npm run check:box -- {b}"), ..item.step.clone() };
                        push(&mut out, Item { step, kind: item.kind, owners: item.owners.clone() });
                    }
                }
                None => push(&mut out, item.clone()),
            }
            continue;
        }
        if !whole(item) {
            push(&mut out, item.clone());
            continue;
        }
        narrowed = true;
        for (step, _) in &derived {
            if !targets.contains(&step.name) {
                targets.push(step.name.clone());
            }
            let step = Step { barrier: item.step.barrier, limit: item.step.limit.max(step.limit), ..step.clone() };
            push(&mut out, Item { step, kind: item.kind, owners: item.owners.clone() });
        }
    }
    Narrowed { items: out, scope: scope.clone(), targets, narrowed }
}

/// A command named twice runs once, and the owners of both share the answer.
fn push(items: &mut Vec<Item>, item: Item) {
    if let Some(have) = items.iter_mut().find(|i| i.kind != crate::barrier_run::Kind::Stop && i.step.command == item.step.command) {
        for o in item.owners {
            if !have.owners.contains(&o) {
                have.owners.push(o);
            }
        }
        return;
    }
    items.push(item);
}

// ---- what is kept ----------------------------------------------------------

/// A project's count of scoped barriers since its last full one, and how long a
/// full run took last: `barrier/<project>-scope.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Net {
    /// `list/group` of each scoped barrier since the last full run.
    pub since_full: Vec<String>,
    pub full_secs: Option<u64>,
}

fn net_file(store: &Store, project: &str) -> std::path::PathBuf {
    store.state_file("barrier").join(format!("{project}-scope.json"))
}

impl Net {
    pub(crate) fn read(store: &Store, project: &str) -> Net {
        let v: Value = std::fs::read_to_string(net_file(store, project)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        Net {
            since_full: v["since_full"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect(),
            full_secs: v["full_secs"].as_u64(),
        }
    }

    pub(crate) fn write(&self, store: &Store, project: &str) {
        let file = net_file(store, project);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, json!({ "since_full": self.since_full, "full_secs": self.full_secs }).to_string());
    }
}

/// What one group's barrier did about scope, kept for the effort report:
/// `barrier/<list>-<n>.scope.json`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Note {
    pub scoped: bool,
    pub why: String,
    pub targets: Vec<String>,
    pub widened: Vec<String>,
    pub seconds: u64,
    /// What a full run took last on this project, when one has.
    pub full_seconds: Option<u64>,
    /// A full run that was red after scoped barriers: what scoping missed, and
    /// the scoped barriers it ran behind.
    pub net_red: Vec<String>,
}

impl Note {
    pub(crate) fn saved(&self) -> u64 {
        match (self.scoped, self.full_seconds) {
            (true, Some(f)) => f.saturating_sub(self.seconds),
            _ => 0,
        }
    }

    pub(crate) fn file(store: &Store, list: &str, at: usize) -> std::path::PathBuf {
        store.state_file("barrier").join(format!("{list}-{at}.scope.json"))
    }

    pub(crate) fn write(&self, store: &Store, list: &str, at: usize) {
        let file = Note::file(store, list, at);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, self.json().to_string());
    }

    pub(crate) fn read(store: &Store, list: &str, at: usize) -> Option<Note> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(Note::file(store, list, at)).ok()?).ok()?;
        let strs = |k: &str| v[k].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>();
        Some(Note {
            scoped: v["scoped"].as_bool()?,
            why: v["why"].as_str().unwrap_or("").to_string(),
            targets: strs("targets"),
            widened: strs("widened"),
            seconds: v["seconds"].as_u64().unwrap_or(0),
            full_seconds: v["full_seconds"].as_u64(),
            net_red: strs("net_red"),
        })
    }

    /// The report's figures for the group.
    pub(crate) fn json(&self) -> Value {
        json!({
            "scoped": self.scoped, "why": self.why, "targets": self.targets, "widened": self.widened,
            "seconds": self.seconds, "full_seconds": self.full_seconds, "seconds_saved": self.saved(),
            "net_red": self.net_red,
        })
    }

    /// The words in the barrier run's log line.
    pub(crate) fn line(&self) -> String {
        if self.scoped {
            let saved = if self.full_seconds.is_some() { format!(", ~{}s under a full run", self.saved()) } else { String::new() };
            let widened = if self.widened.is_empty() { String::new() } else { format!(", widened by shared {}", self.widened.join(" ")) };
            format!("scoped to {}{widened}{saved}", if self.targets.is_empty() { "no tests (prose only)".to_string() } else { self.targets.join(" ") })
        } else {
            let red = if self.net_red.is_empty() { String::new() } else { format!(" · SAFETY-NET RED behind scoped {}", self.net_red.join(" ")) };
            format!("full — {}{red}", self.why)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    fn ask<'a>(kind: Option<GroupKind>, last: bool, changed: &'a [String], config: &'a Config, since_full: usize) -> Ask<'a> {
        Ask { kind, last, changed, config, since_full }
    }

    #[test]
    fn a_components_group_that_touched_units_only_is_narrow_and_everything_else_is_full() {
        let c = Config::default();
        let units = paths(&["web/src/button/Button.tsx", "web/src/card/Card.test.tsx"]);
        assert_eq!(decide(&ask(Some(GroupKind::Components), false, &units, &c, 0)), Scope::Narrow { widened: vec![] });
        assert_eq!(decide(&ask(Some(GroupKind::Wiring), false, &units, &c, 0)), Scope::Narrow { widened: vec![] });
        for (kind, last, why) in [
            (Some(GroupKind::Components), true, "last group"),
            (Some(GroupKind::Insitu), false, "in-situ"),
            (Some(GroupKind::Contract), false, "contract"),
            (None, false, "no kind"),
        ] {
            let s = decide(&ask(kind, last, &units, &c, 0));
            assert!(matches!(&s, Scope::Full(w) if w.contains(why)), "{kind:?} {last}: {s:?}");
        }
        let none: Vec<String> = Vec::new();
        assert!(decide(&ask(Some(GroupKind::Components), false, &none, &c, 0)).is_full(), "nothing observed, nothing to scope by");
    }

    #[test]
    fn a_global_path_widens_to_the_full_suite_and_a_shared_one_to_its_dependents() {
        let mut c = Config::parse("auto-pass: on\nshared: src/types/**, src/services/**\nglobal: scripts/ci.sh\nfull-every: 3\n");
        assert_eq!(c.shared, vec!["src/types/**", "src/services/**"]);
        assert_eq!(c.global, vec!["scripts/ci.sh"]);
        assert_eq!(c.full_every, Some(3));
        for p in ["Cargo.lock", "package.json", "web/package-lock.json", "tsconfig.base.json", "vite.config.ts", "scripts/ci.sh"] {
            let ch = paths(&["web/src/a/A.tsx", p]);
            let s = decide(&ask(Some(GroupKind::Components), false, &ch, &c, 0));
            assert!(matches!(&s, Scope::Full(w) if w.contains(p)), "{p}: {s:?}");
        }
        assert!(!built_in_global("web/package.json") || built_in_global("package.json"), "only the root manifest is global");
        let ch = paths(&["src/types/user.ts", "src/a/A.tsx"]);
        assert_eq!(decide(&ask(Some(GroupKind::Components), false, &ch, &c, 0)), Scope::Narrow { widened: vec!["src/types/user.ts".into()] });
        c.full_every = Some(3);
        let s = decide(&ask(Some(GroupKind::Components), false, &ch, &c, 3));
        assert!(matches!(&s, Scope::Full(w) if w.contains("safety net")), "{s:?}");
        c.full_every = Some(0);
        assert!(!decide(&ask(Some(GroupKind::Components), false, &ch, &c, 99)).is_full(), "full-every: 0 turns the net off");
        assert!(matches!(decide(&ask(Some(GroupKind::Components), false, &ch, &Config::default(), FULL_EVERY)), Scope::Full(_)), "five by default");
    }

    #[test]
    fn only_a_whole_suite_command_is_narrowed() {
        for c in ["cargo test", "cargo test --release", "cargo nextest run", "npm test", "npx vitest run", "vitest run"] {
            assert!(full_suite_command(c), "{c}");
        }
        for c in ["cargo test -p host", "cargo nextest run -p host", "cargo nextest list", "cargo test throttle::", "cargo test && npm run e2e", "sh suite.sh", "npx vitest related --run a.ts", "cargo build"] {
            assert!(!full_suite_command(c), "{c}");
        }
    }

    #[test]
    fn a_note_round_trips_and_says_what_scoping_saved() {
        let n = Note { scoped: true, why: String::new(), targets: vec!["crate:a".into()], widened: vec![], seconds: 40, full_seconds: Some(400), net_red: vec![] };
        assert_eq!(n.saved(), 360);
        assert!(n.line().contains("scoped to crate:a") && n.line().contains("360s"));
        let f = Note { scoped: false, why: "safety net".into(), net_red: vec!["l/1".into()], ..n.clone() };
        assert_eq!(f.saved(), 0);
        assert!(f.line().contains("SAFETY-NET RED behind scoped l/1"));
    }

    /// A workspace of units: `ui` depends on `core`, and `cart` stands alone.
    fn workspace(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-scope-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers = [\"core\", \"ui\", \"cart\"]\nresolver = \"2\"\n").unwrap();
        for (n, deps) in [("core", ""), ("ui", "core = { path = \"../core\" }\n"), ("cart", "")] {
            std::fs::create_dir_all(dir.join(n).join("src")).unwrap();
            std::fs::write(dir.join(n).join("Cargo.toml"), format!("[package]\nname = \"{n}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\n{deps}")).unwrap();
            std::fs::write(dir.join(n).join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        }
        dir
    }

    fn items() -> Vec<Item> {
        let step = |name: &str, command: &str| Step {
            name: name.into(),
            command: command.into(),
            limit: std::time::Duration::from_secs(60),
            of: crate::check::Of::Project,
            on: Vec::new(),
            barrier: true,
            end: false,
        };
        vec![
            Item { step: step("full", "cargo test"), kind: crate::barrier_run::Kind::Suite, owners: vec!["pj-002".into()] },
            Item { step: step("mine", "sh mine.sh"), kind: crate::barrier_run::Kind::Member, owners: vec!["pj-001".into()] },
            Item { step: step("stop:gate", "cargo test"), kind: crate::barrier_run::Kind::Stop, owners: vec![] },
        ]
    }

    fn commands(n: &Narrowed) -> Vec<&str> {
        n.items.iter().map(|i| i.step.command.as_str()).collect()
    }

    #[test]
    fn a_components_group_that_touched_two_units_runs_only_their_tests() {
        let dir = workspace("two");
        let narrow_scope = Scope::Narrow { widened: vec![] };
        let n = narrow(&dir, &items(), &narrow_scope, &paths(&["cart/src/lib.rs", "cart/src/more.rs", "README.md"]));
        assert_eq!(commands(&n), vec!["cargo test -p cart", "sh mine.sh", "cargo test"], "the suite became the unit; a stop check and a member's own script stay as written");
        assert_eq!(n.targets, vec!["crate:cart"]);
        assert_eq!(n.items[0].owners, vec!["pj-002"], "whoever named the suite shares the unit's answer");
        // Two units: the one touched and a dependent of the other.
        let n = narrow(&dir, &items(), &narrow_scope, &paths(&["core/src/lib.rs", "cart/src/lib.rs"]));
        assert_eq!(n.targets, vec!["crate:cart", "crate:core", "crate:ui"], "core reaches its dependent ui; nothing else");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_scope_a_prose_only_group_and_a_change_the_graph_cannot_place_are_each_handled() {
        let dir = workspace("edges");
        let before = commands(&narrow(&dir, &items(), &Scope::Full("last".into()), &paths(&["cart/src/lib.rs"]))).join(",");
        assert_eq!(before, "cargo test,sh mine.sh,cargo test", "a full scope changes nothing");
        let docs = narrow(&dir, &items(), &Scope::Narrow { widened: vec![] }, &paths(&["docs/a.md", "cart/README.md"]));
        assert_eq!(commands(&docs), vec!["sh mine.sh", "cargo test"], "prose reaches no test, and the suite step is gone");
        assert!(docs.narrowed, "a suite step went, so the barrier was narrowed");
        let stray = narrow(&dir, &items(), &Scope::Narrow { widened: vec![] }, &paths(&["scripts/deploy.py"]));
        assert!(matches!(stray.scope, Scope::Full(ref w) if w.contains("reach nothing")), "{:?}", stray.scope);
        assert_eq!(commands(&stray), vec!["cargo test", "sh mine.sh", "cargo test"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_narrow_scope_on_a_unit_with_no_whole_suite_step_narrowed_nothing() {
        let dir = workspace("nothing");
        let only_mine: Vec<Item> = items().into_iter().filter(|i| i.step.command == "sh mine.sh").collect();
        let n = narrow(&dir, &only_mine, &Scope::Narrow { widened: vec![] }, &paths(&["cart/src/lib.rs"]));
        assert!(!n.narrowed && n.targets.is_empty() && commands(&n) == vec!["sh mine.sh"]);
        let with_suite = narrow(&dir, &items(), &Scope::Narrow { widened: vec![] }, &paths(&["cart/src/lib.rs"]));
        assert!(with_suite.narrowed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_middle_groups_react_check_narrows_to_the_boxes_touched_and_a_shared_path_keeps_it_whole() {
        let dir = workspace("react");
        let step = |name: &str, command: &str| Step {
            name: name.into(),
            command: command.into(),
            limit: std::time::Duration::from_secs(600),
            of: crate::check::Of::Project,
            on: Vec::new(),
            barrier: true,
            end: false,
        };
        let stop = |c: &str| vec![Item { step: step("stop:full", c), kind: crate::barrier_run::Kind::Stop, owners: vec![] }];
        let narrow_scope = Scope::Narrow { widened: vec![] };
        let cmd = "cd spikes/web-chrome && npm run check";
        let n = narrow(&dir, &stop(cmd), &narrow_scope, &paths(&["spikes/web-chrome/src/pane/Pane.tsx", "spikes/web-chrome/src/tabs/Tabs.tsx", "spikes/web-chrome/src/pane/x.test.tsx", "docs/a.md"]));
        assert_eq!(commands(&n), vec!["cd spikes/web-chrome && npm run check:box -- pane", "cd spikes/web-chrome && npm run check:box -- tabs"]);
        assert!(n.narrowed && n.items.iter().all(|i| i.kind == crate::barrier_run::Kind::Stop));
        for shared in ["spikes/web-chrome/src/services/api.ts", "spikes/web-chrome/src/contract/t.ts", "spikes/web-chrome/src/shell/App.tsx", "spikes/web-chrome/package.json", "spikes/web-chrome/src/main.tsx"] {
            let n = narrow(&dir, &stop(cmd), &narrow_scope, &paths(&["spikes/web-chrome/src/pane/Pane.tsx", shared]));
            assert_eq!(commands(&n), vec![cmd], "{shared}");
            assert!(!n.narrowed);
        }
        let n = narrow(&dir, &stop(cmd), &Scope::Narrow { widened: vec!["spikes/web-chrome/src/lib/a.ts".into()] }, &paths(&["spikes/web-chrome/src/pane/Pane.tsx"]));
        assert_eq!(commands(&n), vec![cmd], "a path the project calls shared");
        let n = narrow(&dir, &stop(cmd), &Scope::Full("last".into()), &paths(&["spikes/web-chrome/src/pane/Pane.tsx"]));
        assert_eq!(commands(&n), vec![cmd], "a last group runs it whole");
        let n = narrow(&dir, &stop("cd spikes/web-chrome && npm run check:box -- pane"), &narrow_scope, &paths(&["spikes/web-chrome/src/tabs/T.tsx"]));
        assert!(!n.narrowed, "a box already named stays as written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_react_whole_suite_is_read_as_one() {
        assert_eq!(react_suite_command("npm run check"), Some((String::new(), String::new())));
        assert_eq!(react_suite_command("cd web && npm run check:box"), Some(("cd web && ".into(), "web".into())));
        for c in ["npm run check:box -- pane", "cd web && npm run check && echo", "cd /abs && npm run check", "cd ../x && npm run check", "npm test", "npm run check; x"] {
            assert_eq!(react_suite_command(c), None, "{c}");
        }
    }
}
