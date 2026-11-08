//! `wsp piece add | show | drop | cap`: the verbs over the record in [`crate::pieces`].
//!
//! The record, its rules and its writes are `pieces.rs` (engine). The words
//! that read an `Args`, find the seat they are run in and print are here, in
//! the top layer, the way the call-start glue is in `main` (wsp-481): the
//! engine module keeps no edge up to `main`.

use serde_json::{json, Value};

use crate::model::{Status, Task};
use crate::pieces::{
    add_all, at_of, drop_piece, json, parse_blocks, record, refuses_pieces, set_cap, table, write, Actor, New, Piece,
    Record, MAX_PIECES,
};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// The verbs this module answers to, each with the help it carries.
pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["piece"],
    run: dispatch,
    help: &[
        crate::verbs::Help {
            section: crate::verbs::Section::Tasks,
            order: 151,
            text: r##"  wsp piece add <task> "title" --batch N|next --overview FILE|- --done-when "…" [--check "label (time): cmd"]… [--agent "claude opus high" --why "…"]
                                    a piece of a row (docs/pieces.md): under
                                    ## Pieces, in a batch not yet open. No --agent
                                    is claude sonnet medium; another tier needs --why"##,
        },
        crate::verbs::Help {
            section: crate::verbs::Section::Tasks,
            order: 152,
            text: r##"  wsp piece add <task> --from FILE       several at once, in one commit: a `### title` block
                                         each, with batch:, done-when: and check: lines"##,
        },
        crate::verbs::Help {
            section: crate::verbs::Section::Tasks,
            order: 153,
            text: r##"  wsp piece show <task> [<n>]            the pieces, or one piece whole"##,
        },
        crate::verbs::Help {
            section: crate::verbs::Section::Tasks,
            order: 154,
            text: r##"  wsp piece drop <task> <n> "why"        a pending piece, never run"##,
        },
        crate::verbs::Help {
            section: crate::verbs::Section::Tasks,
            order: 155,
            text: r##"  wsp piece cap <task> <N>               how many of the row's pieces run at once (default 3)"##,
        },
    ],
}];

const USAGE: &str = "usage:
  wsp piece add <task> \"title\" --batch N|next --overview FILE|- --done-when \"…\" [--check \"label (time): cmd\"]… [--agent \"claude opus high\" --why \"…\"]
  wsp piece add <task> --from FILE       several at once: a `### title` block each, with batch:, done-when:, check: lines
  wsp piece show <task> [<n>]            the table, or one piece whole
  wsp piece drop <task> <n> \"why\"        a pending piece, never run
  wsp piece cap <task> <N>               how many of the row's pieces run at once";

pub fn dispatch(store: &Store, args: &Args) -> i32 {
    let me = crate::place::my_pane();
    run(store, args, me.as_deref())
}

/// [`dispatch`] with the pane given, so a test is not asked where it is standing.
pub(crate) fn run(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    match args.rest.first().map(String::as_str) {
        Some("add") => cmd_add(store, args, pane),
        Some("drop") => cmd_drop(store, args, pane),
        Some("cap") => cmd_cap(store, args, pane),
        Some("show") | Some("ls") | None => cmd_show(store, args),
        Some(other) => {
            eprintln!("wsp piece: unknown subcommand `{other}`\n{USAGE}");
            2
        }
    }
}

/// Who `pane` is, for this row.
fn actor_here(store: &Store, t: &Task, pane: Option<&str>) -> Actor {
    let Some(p) = pane else { return Actor::Person };
    if let Some(b) = store.bindings().get(p) {
        if b.get("task_id").and_then(Value::as_str) == Some(t.id.as_str()) {
            if let Some(n) = b.get("piece").and_then(Value::as_u64) {
                return Actor::Piece(n as usize);
            }
            if b.get("leg").is_none() {
                return Actor::Claimant;
            }
        }
    }
    if let Some(scope) = crate::place::governs(&store.governors(), &crate::place::Seat::new(p)) {
        return if crate::governors::is_primary(&scope) { Actor::Primary } else { Actor::Governor };
    }
    if crate::cmd_role::bound_here(store, pane) {
        Actor::Other
    } else {
        Actor::Person
    }
}

