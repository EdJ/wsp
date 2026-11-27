//! `wsp attend <task>` — one unattended attention-harness attempt, from the
//! packet to the verify result (wsp-330). It is the program `--kind attention`
//! runs ([`crate::kind_attention`]); nobody types it.
//!
//! The attempt is four steps, and each ends in a record wsp already reads:
//!
//! 1. **Name it.** A directory `<state>/attention/<task>-<time>/` and the claim
//!    taught that name as the session, so release reads tokens and dollars off
//!    it as it does for any kind.
//! 2. **Run the harness** on the row's packet, in this tree: `attention chat
//!    --yes --max-cost <cap> --deny <list>`. The cap is the row's own
//!    (`attention: 2` in its `## Packet`); the harness stops at it and exits 3.
//! 3. **Run the row's `## Checks`** in this tree ([`crate::check::run_all`]).
//!    Whatever the harness said about itself, this is the outcome: a model that
//!    reports success and fails its checks has failed.
//! 4. **Settle the row.** Green: commit what is uncommitted, `wsp review` — which
//!    marks a row of a run ready for its verifier to land (`wsp-341`); a row in
//!    no run is landed first, as before. Anything else: `wsp block` with the account. A block is the row's
//!    honest state, a person's to read, and a respawn on `claude` is theirs to
//!    choose; the wrapper never retries on a dearer kind.
//!
//! The harness is denied the verbs that end a row (`wsp done`, `land`, `review`,
//! `block`, `release`) and `git push`/`stash`: those are step 4's, and an
//! unattended model that ran them would have its attempt judged by itself.

use std::path::Path;
use std::process::Stdio;
use wsp_base::calls::Command;

use crate::agent_commands::{self};
use crate::check;
use crate::model::Task;
use crate::store::{Found, Store};
use crate::base::args::Args;

/// The harness's executable, unless `$WSP_ATTENTION` names another. A seat's
/// environment sheds `WSP_*`, so this reaches `wsp attend` run by hand and not
/// one a spawn started: an install puts `bin/attention` on the PATH.
fn program() -> String {
    std::env::var("WSP_ATTENTION").ok().filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "attention".into())
}

/// Commands the harness may not run even under `--yes`, one regular expression
/// a line. A guard against a model that reaches for the end of the row, not a
/// sandbox: it matches the command text.
const DENY: &str = "# the run settles the row; the attempt is not its own judge\n\
\\bwsp\\s+(done|land|review|block|park|release|despawn|spawn|flag|install)\\b\n\
\\bgit\\s+(push|stash|reset\\s+--hard|clean\\s+-[a-z]*f)\n\
\\bcargo\\s+fmt\\b\n\
\\b(pkill|killall)\\b\n";

/// A row's opt-in: the cap in cents and the steps it ends at.
pub struct OptIn {
    pub cap_cents: u64,
    pub steps: Vec<check::Step>,
}

/// Whether a row may run on the harness, and why not when it may not. Both
/// halves are the author's: `attention:` in the `## Packet` says the packet is
/// enough, and `## Checks` says what green is. Without the second there is
/// nothing for an unattended attempt to end at.
pub fn opt_in(t: &Task) -> Result<OptIn, String> {
    let packet = crate::packet::of(t).filter(|p| !p.off);
    let Some(cap_cents) = packet.and_then(|p| p.attention_cents) else {
        return Err(format!(
            "{} has not opted in to the attention harness — put `attention: 2` (dollars an attempt, or `on`) \
             in its ## Packet, beside the packet text that is the whole brief",
            t.id
        ));
    };
    let steps = check::task_steps(t);
    if steps.is_empty() {
        return Err(format!(
            "{} has no ## Checks — an unattended attempt ends at a command's result, so write the command there (`- cargo test x::`)",
            t.id
        ));
    }
    Ok(OptIn { cap_cents, steps })
}

