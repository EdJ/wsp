//! `wsp verifications` — what verifiers did, as a running number: `wsp-325`.
//!
//! The lines a verdict carries ([`crate::did`]) answer, for one pass, whether it
//! ran the tests, drove the app, read the diff and checked each done-when item.
//! This adds the passes up by project and period, so the question that took a
//! hand analysis of 405 transcripts is a command.
//!
//! **Shares are of the passes whose transcript was read**, and the report says
//! how many that is next to the number of verdicts. A pass written before the
//! lines, or on a kind whose record is not read, is *unrecorded*: counting it
//! as one that ran nothing would make the share fall the day the feature
//! shipped. `checked` is the exception, because it is the verifier's own line
//! and not the transcript's, so its share is of every verdict in the bucket.
//!
//! **A block's cause is the first of these that applies**, read from its own
//! lines and nothing else: a done-when item it says did not hold; a test run
//! whose last word was failed; some other command that failed; a block that
//! failed nothing it ran, which is a verifier that found it by reading; and a
//! block with no lines to say.

use std::collections::BTreeMap;

use serde_json::json;

use crate::did::Did;
use crate::model::Task;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::verification::{self, Pass, State};
use crate::Args;

const USAGE: &str = "wsp verifications [<project>] [--since DATE|Nd] [--until DATE] [--by project|week|month|project,week|project,month] [--json]\n       wsp verifications --transcript FILE   the lines a verifier's transcript would give";

/// The causes of a block, in the order they are tried.
const CAUSES: [&str; 5] = ["done-when not held", "test failed", "command failed", "found by reading", "unrecorded"];

pub fn verifications(store: &Store, args: &Args) -> i32 {
    if let Some(file) = args.get("transcript") {
        return explain(&file);
    }
    let since = match when(args, "since") {
        Ok(s) => s,
        Err(code) => return code,
    };
    let until = match when(args, "until") {
        Ok(s) => s,
        Err(code) => return code,
    };
    let by = args.get("by").unwrap_or_else(|| "project".into());
    let Some(keys) = grouping(&by) else {
        eprintln!("wsp: --by is project, week, month, project,week or project,month — not `{by}`\nusage: {USAGE}");
        return 2;
    };
    let wanted = args.rest.first().cloned();

    let index = crate::resolve::Index::new(store.projects());
    let allowed: Option<Vec<String>> = match &wanted {
        Some(p) => match index.find(p) {
            Some(found) => Some(index.subtree(&found.id)),
            None => {
                eprintln!("wsp: no project {p}");
                return 2;
            }
        },
        None => None,
    };

    let mut tasks: Vec<Task> = store.tasks();
    tasks.extend(store.archived_tasks());
    let mut rows: Vec<Row> = Vec::new();
    for t in &tasks {
        let project = project_of(t);
        if allowed.as_ref().is_some_and(|a| !a.contains(&project)) {
            continue;
        }
        for p in verification::passes(t) {
            if !matches!(p.state, State::Holds | State::Blocks) {
                continue;
            }
            let at = p.decided.clone().unwrap_or_else(|| p.at.clone());
            let when = util::epoch_of(&at);
            if since.is_some_and(|s| when < s) || until.is_some_and(|u| when >= u) {
                continue;
            }
            rows.push(Row { project: project.clone(), at, pass: p });
        }
    }

    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    for r in &rows {
        buckets.entry(key_of(r, &keys)).or_default().add(&r.pass);
    }

    if args.json() {
        let out: Vec<_> = buckets.iter().map(|(k, b)| b.json(k)).collect();
        println!("{}", json!({ "by": by, "since": since.map(util::iso_at), "until": until.map(util::iso_at), "buckets": out }));
        return 0;
    }
    if buckets.is_empty() {
        println!("no verdicts{}", if since.is_some() || until.is_some() || wanted.is_some() { " in that window" } else { "" });
        return 0;
    }
    print!("{}", render(&buckets, &by, since, until, wanted.as_deref()));
    0
}

struct Row {
    project: String,
    at: String,
    pass: Pass,
}

fn project_of(t: &Task) -> String {
    t.project.clone().filter(|p| !p.is_empty()).or_else(|| t.id.rsplit_once('-').map(|(p, _)| p.to_string())).unwrap_or_default()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Project,
    Week,
    Month,
}