/// Apply one edit to a row's pieces under the store lock, so two seats writing
/// at once each see the other's result.
fn edit(
    store: &Store,
    needle: &str,
    pane: Option<&str>,
    f: impl FnOnce(&Record, &Actor, Status) -> Result<Record, String>,
) -> Result<(Task, Record), String> {
    let id = store.task_or_why(needle)?.id;
    store.locked(|| {
        let mut t = store.find_task(&id).ok_or_else(|| format!("no task {id}"))?;
        if let Some(why) = refuses_pieces(&t) {
            return Err(why);
        }
        let old = record(&t);
        let actor = actor_here(store, &t, pane);
        let new = f(&old, &actor, t.status())?;
        write(&mut t, &new);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok((t, new))
    })
}

fn refuse(why: String) -> i32 {
    eprintln!("wsp: {}", why.trim_start_matches("wsp: "));
    1
}

/// The overview: `--overview FILE`, or `-` for stdin. `-` after a long flag is a
/// positional (a `-`-led word is never a value), so the flag reads as bare.
fn overview_text(args: &Args) -> Result<String, String> {
    let src = match args.get("overview") {
        Some(v) if v == "true" && args.rest.iter().skip(3).any(|r| r == "-") => "-".to_string(),
        Some(v) if v != "true" => v,
        _ => return Err("a piece needs an overview: --overview FILE, or --overview - to read it from stdin".into()),
    };
    if src == "-" && util::stdin_is_tty() {
        return Err("nothing is piped in — `--overview -` reads the overview from a stream".into());
    }
    util::read_source(&src).map_err(|e| format!("cannot read {src}: {e}"))
}

fn cmd_add(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let Some(task) = args.rest.get(1) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let news = if let Some(file) = args.get("from").filter(|f| f != "true") {
        match util::read_source(&file).map_err(|e| format!("cannot read {file}: {e}")).and_then(|t| parse_blocks(&t)) {
            Ok(n) => n,
            Err(why) => return refuse(why),
        }
    } else {
        let Some(title) = args.rest.get(2).filter(|t| t.as_str() != "-") else {
            eprintln!("{USAGE}");
            return 2;
        };
        let at = match args.get("batch") {
            Some(b) if b != "true" => match at_of(&b) {
                Ok(a) => a,
                Err(why) => return refuse(why),
            },
            _ => return refuse("a piece needs a batch: `--batch N`, or `--batch next` for a new one after the last".into()),
        };
        let overview = match overview_text(args) {
            Ok(o) => o,
            Err(why) => return refuse(why),
        };
        let Some(done_when) = args.get("done-when").filter(|d| d != "true") else {
            return refuse("a piece needs a done-when: `--done-when \"…\"`".into());
        };
        vec![New {
            title: title.clone(),
            at,
            overview,
            done_when,
            checks: args.all("check"),
            agent: args.get("agent"),
            why: args.get("why"),
        }]
    };
    let count = news.len();
    let before = std::cell::Cell::new(0);
    match edit(store, task, pane, |old, actor, status| {
        before.set(old.pieces.len());
        add_all(old, news, actor, status)
    }) {
        Ok((t, rec)) => {
            let added: Vec<&Piece> = rec.pieces.iter().skip(before.get()).collect();
            for p in &added {
                store.log_event("piece-added", json!({ "id": t.id, "piece": p.n }));
            }
            let nums = added.iter().map(|p| p.n.to_string()).collect::<Vec<_>>().join(", ");
            store.git_commit(&format!("wsp: {} piece{} {nums} added", t.id, if count == 1 { "" } else { "s" }));
            for p in &added {
                println!(
                    "{} {}  piece {} · batch {} · {}  ({} of {MAX_PIECES})",
                    Paint::new().green("✓"),
                    t.id,
                    p.n,
                    p.batch,
                    util::truncate(&util::fold(&p.title), 50),
                    rec.pieces.len()
                );
            }
            0
        }
        Err(why) => refuse(why),
    }
}

fn cmd_drop(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let (Some(task), Some(n)) = (args.rest.get(1), args.rest.get(2).and_then(|n| n.parse::<usize>().ok())) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let why = args.text(3);
    match edit(store, task, pane, |old, actor, status| drop_piece(old, n, &why, actor, status)) {
        Ok((t, _)) => {
            store.git_commit(&format!("wsp: {} piece {n} dropped", t.id));
            println!("{} {}  piece {n} dropped", Paint::new().green("✓"), t.id);
            0
        }
        Err(why) => refuse(why),
    }
}