fn dollars(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// What the harness is handed as its one message: the packet, then the rules of
/// an unattended run.
fn message(packet: &str, id: &str, steps: &[check::Step]) -> String {
    let commands: Vec<&str> = steps.iter().map(|s| s.command.lines().next().unwrap_or_default()).collect();
    format!(
        "{packet}\n---\nYou are running unattended in your own git tree. Do the change above, run `{}` until it \
         passes, and commit your work with git (`git add <files>`, `git commit -m \"{id}: <what changed>\"`). \
         Do not run `wsp` verbs, `git push` or `git stash`: the run checks your work with that command, then \
         reviews the row (landing it first when no run verifies it), or blocks it. Finish with one line saying what you did.",
        commands.join("` and `")
    )
}

/// How the harness ended: its exit code, `None` for a signal, or why it never
/// started.
type Ended = Result<Option<i32>, String>;

fn clause(ran: Option<&crate::agent_kind::Ran>, cap_cents: u64, exit: &Ended) -> String {
    let spent = ran
        .map(|r| {
            let what = r.spend.clause().unwrap_or_else(|| "nothing measured".into());
            format!("{} turns · {what}", r.turns)
        })
        .unwrap_or_else(|| "nothing logged".into());
    let end = match exit {
        Ok(Some(0)) => "answered".to_string(),
        Ok(Some(3)) => format!("stopped at its ${} cap", dollars(cap_cents)),
        Ok(Some(n)) => format!("exited {n}"),
        Ok(None) => "was killed by a signal".to_string(),
        Err(why) => format!("did not start: {why}"),
    };
    format!("the harness {end} · {spent} · cap ${}", dollars(cap_cents))
}

fn git(dir: &Path, argv: &[&str]) -> Option<String> {
    let out = Command::new("git").args(argv).current_dir(dir).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run `wsp <verb> <task> -` with the account on stdin, the way an agent is
/// told to, so prose never passes through a shell string.
fn wsp(verb: &str, task: &str, account: &str) -> bool {
    use std::io::Write;
    let Ok(exe) = wsp_base::util::self_exe() else { return false };
    let Ok(mut child) = Command::new(exe).args([verb, task, "-"]).stdin(Stdio::piped()).spawn() else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(account.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Say what the attempt is doing the way Claude Code's hooks do, through `wsp
/// report`: the harness fires none, and a seat that has said nothing reads as
/// still starting, so `spawn` would wait out its launch window and end the
/// attempt it was waiting for. `working` holds from the harness's first call to
/// the checks' end, so nothing treats the seat as idle mid-attempt.
fn say(hook: &str, session: &str) {
    use std::io::Write;
    if std::env::var_os(crate::base::seat::SEAT_ENV).is_none() {
        return;
    }
    let Ok(exe) = wsp_base::util::self_exe() else { return };
    let Ok(mut child) = Command::new(exe).args(["report", hook]).stdin(Stdio::piped()).spawn() else { return };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(serde_json::json!({ "hook_event_name": hook, "session_id": session }).to_string().as_bytes());
    }
    let _ = child.wait();
}

pub fn attend(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first() else {
        eprintln!("usage: wsp attend <task> [--model M]   (the program `--kind attention` runs)");
        return 2;
    };
    let t = match store.resolve_task(needle) {
        Found::One(t) => *t,
        other => {
            eprintln!("{}", other.why(needle).unwrap_or_default());
            return 1;
        }
    };
    let opted = match opt_in(&t) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };
    let here = std::env::current_dir().unwrap_or_default();
    let packet = match crate::packet::build(&t, &here) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    let session = format!("{}-{}", t.id, crate::util::epoch_of(&crate::util::now_iso()));
    let dir = crate::kind_attention::attempt_dir(&session);
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join("deny.txt"), DENY)) {
        eprintln!("wsp: cannot make {}: {e}", dir.display());
        return 1;
    }
    if let Ok(seat) = std::env::var(crate::base::seat::SEAT_ENV) {
        crate::cmd_agent::learn_sessions(store, std::iter::once((seat.as_str(), session.as_str(), "attention")));
    }

    say("SessionStart", &session);
    say("UserPromptSubmit", &session);
    let cap = dollars(opted.cap_cents);
    let mut run = Command::new(program());
    run.args(["chat", "."]).args(["--yes", "--max-cost", &cap, "--log-dir"]).arg(&dir);
    run.arg("--deny").arg(dir.join("deny.txt"));
    if let Some(model) = args.get("model") {
        run.args(["--model", &model]);
    }
    run.arg("-m").arg(message(&packet, &t.id, &opted.steps));
    println!("wsp attend: {} on the attention harness, cap ${cap}, log {}", t.id, dir.display());
    let exit: Ended = run.status().map(|s| s.code()).map_err(|e| {
        format!("`{}` could not run ({e}) — put the attention repo's bin/attention on the PATH", program())
    });

    let ran = agent_commands::of("attention").ran(&session, "", 0);
    let ran_clause = clause(ran.as_ref(), opted.cap_cents, &exit);
    // Under `WSP_JOBS=manager` the checks are jobs in the row's own tree, as
    // `wsp check`'s are; under `inproc`, here as before.
    let runner = match check::Runner::chosen(store, check::Whose::Row(&t.id), check::TreeKind::Work, &here, None) {
        Ok(r) => r,
        Err(why) => {
            eprintln!("wsp attend: {why}");
            return 1;
        }
    };
    let checks = check::run_all(&here, &opted.steps, &dir.join("verify.log"), None, &runner, |_| {});
    let green = check::passed(&checks);
    say("Stop", &session);
    let account = format!(
        "{} · {ran_clause}\nverify (`## Checks`, run in this tree): {}\n{}",
        if green { "attention attempt green" } else { "attention attempt did not pass" },
        if green { "passed" } else { "failed" },
        check::account(&checks)
    );
    println!("{account}");

    if green {
        if git(&here, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty()) {
            let _ = git(&here, &["add", "-A"]);
            let _ = git(&here, &["commit", "-q", "-m", &format!("{}: {} (attention)", t.id, t.title)]);
        }
        let landed = crate::worklist::verified_by_a_run(store, &t.id)
            || Command::new(wsp_base::util::self_exe().unwrap_or_default())
                .args(["land", &t.id])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        // Review asks for the line a glance reads; a row filed without one gets
        // the one its title gives, as review's own refusal suggests.
        if t.caption.is_none() {
            wsp("caption", &t.id, &crate::caption::title(&t.title));
        }
        if landed && wsp("review", &t.id, &account) {
            return 0;
        }
        let why = format!("{account}\nit passed its checks but could not be landed and reviewed — `wsp land {}` says why", t.id);
        wsp("block", &t.id, &why);
        return 1;
    }
    wsp("block", &t.id, &account);
    1
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["attend"],
        run: attend,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 50,
                text: r##"  wsp attend <id>                   one attention-harness attempt on a row, to its checks'
                                    result: what `spawn --kind attention` runs; nobody types it"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn row(section: &str, checks: &str) -> Task {
        let mut t = Task::default();
        t.id = "t-1".into();
        t.title = "Add a flag".into();
        t.body = format!("## Packet\n{section}\n\n## Checks\n{checks}\n");
        t
    }

    #[test]
    fn a_row_opts_in_by_naming_a_cap_and_a_command() {
        let o = opt_in(&row("Do it.\nattention: 1.50", "- cargo test x::")).ok().unwrap();
        assert_eq!((o.cap_cents, o.steps.len()), (150, 1));
        assert_eq!(opt_in(&row("Do it.\nattention: on", "- true")).ok().unwrap().cap_cents, 200);
    }

    #[test]
    fn a_row_that_did_not_opt_in_is_refused_with_the_line_to_add() {
        let e = opt_in(&row("Do it.", "- true")).err().unwrap();
        assert!(e.contains("attention: 2"), "{e}");
        let off = opt_in(&row("packet: off\nattention: 2\nDo it.", "- true")).err().unwrap();
        assert!(off.contains("has not opted in"), "a declined packet is no packet");
    }

    #[test]
    fn an_opted_in_row_with_nothing_to_end_at_is_refused() {
        let e = opt_in(&row("Do it.\nattention: 2", "")).err().unwrap();
        assert!(e.contains("no ## Checks"), "{e}");
    }

    #[test]
    fn a_cap_that_is_not_money_is_not_an_opt_in() {
        for bad in ["attention: 0", "attention: free", "attention: -1"] {
            assert!(opt_in(&row(&format!("Do it.\n{bad}"), "- true")).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_message_is_the_packet_then_the_rules_naming_the_command() {
        let steps = check::task_steps(&row("x", "- cargo test x::"));
        let m = message("# Packet · t-1", "t-1", &steps);
        assert!(m.starts_with("# Packet · t-1\n---\n"));
        assert!(m.contains("`cargo test x::`") && m.contains("t-1: <what changed>"));
    }

    #[test]
    fn the_deny_list_names_the_verbs_that_end_a_row() {
        for verb in ["done", "land", "review", "block", "release"] {
            assert!(DENY.contains(verb), "{verb}");
        }
        assert!(DENY.lines().all(|l| l.starts_with('#') || !l.trim().is_empty()));
    }

    #[test]
    fn the_account_says_how_the_harness_ended() {
        assert!(clause(None, 200, &Ok(Some(3))).contains("stopped at its $2.00 cap"));
        let answered = clause(None, 150, &Ok(Some(0)));
        assert!(answered.contains("answered") && answered.contains("nothing logged"));
        assert!(clause(None, 200, &Ok(None)).contains("killed by a signal"));
        assert!(clause(None, 200, &Err("no such file".into())).contains("did not start: no such file"));
    }
}
