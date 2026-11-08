//! The size check (`wsp-499`, part 2): a row whose overview shows it is more than
//! one seat's work is asked, before any seat is spent, to carry legs, pieces,
//! sub-tasks, or `--whole --why`.
//!
//! The signals are written down and simple, so a planner can predict them. A row
//! trips the check when its overview shows:
//!
//! - **(a) many observations**: a done-when with three or more separate
//!   observations — clauses split by `;`, bulleted or numbered items, or `(1)`
//!   markers, whichever counts most;
//! - **(b) build then drive**: a build or setup step stated at over ~5 minutes
//!   (a figure in minutes, or a `release build` or `cold build`) followed in the
//!   text by a drive, run or measure;
//! - **(c) the planner's own exception**: "exception to the small-row size" or
//!   "mostly the build".
//!
//! A row passes with `## Legs`, `## Pieces`, sub-tasks, or a `## Whole` the
//! planner recorded with `--whole --why`. The verifier sees the `## Whole`.
//!
//! The check is pure: it reads a row and its siblings, and the verbs that run it
//! (`wsp worklist add`, `wsp worklist status <list> planned`) decide what to do
//! with a hit. **The mode is a setting**, the file `size-check` in the state
//! directory, `warn` or `refuse` or `off`, set by `wsp worklist size-check`: it
//! rolls out as `warn` (hits are logged as `size-check` events, with the
//! signal), and moves to `refuse` after Ed has read what it caught.

use crate::model::Task;

/// The heading the planner's reason for one whole seat sits under.
pub(crate) const WHOLE: &str = "Whole";

/// The file in the state directory that holds the mode.
pub(crate) const SETTING: &str = "size-check";

/// What a hit does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Warn,
    Refuse,
}

impl Mode {
    pub(crate) fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Mode::Off),
            "warn" => Some(Mode::Warn),
            "refuse" => Some(Mode::Refuse),
            _ => None,
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Warn => "warn",
            Mode::Refuse => "refuse",
        }
    }
}

/// The mode set: `warn` until somebody sets it, and when the file says something
/// that is not a mode.
pub(crate) fn mode(state: &std::path::Path) -> Mode {
    std::fs::read_to_string(state.join(SETTING)).ok().and_then(|s| Mode::parse(&s)).unwrap_or(Mode::Warn)
}

/// Which signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Observations,
    BuildThenDrive,
    Exception,
}

impl Signal {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Signal::Observations => "a",
            Signal::BuildThenDrive => "b",
            Signal::Exception => "c",
        }
    }
}

/// A signal, and what in the overview tripped it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub signal: Signal,
    pub what: String,
}

impl Hit {
    /// One line a planner can act on: the signal, what tripped it, and the way out.
    pub(crate) fn line(&self) -> String {
        let (name, route) = match self.signal {
            Signal::Observations => ("a done-when with three or more separate observations", "pieces when the observations are of separate parts, legs when they are of one built thing"),
            Signal::BuildThenDrive => ("a build or setup step over ~5 minutes, then a drive, run or measure", "legs: the build, then the drives"),
            Signal::Exception => ("the overview calls itself an exception to the small-row size", "legs or pieces"),
        };
        format!("({}) {name}: {} — {route}", self.signal.key(), self.what)
    }
}

/// The phrases of signal (c), matched without regard to case.
const EXCEPTION: &[&str] = &["exception to the small-row size", "exception to the small row size", "mostly the build", "mostly the shell's build"];

/// Words that start a drive, run or measure, for signal (b).
const DRIVE: &[&str] = &["drive", "measure", "benchmark", "then run", "and run", "then launch", "walkthrough"];

/// Words that name a build or setup step, for signal (b).
const BUILD: &[&str] = &["build", "setup", "set up", "install", "compile", "bootstrap"];

/// The signals a row's overview shows, in order a, b, c.
pub(crate) fn signals(overview: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    if let Some((n, text)) = observations(overview).filter(|(n, _)| *n >= 3) {
        hits.push(Hit { signal: Signal::Observations, what: format!("{n} observations in `{}`", clip(&text)) });
    }
    if let Some(what) = build_then_drive(overview) {
        hits.push(Hit { signal: Signal::BuildThenDrive, what });
    }
    let low = overview.to_ascii_lowercase();
    if let Some(p) = EXCEPTION.iter().find(|p| low.contains(*p)) {
        hits.push(Hit { signal: Signal::Exception, what: format!("\"{p}\"") });
    }
    hits
}

fn clip(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.char_indices().nth(70) {
        Some((i, _)) => format!("{}…", &one[..i]),
        None => one,
    }
}