fn cmd_cap(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let (Some(task), Some(cap)) = (args.rest.get(1), args.rest.get(2).and_then(|n| n.parse::<usize>().ok())) else {
        eprintln!("{USAGE}");
        return 2;
    };
    match edit(store, task, pane, |old, actor, status| set_cap(old, cap, actor, status)) {
        Ok((t, _)) => {
            store.git_commit(&format!("wsp: {} pieces cap {cap}", t.id));
            println!("{} {}  cap {cap}", Paint::new().green("✓"), t.id);
            0
        }
        Err(why) => refuse(why),
    }
}

fn cmd_show(store: &Store, args: &Args) -> i32 {
    let Some(task) = args.rest.get(1) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let t = match store.task_or_why(task) {
        Ok(t) => t,
        Err(why) => return refuse(why),
    };
    let rec = record(&t);
    if args.json() {
        println!("{}", serde_json::to_string_pretty(&json(&t)).unwrap_or_default());
        return 0;
    }
    if rec.pieces.is_empty() {
        println!("{} has no pieces", t.id);
        return 0;
    }
    let Some(n) = args.rest.get(2) else {
        println!("{}", table(&rec));
        return 0;
    };
    let Some(p) = n.parse::<usize>().ok().and_then(|n| rec.pieces.iter().find(|p| p.n == n)) else {
        return refuse(format!("{} has no piece {n}", t.id));
    };
    println!("{} · {}\n{}", t.id, p.id(&t.id), p.lines());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pieces::{refuses_legs, At, State};

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("pieces-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a row", "m-1");
        t.set_status(Status::Doing);
        t.body = "## Overview\nthe work\n\n## Log\n- 2026-10-18 claimed\n".into();
        store.save_task(&t).unwrap();
        (env, store)
    }

    fn file(env: &util::Isolated, name: &str, text: &str) -> String {
        let p = env.home().join(name);
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().to_string()
    }

    fn parse(line: &[&str]) -> Args {
        Args::parse(line.iter().map(|s| (*s).to_string()).collect())
    }

    #[test]
    fn the_verbs_add_show_drop_and_cap_and_refuse_what_the_rules_refuse() {
        let (env, store) = scratch("verbs");
        let overview = file(&env, "overview.md", "Read the call sites.");
        let go = |line: &[&str]| run(&store, &parse(line), None);
        assert_eq!(go(&["piece", "add", "m-1", "contract types", "--batch", "next", "--overview", &overview, "--done-when", "fixtures read back", "--check", "tsc (2m): npx tsc --noEmit", "--check", "unit: cargo test pieces::"]), 0);
        assert_eq!(go(&["piece", "add", "m-1", "board", "--batch=2", "--overview", &overview, "--done-when", "renders"]), 0);
        assert_eq!(go(&["piece", "add", "m-1", "rail", "--batch", "next", "--overview", &overview, "--done-when", "decided", "--agent", "claude opus high", "--why", "chooses between layouts"]), 0);
        let rec = record(&store.find_task("m-1").unwrap());
        assert_eq!(rec.pieces.iter().map(|p| (p.n, p.batch)).collect::<Vec<_>>(), vec![(1, 1), (2, 2), (3, 3)]);
        assert_eq!(rec.pieces[0].checks, vec!["tsc (2m): npx tsc --noEmit", "unit: cargo test pieces::"]);
        assert_eq!(crate::legs::policy_line(&rec.pieces[2].policy), "claude opus high");
        // Refused: no why, no batch, no done-when, no overview, a gap, a bad check.
        assert_eq!(go(&["piece", "add", "m-1", "x", "--batch", "next", "--overview", &overview, "--done-when", "d", "--agent", "claude opus high"]), 1);
        assert_eq!(go(&["piece", "add", "m-1", "x", "--overview", &overview, "--done-when", "d"]), 1);
        assert_eq!(go(&["piece", "add", "m-1", "x", "--batch", "next", "--overview", &overview]), 1);
        assert_eq!(go(&["piece", "add", "m-1", "x", "--batch", "next", "--done-when", "d"]), 1);
        assert_eq!(go(&["piece", "add", "m-1", "x", "--batch", "9", "--overview", &overview, "--done-when", "d"]), 1);
        assert_eq!(go(&["piece", "add", "m-1", "x", "--batch", "soon", "--overview", &overview, "--done-when", "d"]), 1);
        assert_eq!(go(&["piece", "add", "m-1", "x", "--batch", "next", "--overview", &overview, "--done-when", "d", "--check", "label (never): cmd"]), 1);
        assert_eq!(go(&["piece", "add", "m-1"]), 2);
        assert_eq!(go(&["piece", "show", "m-1"]), 0);
        assert_eq!(go(&["piece", "show", "m-1", "2"]), 0);
        assert_eq!(go(&["piece", "show", "m-1", "9"]), 1);
        assert_eq!(go(&["piece", "drop", "m-1", "2", "the", "split", "was", "wrong"]), 0);
        assert_eq!(go(&["piece", "drop", "m-1", "2", "again"]), 1);
        assert_eq!(go(&["piece", "drop", "m-1", "3"]), 1, "a drop takes the reason");
        assert_eq!(go(&["piece", "cap", "m-1", "2"]), 0);
        assert_eq!(go(&["piece", "cap", "m-1", "0"]), 1);
        assert_eq!(go(&["piece", "bogus"]), 2);
        let rec = record(&store.find_task("m-1").unwrap());
        assert_eq!(rec.cap, 2);
        assert_eq!(rec.pieces[1].state, State::Dropped);
        assert_eq!(rec.pieces[1].why.as_deref(), Some("the split was wrong"));
    }

    #[test]
    fn a_from_file_adds_three_pieces_in_two_batches_in_one_commit() {
        let (env, store) = scratch("from");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&store.root).args(args).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
        let before: usize = git(&["rev-list", "--count", "HEAD"]).parse().unwrap();
        let blocks = file(
            &env,
            "pieces.md",
            "### contract types\nbatch: 1\ndone-when: fixtures read back\ncheck: tsc (2m): npx tsc --noEmit\nTypes and a fixture each.\nSecond line.\n\n\
             ### board\nbatch: 2\ndone-when: renders\ncheck: board (1m): npx vitest run src/board\nThe board.\n\n\
             ### rail\nbatch: 2\ndone-when: laid out\nagent: claude opus high\nwhy: chooses between layouts\nThe rail.\n",
        );
        let go = |line: &[&str]| run(&store, &parse(line), None);
        assert_eq!(go(&["piece", "add", "m-1", "--from", &blocks]), 0);
        let rec = record(&store.find_task("m-1").unwrap());
        assert_eq!(rec.pieces.iter().map(|p| (p.n, p.batch, p.title.as_str())).collect::<Vec<_>>(), vec![(1, 1, "contract types"), (2, 2, "board"), (3, 2, "rail")]);
        assert_eq!(rec.pieces[0].overview, "Types and a fixture each.\nSecond line.");
        assert_eq!(rec.pieces[0].checks, vec!["tsc (2m): npx tsc --noEmit"]);
        assert_eq!(rec.pieces[2].why.as_deref(), Some("chooses between layouts"));
        let after: usize = git(&["rev-list", "--count", "HEAD"]).parse().unwrap();
        assert_eq!(after - before, 1, "one store commit for the three pieces");
        // A block that cannot go in leaves the others out too.
        let bad = file(&env, "bad.md", "### ok\nbatch: next\ndone-when: d\nfine\n\n### no done-when\nbatch: next\nfine\n");
        assert_eq!(go(&["piece", "add", "m-1", "--from", &bad]), 1);
        assert_eq!(record(&store.find_task("m-1").unwrap()).pieces.len(), 3);
        assert_eq!(git(&["rev-list", "--count", "HEAD"]).parse::<usize>().unwrap(), after);
        // Pieces that say `next` share the one new batch.
        let nexts = file(&env, "nexts.md", "### a\ndone-when: d\nthe a\n\n### b\ndone-when: d\nthe b\n");
        assert_eq!(go(&["piece", "add", "m-1", "--from", &nexts]), 0);
        let rec = record(&store.find_task("m-1").unwrap());
        assert_eq!((rec.pieces[3].batch, rec.pieces[4].batch), (3, 3));
        let junk = file(&env, "junk.md", "words before any heading\n");
        assert_eq!(go(&["piece", "add", "m-1", "--from", &junk]), 1);
        let empty = file(&env, "empty.md", "");
        assert_eq!(go(&["piece", "add", "m-1", "--from", &empty]), 1);
    }

    #[test]
    fn the_verbs_take_the_actor_from_the_seat_they_are_run_in() {
        let (env, store) = scratch("actors");
        let overview = file(&env, "overview.md", "do it");
        let at = |pane: Option<&str>, line: &[&str]| run(&store, &parse(line), pane);
        let add = |pane: Option<&str>, title: &str, batch: &str| {
            at(pane, &["piece", "add", "m-1", title, "--batch", batch, "--overview", &overview, "--done-when", "d"])
        };
        // The seat holding the claim (a binding to the row) may write.
        store.set_binding("cpd-1", json!({ "task_id": "m-1", "pane_id": "cpd-1" }));
        assert_eq!(add(Some("cpd-1"), "one", "next"), 0);
        // A seat bound to another row may not.
        store.set_binding("cpd-2", json!({ "task_id": "z-9", "pane_id": "cpd-2" }));
        assert_eq!(add(Some("cpd-2"), "nope", "next"), 1);
        // A seat of a leg of this row is no claimant.
        store.set_binding("cpd-3", json!({ "task_id": "m-1", "pane_id": "cpd-3", "leg": "scout" }));
        assert_eq!(add(Some("cpd-3"), "nope", "next"), 1);
        assert_eq!(add(None, "two", "next"), 0);
        // A seated piece adds only to later batches.
        let mut t = store.find_task("m-1").unwrap();
        let mut rec = record(&t);
        rec.pieces[0].state = State::Running;
        write(&mut t, &rec);
        store.save_task(&t).unwrap();
        store.set_binding("cpd-4", json!({ "task_id": "m-1", "pane_id": "cpd-4", "piece": 1 }));
        assert_eq!(add(Some("cpd-4"), "same batch", "1"), 1);
        assert_eq!(add(Some("cpd-4"), "later", "next"), 0);
        let rec = record(&store.find_task("m-1").unwrap());
        assert_eq!(rec.pieces.last().unwrap().by, Some(1));
        assert_eq!(at(Some("cpd-4"), &["piece", "drop", "m-1", "2", "not mine"]), 1);
        assert_eq!(at(Some("cpd-4"), &["piece", "drop", "m-1", "3", "mine"]), 0);
        assert_eq!(at(Some("cpd-4"), &["piece", "cap", "m-1", "2"]), 1);
    }

    #[test]
    fn a_row_with_legs_takes_no_piece_and_a_row_with_pieces_takes_no_leg() {
        let (env, store) = scratch("both");
        let mut other = Task::new("another row", "m-2");
        other.set_status(Status::Doing);
        other.body = "## Overview\nthe work\n\n## Log\n- 2026-10-18 claimed\n".into();
        store.save_task(&other).unwrap();
        let order = file(&env, "o.md", "do it");
        let piece = |row: &str| run(&store, &parse(&["piece", "add", row, "p", "--batch", "next", "--overview", &order, "--done-when", "done"]), None);
        let leg = |row: &str| crate::legs::run(&store, &parse(&["leg", "add", row, "scout", "--order", &order]), None);
        // Legs first: a piece is refused, naming the legs.
        assert_eq!(leg("m-1"), 0);
        assert_eq!(piece("m-1"), 1);
        assert!(record(&store.find_task("m-1").unwrap()).pieces.is_empty());
        assert!(refuses_pieces(&store.find_task("m-1").unwrap()).unwrap().contains("## Legs"));
        // Pieces first: a leg is refused, naming the pieces.
        assert_eq!(piece("m-2"), 0);
        assert_eq!(leg("m-2"), 1);
        assert!(crate::legs::legs(&store.find_task("m-2").unwrap()).is_empty());
        assert!(refuses_legs(&store.find_task("m-2").unwrap()).unwrap().contains("## Pieces"));
        assert!(refuses_legs(&store.find_task("m-1").unwrap()).is_none());
    }
}
