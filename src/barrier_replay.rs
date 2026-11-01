//! `wsp worklist replay` — what an automatic pass would have done to the barriers
//! already recorded: `wsp-326`.
//!
//! The rule is [`crate::barrier_run::decide`]'s: pass without an agent when every
//! member's verdict held, every check is green and the stop has no prose. A
//! barrier already recorded cannot be re-run — no check was declared when most
//! of them were read — so this reads what the record does hold: the stop's
//! prose, the members' newest verdicts, and the sentence each hold or follow-up
//! gave as its reason. It never writes.
//!
//! A hold is **mechanical** when its reason is a build, a test, a golden or a
//! gate going red — the kind a declared check can see. A hold whose stop had
//! prose could not have passed without an agent. One that is neither would have
//! **slipped**: the rule passes it, and no check could have caught it. The count
//! of those is what decides whether the automatic pass may be turned on.

use crate::barrier::{Check, State};
use crate::model::{Group, Worklist};
use crate::store::Store;
use crate::Args;

/// What the rule would have done to one barrier that held, or passed with
/// follow-ups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fate {
    /// The stop had prose, so an agent reads it whatever else is true.
    AgentReads,
    /// The reason is a failing build or test; a declared check would have seen
    /// it, and the pass would have waited for a green one.
    CaughtIfDeclared,
    /// Nothing the rule reads would have stopped it.
    Slipped,
}

#[derive(Debug, Clone)]
pub(crate) struct Case {
    pub list: String,
    pub group: usize,
    pub state: State,
    pub fate: Fate,
    pub reason: String,
}

const MECHANICAL: [&str; 14] = [
    "red", "fail", "compile", "does not build", "doesn't build", "build", "golden", "gate", "clippy", "lint", "suite", "test", "panic", "regress",
];

/// Whether a reason reads as a check going red.
pub(crate) fn mechanical(reason: &str) -> bool {
    let r = reason.to_lowercase();
    MECHANICAL.iter().any(|k| r.contains(k))
}

/// The log lines of a list that gave a hold or a follow-up, with their instants.
fn reasons(w: &Worklist) -> Vec<(String, String)> {
    let log = w.section("Log").unwrap_or_default();
    log.lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("- ")?;
            let (at, rest) = l.split_once(' ')?;
            let held = rest.starts_with("held") || rest.contains("follow-up by") && (rest.contains(" holds") || rest.contains(" passes"));
            held.then(|| (at.to_string(), rest.to_string()))
        })
        .collect()
}

/// The cases on one group: each entry that held or found follow-ups, with the
/// reason given after it opened and before the next entry did.
pub(crate) fn cases(w: &Worklist, at: usize, g: &Group, reasons: &[(String, String)]) -> Vec<Case> {
    let mut out = Vec::new();
    for (i, c) in g.checks.iter().enumerate() {
        if !matches!(c.state, State::Holds | State::Blocking | State::Next) {
            continue;
        }
        let end = g.checks.get(i + 1).map(|n: &Check| n.at.clone()).unwrap_or_else(|| "9999".into());
        let reason = reasons
            .iter()
            .find(|(t, _)| t.as_str() >= c.at.as_str() && t.as_str() < end.as_str())
            .map(|(_, r)| r.clone())
            .unwrap_or_default();
        let fate = if !g.stop.trim().is_empty() {
            Fate::AgentReads
        } else if mechanical(&reason) {
            Fate::CaughtIfDeclared
        } else {
            Fate::Slipped
        };
        out.push(Case { list: w.id.clone(), group: at, state: c.state, fate, reason });
    }
    out
}

pub(crate) struct Replay {
    pub lists: usize,
    pub barriers: usize,
    pub passed: usize,
    pub cases: Vec<Case>,
    /// Passes whose stop had no prose: what an automatic pass would have taken
    /// off an agent's hands.
    pub passed_without_prose: usize,
}