/// The done-when's text and how many observations it states: from the words
/// "done when" to the blank line that ends the paragraph, and past it while the
/// lines that follow are items. `None` when the overview has no done-when.
fn observations(overview: &str) -> Option<(usize, String)> {
    let lines: Vec<&str> = overview.lines().collect();
    let at = lines.iter().position(|l| l.to_ascii_lowercase().contains("done when"))?;
    let low = lines[at].to_ascii_lowercase();
    let from = low.find("done when")? + "done when".len();
    let mut block = vec![lines[at].get(from..).unwrap_or_default().trim_start_matches([':', ' ', '\t']).to_string()];
    let mut bullets = 0usize;
    for l in &lines[at + 1..] {
        let t = l.trim();
        if t.is_empty() {
            break;
        }
        if is_item(t) {
            bullets += 1;
        }
        block.push(t.to_string());
    }
    let text = block.join(" ");
    let clauses = text.split(';').filter(|c| !c.trim().is_empty()).count();
    let marked = (1..=9).filter(|n| text.contains(&format!("({n})")) || text.contains(&format!(" {n}) ")) || text.starts_with(&format!("{n}) "))).count();
    Some((clauses.max(bullets).max(marked), text))
}

/// A bulleted or numbered line.
fn is_item(t: &str) -> bool {
    let rest = t.trim_start_matches(|c: char| c.is_ascii_digit());
    t.starts_with("- ") || t.starts_with("* ") || (rest.len() < t.len() && (rest.starts_with(". ") || rest.starts_with(") ")))
}

/// A build step over ~5 minutes with a drive, run or measure after it.
fn build_then_drive(overview: &str) -> Option<String> {
    let low = overview.to_ascii_lowercase();
    let mut at = 0usize;
    for sentence in low.split_inclusive(['.', '\n']) {
        let start = at;
        at += sentence.len();
        let Some(bm) = BUILD.iter().filter_map(|w| sentence.find(w)).min() else { continue };
        let long = ["release build", "cold build"].iter().find(|w| sentence.contains(*w)).map(|w| w.to_string());
        let minutes = minutes_in(sentence).filter(|m| *m > 5);
        let stated = match (&long, minutes) {
            (_, Some(m)) => format!("a build or setup of {m} min"),
            (Some(w), None) => format!("a {w}"),
            _ => continue,
        };
        // After the build word: the rest of this sentence and everything below.
        let after = &low[start + bm..];
        if let Some(d) = DRIVE.iter().filter_map(|w| after.find(w).map(|i| (i, *w))).min() {
            return Some(format!("{stated}, then `{}`", d.1));
        }
    }
    None
}

/// The most minutes a sentence states, "15 to 20 minutes" as 20, "an hour" as 60.
fn minutes_in(s: &str) -> Option<u32> {
    let mut nums: Vec<(u32, usize, usize)> = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let j = (i..b.len()).find(|k| !b[*k].is_ascii_digit()).unwrap_or(b.len());
            if let Ok(n) = s[i..j].parse() {
                nums.push((n, i, j));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let unit = |from: usize| -> bool {
        let t = s[from..].trim_start();
        t.starts_with("min") || t.starts_with("hour") || t.starts_with("hr")
    };
    let mut best: Option<u32> = None;
    for (k, (n, _, end)) in nums.iter().enumerate() {
        let tail = s[*end..].trim_start();
        let scale = |from: usize| if s[from..].trim_start().starts_with("min") { 1 } else { 60 };
        let (value, ok) = if unit(*end) {
            (*n * scale(*end), true)
        } else if tail.starts_with("to ") || tail.starts_with('-') || tail.starts_with('–') {
            match nums.get(k + 1).filter(|(_, st, _)| *st >= *end && *st - *end <= 4) {
                Some((m, _, e2)) if unit(*e2) => (*m * scale(*e2), true),
                _ => (0, false),
            }
        } else {
            (0, false)
        };
        if ok {
            best = Some(best.map_or(value, |b| b.max(value)));
        }
    }
    best
}

/// How a row passes: what it carries instead of one seat's whole work. `None`
/// when it carries none of them.
pub(crate) fn passes_by(t: &Task, siblings: &[Task]) -> Option<&'static str> {
    if crate::legs::staged(t) {
        Some("legs")
    } else if crate::pieces::staged(t) {
        Some("pieces")
    } else if crate::resolve::children_of(siblings, &t.id).iter().any(|k| k.status().is_open()) {
        Some("sub-tasks")
    } else if t.section(WHOLE).is_some_and(|s| !s.trim().is_empty()) {
        Some("whole")
    } else {
        None
    }
}

/// The signals a row trips and does not pass: empty for a row that is one seat's
/// work or that carries its way out.
pub(crate) fn trips(t: &Task, siblings: &[Task]) -> Vec<Hit> {
    let hits = signals(&t.section("Overview").unwrap_or_default());
    if hits.is_empty() || passes_by(t, siblings).is_some() {
        return Vec::new();
    }
    hits
}