fn grouping(by: &str) -> Option<Vec<Key>> {
    by.split(',')
        .map(|k| match k.trim() {
            "project" => Some(Key::Project),
            "week" => Some(Key::Week),
            "month" => Some(Key::Month),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .filter(|k| !k.is_empty() && k.len() <= 2)
}

fn key_of(r: &Row, keys: &[Key]) -> String {
    keys.iter()
        .map(|k| match k {
            Key::Project => r.project.clone(),
            Key::Week => week_of(&r.at),
            Key::Month => r.at.chars().take(7).collect(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The Monday a stamp's week begins on.
fn week_of(iso: &str) -> String {
    let day = util::epoch_of(iso).div_euclid(86_400);
    // 1970-01-01 was a Thursday.
    let monday = day - (day + 3).rem_euclid(7);
    util::iso_at(monday * 86_400).chars().take(10).collect()
}

/// `2026-10-01` or `30d` or `4w`, as the instant it names.
fn when(args: &Args, name: &str) -> Result<Option<i64>, i32> {
    let Some(v) = args.get(name) else { return Ok(None) };
    let v = v.trim();
    let ago = |n: &str, secs: i64| n.parse::<i64>().ok().map(|n| util::epoch_secs() - n * secs);
    let at = match v.chars().last() {
        Some('d') => ago(&v[..v.len() - 1], 86_400),
        Some('w') => ago(&v[..v.len() - 1], 7 * 86_400),
        _ if v.len() == 10 && v.as_bytes()[4] == b'-' && v.as_bytes()[7] == b'-' => Some(util::epoch_of(v)),
        _ => None,
    };
    match at {
        Some(t) if t > 0 => Ok(Some(t)),
        _ => {
            eprintln!("wsp: --{name} is a date (2026-10-01) or an age (30d, 4w) — not `{v}`\nusage: {USAGE}");
            Err(2)
        }
    }
}

#[derive(Default)]
struct Bucket {
    verdicts: usize,
    holds: usize,
    blocks: usize,
    /// Passes whose transcript was read.
    recorded: usize,
    ran_tests: usize,
    tests_passed: usize,
    drove: usize,
    read_diff: usize,
    with_checked: usize,
    held: usize,
    not_held: usize,
    causes: BTreeMap<&'static str, usize>,
}

impl Bucket {
    fn add(&mut self, p: &Pass) {
        self.verdicts += 1;
        match p.state {
            State::Holds => self.holds += 1,
            _ => self.blocks += 1,
        }
        if let Some(d) = &p.did {
            self.recorded += 1;
            let tests = d.tests();
            if !tests.is_empty() {
                self.ran_tests += 1;
                if tests.iter().all(|c| c.last() == crate::did::Outcome::Passed) {
                    self.tests_passed += 1;
                }
            }
            self.drove += usize::from(d.drove());
            self.read_diff += usize::from(d.read.diff_count() > 0);
        }
        if !p.checked.is_empty() {
            self.with_checked += 1;
        }
        self.held += p.checked.iter().filter(|c| c.held).count();
        self.not_held += p.checked.iter().filter(|c| !c.held).count();
        if p.state == State::Blocks {
            *self.causes.entry(cause_of(p)).or_default() += 1;
        }
    }

    fn json(&self, key: &str) -> serde_json::Value {
        json!({
            "key": key, "verdicts": self.verdicts, "holds": self.holds, "blocks": self.blocks,
            "recorded": self.recorded, "ran_tests": self.ran_tests, "tests_passed": self.tests_passed,
            "drove": self.drove, "read_diff": self.read_diff,
            "with_checked": self.with_checked, "held": self.held, "not_held": self.not_held,
            "blocks_by_cause": self.causes,
        })
    }
}

/// Why a block blocked, as far as its own lines say.
pub(crate) fn cause_of(p: &Pass) -> &'static str {
    if p.checked.iter().any(|c| !c.held) {
        return CAUSES[0];
    }
    let Some(d) = &p.did else { return CAUSES[4] };
    let failed = |c: &crate::did::Command| c.last() == crate::did::Outcome::Failed;
    if d.ran.iter().filter(|c| c.is_test()).any(failed) {
        CAUSES[1]
    } else if d.ran.iter().any(failed) {
        CAUSES[2]
    } else {
        CAUSES[3]
    }
}

fn share(n: usize, of: usize) -> String {
    if of == 0 {
        return "–".into();
    }
    format!("{}%", (n * 100 + of / 2) / of)
}

fn render(buckets: &BTreeMap<String, Bucket>, by: &str, since: Option<i64>, until: Option<i64>, project: Option<&str>) -> String {
    let p = Paint::new();
    let mut head = format!("verifications by {by}");
    if let Some(project) = project {
        head.push_str(&format!(" · {project}"));
    }
    if let Some(s) = since {
        head.push_str(&format!(" · since {}", &util::iso_at(s)[..10]));
    }
    if let Some(u) = until {
        head.push_str(&format!(" · until {}", &util::iso_at(u)[..10]));
    }
    let width = buckets.keys().map(|k| k.chars().count()).max().unwrap_or(0).max(6);
    let mut out = format!("{}\n\n", p.bold(&head));
    out.push_str(&p.dim(&format!(
        "{}  {:>8}  {:>5}  {:>6}  {:>8}  {:>9}  {:>6}  {:>9}  {:>8}\n",
        util::pad("", width), "verdicts", "holds", "blocks", "recorded", "ran tests", "drove", "read diff", "checked"
    )));
    let mut all = Bucket::default();
    for (k, b) in buckets {
        out.push_str(&row(&util::pad(k, width), b));
        all.merge(b);
    }
    if buckets.len() > 1 {
        out.push_str(&row(&util::pad("all", width), &all));
    }
    out.push_str(&p.dim("\nran tests, drove and read diff are shares of the recorded passes; checked is of every verdict\n"));
    let tests: usize = all.tests_passed;
    if all.ran_tests > 0 {
        out.push_str(&p.dim(&format!("{} of the {} that ran tests had every test run end passed\n", tests, all.ran_tests)));
    }
    if all.held + all.not_held > 0 {
        out.push_str(&p.dim(&format!("{} done-when items checked: {} held, {} not\n", all.held + all.not_held, all.held, all.not_held)));
    }
    if all.blocks > 0 {
        out.push_str(&format!("\n{}\n", p.bold("blocks by cause")));
        for (k, b) in buckets {
            if b.blocks > 0 {
                out.push_str(&format!("  {}  {}\n", util::pad(k, width), causes_text(&b.causes)));
            }
        }
        if buckets.len() > 1 {
            out.push_str(&format!("  {}  {}\n", util::pad("all", width), causes_text(&all.causes)));
        }
    }
    out
}

fn row(name: &str, b: &Bucket) -> String {
    format!(
        "{name}  {:>8}  {:>5}  {:>6}  {:>8}  {:>9}  {:>6}  {:>9}  {:>8}\n",
        b.verdicts,
        b.holds,
        b.blocks,
        b.recorded,
        share(b.ran_tests, b.recorded),
        share(b.drove, b.recorded),
        share(b.read_diff, b.recorded),
        share(b.with_checked, b.verdicts),
    )
}

fn causes_text(c: &BTreeMap<&'static str, usize>) -> String {
    CAUSES.iter().filter_map(|k| c.get(k).map(|n| format!("{k} {n}"))).collect::<Vec<_>>().join(" · ")
}

impl Bucket {
    fn merge(&mut self, o: &Bucket) {
        self.verdicts += o.verdicts;
        self.holds += o.holds;
        self.blocks += o.blocks;
        self.recorded += o.recorded;
        self.ran_tests += o.ran_tests;
        self.tests_passed += o.tests_passed;
        self.drove += o.drove;
        self.read_diff += o.read_diff;
        self.with_checked += o.with_checked;
        self.held += o.held;
        self.not_held += o.not_held;
        for (k, n) in &o.causes {
            *self.causes.entry(k).or_default() += n;
        }
    }
}

/// `--transcript FILE`: the lines that transcript would give an entry — the
/// check on the derivation, run on any session.
fn explain(file: &str) -> i32 {
    let Ok(f) = std::fs::File::open(file) else {
        eprintln!("wsp: cannot read {file}");
        return 1;
    };
    match crate::did::of_transcript(&mut std::io::BufReader::new(f), 0) {
        Some(d) => {
            print_lines(&d);
            0
        }
        None => {
            eprintln!("wsp: {file} has no assistant turn to read");
            1
        }
    }
}

fn print_lines(d: &Did) {
    for l in d.lines() {
        println!("{l}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::{Checked, Command, Outcome};

    fn command(text: &str, o: &[Outcome]) -> Command {
        Command { text: text.into(), outcomes: o.to_vec() }
    }

    fn pass(state: State, did: Option<Did>, checked: Vec<Checked>) -> Pass {
        Pass { state, did, checked, ..Pass::opened(None, None) }
    }

    fn did_with(ran: Vec<Command>, drove: &[&str], diffs: &[&str]) -> Did {
        let mut d = Did { ran, ..Did::default() };
        d.drove = drove.iter().map(|s| s.to_string()).collect();
        d.read.diffs = diffs.iter().map(|s| s.to_string()).collect();
        d
    }

    /// A block's cause is read from its own lines, in a fixed order: what the
    /// verifier says did not hold outranks a failed test, which outranks any
    /// failed command, which outranks nothing failing at all.
    #[test]
    fn a_blocks_cause_is_the_first_thing_its_own_lines_say() {
        let failed_test = did_with(vec![command("cargo test a::", &[Outcome::Failed])], &[], &[]);
        let flaky = did_with(vec![command("cargo test a::", &[Outcome::Failed, Outcome::Passed])], &[], &[]);
        let failed_script = did_with(vec![command("python3 s.py", &[Outcome::Failed])], &[], &[]);
        let nothing = did_with(vec![command("cargo test a::", &[Outcome::Passed])], &[], &[]);
        let not = Checked { item: "x".into(), held: false };
        let yes = Checked { item: "x".into(), held: true };
        let b = |d: Option<Did>, c: Vec<Checked>| cause_of(&pass(State::Blocks, d, c));
        assert_eq!(b(Some(failed_test.clone()), vec![not.clone()]), "done-when not held");
        assert_eq!(b(Some(failed_test), vec![yes.clone()]), "test failed");
        assert_eq!(b(Some(failed_script), vec![]), "command failed");
        assert_eq!(b(Some(flaky), vec![]), "found by reading", "a test that failed and then passed did not fail");
        assert_eq!(b(Some(nothing), vec![yes]), "found by reading");
        assert_eq!(b(None, vec![]), "unrecorded");
        assert_eq!(b(None, vec![not]), "done-when not held", "the verifier's own line needs no transcript");
    }

    /// Shares are of the passes whose transcript was read: an unrecorded one
    /// adds to the verdicts and to nothing else, except `checked`, which is the
    /// verifier's own and is of every verdict.
    #[test]
    fn shares_are_of_the_recorded_passes_and_checked_is_of_every_verdict() {
        let mut b = Bucket::default();
        b.add(&pass(State::Holds, Some(did_with(vec![command("cargo test a::", &[Outcome::Passed])], &[], &["A..B"])), vec![]));
        b.add(&pass(State::Blocks, Some(did_with(vec![command("wsp check m-1", &[Outcome::Failed])], &["ctl · 2 commands"], &[])), vec![]));
        b.add(&pass(State::Holds, Some(did_with(vec![], &[], &[])), vec![Checked { item: "a".into(), held: true }]));
        b.add(&pass(State::Holds, None, vec![]));
        assert_eq!((b.verdicts, b.holds, b.blocks, b.recorded), (4, 3, 1, 3));
        assert_eq!((b.ran_tests, b.tests_passed, b.drove, b.read_diff, b.with_checked), (2, 1, 1, 1, 1));
        let r = row("p", &b);
        assert!(r.contains("67%") && r.contains("33%") && r.contains("25%"), "{r}");
        assert_eq!(b.causes.get("test failed"), Some(&1));
        assert_eq!(share(0, 0), "–");
    }

    #[test]
    fn periods_name_the_week_by_its_monday_and_the_month_by_its_first_seven() {
        assert_eq!(week_of("2026-11-01T10:00:00Z"), "2026-10-26", "a Sunday belongs to the week before");
        assert_eq!(week_of("2026-11-02T00:00:00Z"), "2026-11-02");
        assert_eq!(week_of("1970-01-01T00:00:00Z"), "1969-12-29");
        assert_eq!(grouping("project,week").map(|k| k.len()), Some(2));
        assert!(grouping("day").is_none() && grouping("week,week,week").is_none());
    }
}
