//! `wsp budget` — what each project may spend in a month, for `wsp spend`.
//!
//! **Projects stand in for teams.** Compound's governance view draws budgets
//! "by team or project", and wsp has no teams: a budget is set on a project id
//! (`wsp project`), and that is the only owner money has here.
//!
//! Kept in wsp's state, machine-written, at `<state>/budgets.json`:
//! `{"standing": {"<project>": dollars}, "months": {"YYYY-MM": {"<project>": dollars}}}`.
//! A budget set without `--month` is standing — every month that has none of
//! its own reads it; one set with `--month` is that month's alone and wins over
//! the standing one. Dollars as a float, as `wsp spend --json` prints money.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::store::Store;
use crate::util::Paint;
use crate::Args;

pub const BUDGETS_FILE: &str = "budgets.json";

fn path(state: &Path) -> PathBuf {
    state.join(BUDGETS_FILE)
}

fn read(state: &Path) -> Value {
    std::fs::read_to_string(path(state))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "standing": {}, "months": {} }))
}

fn dollars_of(v: &Value) -> BTreeMap<String, f64> {
    v.as_object()
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_f64()?))).collect())
        .unwrap_or_default()
}

/// The budgets that hold in `month` (`YYYY-MM`): the standing ones, with the
/// month's own over them.
pub fn for_month(state: &Path, month: &str) -> BTreeMap<String, f64> {
    let all = read(state);
    let mut out = dollars_of(&all["standing"]);
    out.extend(dollars_of(&all["months"][month]));
    out
}

/// Set (or, with `None`, clear) one project's budget: standing when `month` is
/// `None`, else that month's.
pub fn set(state: &Path, project: &str, dollars: Option<f64>, month: Option<&str>) -> std::io::Result<()> {
    let mut all = read(state);
    let slot = match month {
        None => &mut all["standing"],
        Some(m) => &mut all["months"][m],
    };
    if !slot.is_object() {
        *slot = json!({});
    }
    let map = slot.as_object_mut().expect("an object");
    match dollars {
        Some(d) => {
            map.insert(project.to_string(), json!(d));
        }
        None => {
            map.remove(project);
        }
    }
    std::fs::create_dir_all(state)?;
    crate::store::write_atomic(&path(state), &format!("{}\n", serde_json::to_string_pretty(&all).unwrap_or_default()))
}

/// A `YYYY-MM` that is one, or the reason it is not.
pub fn month_arg(args: &Args) -> Result<Option<String>, String> {
    match args.get("month").filter(|m| m != "true") {
        None => Ok(None),
        Some(m) if crate::spend::Month::parse(&m).is_some() => Ok(Some(m)),
        Some(m) => Err(format!("--month takes YYYY-MM, not {m:?}")),
    }
}

pub fn budget(store: &Store, args: &Args) -> i32 {
    let month = match month_arg(args) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };
    if args.rest.first().map(String::as_str) == Some("set") {
        let (Some(project), Some(amount)) = (args.rest.get(1), args.rest.get(2)) else {
            eprintln!("wsp: budget set <project> <dollars> [--month YYYY-MM]   (`-` for dollars clears it)");
            return 2;
        };
        let dollars = match amount.trim_start_matches('$') {
            "-" => None,
            a => match a.parse::<f64>() {
                Ok(d) if d.is_finite() && d >= 0.0 => Some(d),
                _ => {
                    eprintln!("wsp: {amount:?} is not an amount of dollars");
                    return 2;
                }
            },
        };
        let known: Vec<String> = store.projects().into_iter().map(|p| p.id).collect();
        if !known.iter().any(|k| k == project) {
            eprintln!("wsp: no project {project:?} — a budget is set on a project id (teams do not exist in wsp; projects stand in for them): {}", known.join(", "));
            return 2;
        }
        if let Err(e) = set(&store.state, project, dollars, month.as_deref()) {
            eprintln!("wsp: could not write {}: {e}", path(&store.state).display());
            return 1;
        }
        let when = month.as_deref().map_or("standing, every month without its own".to_string(), |m| format!("for {m}"));
        match dollars {
            Some(d) => println!("{project}: ${d:.2} a month ({when})"),
            None => println!("{project}: budget cleared ({when})"),
        }
        return 0;
    }
    if let Some(other) = args.rest.first() {
        eprintln!("wsp: budget takes `set` or nothing, not {other:?}");
        return 2;
    }
    let month = month.unwrap_or_else(|| crate::spend::Month::now().label());
    let budgets = for_month(&store.state, &month);
    if args.json() {
        let total: Option<f64> = (!budgets.is_empty()).then(|| budgets.values().sum());
        println!("{}", json!({ "month": month, "currency": "USD", "budget": total, "projects": budgets.iter().map(|(p, d)| json!({ "id": p, "budget": d })).collect::<Vec<_>>() }));
        return 0;
    }
    let p = Paint::new();
    if budgets.is_empty() {
        println!("{}", p.dim(&format!("no budget set for {month} — wsp budget set <project> <dollars>")));
        return 0;
    }
    for (proj, d) in &budgets {
        println!("{:<24} ${d:.2}", proj);
    }
    println!("{}", p.dim(&format!("{month} · {} total · projects stand in for teams", format_args!("${:.2}", budgets.values().sum::<f64>()))));
    0
}

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["budget"],
    run: budget,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Plumbing,
        order: 113,
        text: r##"  wsp budget [set <project> <dollars>] [--month YYYY-MM] [--json]
                                    a project's monthly budget, for wsp spend:
                                    standing unless --month, `-` clears it;
                                    projects stand in for teams, which wsp has
                                    none of"##,
    }],
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_set_reads_back_and_a_months_own_wins_over_the_standing_one() {
        let env = crate::util::isolated("budget-round");
        let state = env.state();
        set(&state, "compounding", Some(120.0), None).unwrap();
        set(&state, "wsp", Some(40.5), None).unwrap();
        set(&state, "compounding", Some(200.0), Some("2026-11")).unwrap();
        let nov = for_month(&state, "2026-11");
        assert_eq!(nov.get("compounding"), Some(&200.0), "{nov:?}");
        assert_eq!(nov.get("wsp"), Some(&40.5), "standing carries into a month without its own");
        assert_eq!(for_month(&state, "2026-12").get("compounding"), Some(&120.0));
        set(&state, "wsp", None, None).unwrap();
        assert_eq!(for_month(&state, "2026-12").get("wsp"), None, "`-` clears it");
    }

    #[test]
    fn budget_set_refuses_a_project_that_does_not_exist() {
        let env = crate::util::isolated("budget-unknown");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let args = crate::Args::parse(["budget", "set", "nope", "10"].iter().map(|s| s.to_string()).collect());
        assert_eq!(budget(&store, &args), 2);
        assert!(for_month(&store.state, "2026-11").is_empty());
    }
}