/// The line a verifier's order gains on a row run whole against a signal.
pub(crate) fn whole_line(t: &Task) -> String {
    match t.section(WHOLE).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(why) => format!(
            "\n\n**This row is one seat's work by the planner's word** (the size check asked for legs, pieces or sub-tasks, and it was filed `--whole`): {why}"
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(overview: &str) -> Vec<&'static str> {
        signals(overview).iter().map(|h| h.signal.key()).collect()
    }

    #[test]
    fn three_observations_trip_a_and_two_do_not() {
        assert_eq!(keys("Done when: the rail places; the task page opens; a Board drop lands."), ["a"]);
        assert_eq!(keys("Done when: the rail places; the task page opens."), Vec::<&str>::new());
        assert_eq!(keys("Done when:\n- one\n- two\n- three\n\nElse."), ["a"]);
        assert_eq!(keys("Done when (1) it builds (2) it runs (3) it reads"), ["a"]);
        assert_eq!(keys("No stop clause here at all; one; two; three"), Vec::<&str>::new(), "a done-when is required");
    }

    #[test]
    fn a_long_build_then_a_drive_trips_b() {
        assert_eq!(keys("Build the shell, 15 to 20 minutes, then drive the rail."), ["b"]);
        assert_eq!(keys("The release build, then the drives of the task page."), ["b"]);
        assert_eq!(keys("Setup takes about 10 min.\nThen measure the cold start."), ["b"]);
        assert_eq!(keys("A build of 3 minutes, then a drive."), Vec::<&str>::new(), "five minutes or under");
        assert_eq!(keys("A build of 20 minutes, and nothing after it."), Vec::<&str>::new(), "no drive follows");
        assert_eq!(keys("Drive the page first. Then the build takes 20 minutes."), Vec::<&str>::new(), "the drive comes before");
    }

    #[test]
    fn the_planners_exception_wording_trips_c() {
        assert_eq!(keys("This is an exception to the small-row size: one seat."), ["c"]);
        assert_eq!(keys("It is mostly the build."), ["c"]);
        assert_eq!(keys("A small row."), Vec::<&str>::new());
    }

    #[test]
    fn each_hit_names_what_tripped() {
        let hits = signals("Exception to the small-row size, mostly the shell's build, 15 to 20 minutes. Drives after. Done when: a; b; c");
        assert_eq!(hits.iter().map(|h| h.signal.key()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert!(hits.iter().all(|h| h.line().contains(": ")), "{hits:?}");
    }

    fn row(id: &str, overview: &str) -> Task {
        let mut t = Task::new(id, id);
        crate::model::set_section_in(&mut t.body, "Overview", overview);
        t
    }

    #[test]
    fn a_row_passes_with_legs_pieces_sub_tasks_or_whole_and_not_with_nothing() {
        let big = "Exception to the small-row size.";
        let plain = row("m-1", big);
        assert_eq!(trips(&plain, &[plain.clone()]).len(), 1);

        let mut legs = row("m-1", big);
        crate::model::set_section_in(&mut legs.body, "Legs", "- build · claude sonnet medium · pending\n  order: build it");
        assert_eq!(passes_by(&legs, &[legs.clone()]), Some("legs"));

        let mut pieces = row("m-1", big);
        crate::model::set_section_in(&mut pieces.body, "Pieces", "- piece");
        assert_eq!(passes_by(&pieces, &[pieces.clone()]), Some("pieces"));

        let mut kid = Task::new("m-2", "kid");
        kid.parent = Some("m-1".into());
        assert_eq!(passes_by(&plain, &[plain.clone(), kid.clone()]), Some("sub-tasks"));
        kid.status_raw = "done".into();
        assert_eq!(passes_by(&plain, &[plain.clone(), kid]), None, "a finished sub-task is not a way through");

        let mut whole = row("m-1", big);
        crate::model::set_section_in(&mut whole.body, WHOLE, "one tree, one reader");
        assert_eq!(passes_by(&whole, &[whole.clone()]), Some("whole"));
        assert!(trips(&whole, &[whole.clone()]).is_empty());
        assert!(whole_line(&whole).contains("one tree, one reader"));
        assert!(whole_line(&plain).is_empty());
    }

    #[test]
    fn the_mode_is_a_setting_that_starts_at_warn() {
        let dir = std::env::temp_dir().join(format!("wsp-size-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(mode(&dir), Mode::Warn);
        std::fs::write(dir.join(SETTING), "refuse\n").unwrap();
        assert_eq!(mode(&dir), Mode::Refuse);
        std::fs::write(dir.join(SETTING), "nonsense").unwrap();
        assert_eq!(mode(&dir), Mode::Warn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