pub(crate) fn replay(lists: &[Worklist]) -> Replay {
    let mut r = Replay { lists: 0, barriers: 0, passed: 0, cases: Vec::new(), passed_without_prose: 0 };
    for w in lists {
        let groups = w.groups();
        if groups.iter().all(|g| g.checks.is_empty()) {
            continue;
        }
        r.lists += 1;
        let why = reasons(w);
        for (i, g) in groups.iter().enumerate() {
            if g.checks.is_empty() {
                continue;
            }
            r.barriers += 1;
            if g.checks.iter().any(|c| c.state == State::Passes) {
                r.passed += 1;
                if g.stop.trim().is_empty() {
                    r.passed_without_prose += 1;
                }
            }
            r.cases.extend(cases(w, i + 1, g, &why));
        }
    }
    r
}

pub fn run(store: &Store, args: &Args) -> i32 {
    let mut lists = store.worklists();
    let named: Vec<&String> = args.rest.iter().skip(1).collect();
    if !named.is_empty() {
        lists.retain(|w| named.iter().any(|n| **n == w.id));
    }
    let r = replay(&lists);
    let count = |f: &dyn Fn(&Case) -> bool| r.cases.iter().filter(|c| f(c)).count();
    let held = count(&|c| matches!(c.state, State::Holds | State::Blocking));
    let next = count(&|c| c.state == State::Next);
    println!("barrier replay — {} lists, {} barriers read, {} passed, {} held, {} passed with follow-ups", r.lists, r.barriers, r.passed, held, next);
    println!("  passed with no stop prose: {} — what an automatic pass would take from an agent", r.passed_without_prose);
    for (title, pick) in [
        ("held, stop has prose (an agent reads it regardless)", Fate::AgentReads),
        ("held for a red build or test (a declared check sees it)", Fate::CaughtIfDeclared),
        ("held for something else (the rule would have passed it)", Fate::Slipped),
    ] {
        let mine: Vec<&Case> = r.cases.iter().filter(|c| c.fate == pick && c.state != State::Next).collect();
        println!("\n{title}: {}", mine.len());
        for c in mine {
            println!("  {} group {} · {}", c.list, c.group, crate::util::truncate(&c.reason, 150));
        }
    }
    let nexts: Vec<&Case> = r.cases.iter().filter(|c| c.state == State::Next).collect();
    println!("\npassed with follow-ups, which an automatic pass would not have found: {}", nexts.len());
    for c in nexts {
        if c.fate == Fate::AgentReads {
            println!("  {} group {} · stop had prose", c.list, c.group);
        } else {
            println!("  {} group {} · stop had none · {}", c.list, c.group, crate::util::truncate(&c.reason, 260));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_red_build_or_test_is_mechanical_and_a_design_objection_is_not() {
        assert!(mechanical("Group 8 holds on one failing test; all six members are sound"));
        assert!(mechanical("holds, on one red gate test"));
        assert!(!mechanical("Holds, pending four small rows: the docs name the wrong crate"));
        assert!(!mechanical("the two members disagree about who owns the config"));
    }

    fn list(stop: &str, log: &str) -> Worklist {
        let mut g = Group { members: vec!["a-1".into()], stop: stop.into(), ..Default::default() };
        g.checks = vec![Check { at: "2026-10-01T10:00:00Z".into(), state: State::Holds, ..Check::opened(None, None) }];
        let mut w = Worklist::new("run", "a run");
        w.set_groups(&[g]);
        w.body = format!("{}\n## Log\n{log}\n", w.body);
        w
    }

    #[test]
    fn a_hold_is_sorted_by_its_stop_and_by_what_it_said() {
        let held = |stop: &str, why: &str| {
            let w = list(stop, &format!("- 2026-10-01T11:00:00Z held — {why}"));
            let g = w.groups().remove(0);
            cases(&w, 1, &g, &reasons(&w)).remove(0).fate
        };
        assert_eq!(held("flag Ed if cost doubles", "one red test"), Fate::AgentReads);
        assert_eq!(held("", "one red test"), Fate::CaughtIfDeclared);
        assert_eq!(held("", "the docs claim a thing the code does not do"), Fate::Slipped);
    }
}
