//! `wsp machine` — the machines agents can be run on.
//!
//! A machine here is an *executor*: a second box the seat drives, running a
//! headless herdr and nothing of ours. Nobody sits at it, it holds no store and
//! no state, and its `wsp` is a shim that runs this machine's. So a machine
//! record is small on purpose — a name, an ssh alias, and enough about the box
//! to recognise it in a list.
//!
//! The name is load-bearing in a way a project slug is not: it is the suffix on
//! a host-qualified id, `w0:p3@mb2`, which is how every herdr call finds the
//! right socket. See [`crate::model::valid_machine_name`].
//!
//! Nothing here dials anything, and `add` deliberately does not test the
//! connection. Reachability belongs to the daemon's tunnel supervisor, which is
//! the only writer of `machines.json`; a machine you add while it is switched
//! off must still be a machine, and a second opinion on whether it answers is
//! how "offline" and "empty" get confused.

use serde_json::json;

use crate::model::{valid_machine_name, Machine};
use crate::store::{MachineLive, Store};
use crate::util::{self, Paint};
use crate::Args;

pub fn dispatch(store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(|s| s.as_str()).unwrap_or("ls") {
        "add" | "new" => add(store, args),
        "ls" | "list" => list(store, args),
        "show" | "get" => show(store, args),
        "set" => set(store, args),
        "rm" | "remove" | "retire" => rm(store, args),
        other => {
            eprintln!("wsp machine: unknown subcommand `{other}`");
            2
        }
    }
}

/// This seat, in the vocabulary a machine name uses: short and lowercase.
///
/// Not a machine record and never written down — the seat is where you are,
/// not somewhere you reach. It exists to be compared against.
fn seat_name() -> String {
    util::hostname().split('.').next().unwrap_or_default().to_ascii_lowercase()
}

/// Resolve a typed name: exact, then unique prefix. No fuzzy title match, and
/// no slugifying — a machine you meant to name is worth an error rather than a
/// guess, because the guess ends up in an id.
fn find(store: &Store, needle: &str) -> Option<Machine> {
    let all = store.machines();
    if let Some(m) = all.iter().find(|m| m.name == needle) {
        return Some(m.clone());
    }
    let mut hits = all.iter().filter(|m| m.name.starts_with(needle));
    let first = hits.next()?.clone();
    hits.next().is_none().then_some(first)
}

pub fn add(store: &Store, args: &Args) -> i32 {
    let Some(name) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp machine add <name> [<ssh-target>] [--os darwin] [--arch arm64]");
        return 2;
    };
    // The ssh alias defaults to the name, which is the common case and the one
    // worth not having to say twice.
    let ssh = args.rest.get(2).cloned().or_else(|| args.get("ssh")).unwrap_or_else(|| name.clone());

    if let Err(why) = valid_machine_name(&name) {
        eprintln!("wsp: `{name}` is not a usable machine name — {why}");
        return 2;
    }
    if store.machine(&name).is_some() {
        eprintln!("wsp: machine `{name}` already exists — wsp machine set {name} ssh=…");
        return 1;
    }
    // The seat is not one of its own executors. Claims already record
    // `host: hostname()` and remote ids are the ones carrying `@`; a machine
    // sharing this host's name would make "local" and "remote" the same word in
    // both places at once.
    //
    // Against the *short, lowercased* hostname, because that is the name a
    // person would actually type. `hostname` here answers
    // `MacBook-Pro-of-Ed.local`, which no machine name could ever equal — a
    // guard that only fires on a string nobody can type is not a guard.
    if name == seat_name() {
        eprintln!(
            "wsp: `{name}` is this machine. The seat is not an executor of itself — \
             a machine record is for the far end of an ssh connection."
        );
        return 1;
    }

    let mut m = Machine::new(&name, &ssh);
    m.os = args.get("os").unwrap_or_default();
    m.arch = args.get("arch").unwrap_or_default();
    if let Some(note) = args.get("note") {
        m.body = format!("## Overview\n{note}\n");
    }

    if let Err(e) = store.save_machine(&m) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("machine-added", json!({ "name": m.name, "ssh": m.ssh }));
    store.git_commit(&format!("wsp: add machine {}", m.name));

    if args.json() {
        println!("{}", machine_json(&m, None));
        return 0;
    }
    let p = Paint::new();
    println!("added machine {} {}", p.bold(&m.name), p.dim(&format!("ssh {}", m.ssh)));
    println!(
        "  {}",
        p.dim("the daemon dials it — `wsp machine ls` for whether it answers")
    );
    // Said now rather than discovered later: everything about reaching the
    // machine lives outside wsp, so the next step is not a wsp command.
    println!("  {}", p.dim(&format!("needs: a `Host {}` block in ~/.ssh/config, and herdr server running there", m.ssh)));
    // The mirrored-path assumption, said out loud at the moment it is made
    // rather than discovered as a tunnel that will not come up.
    println!("  {}", p.dim(&format!("forwards {} — wsp machine set {} herdr_sock=… if that is not where it is", m.herdr_sock, m.name)));
    0
}

pub fn list(store: &Store, args: &Args) -> i32 {
    let machines = store.machines();
    let live = store.machines_live();
    let p = Paint::new();

    if args.json() {
        let rows: Vec<_> = machines.iter().map(|m| machine_json(m, live.get(&m.name))).collect();
        println!("{}", json!({ "seat": util::hostname(), "machines": rows }));
        return 0;
    }

    if machines.is_empty() {
        println!("{}", p.dim("no machines — this seat only"));
        println!("{}", p.dim(&format!("  seat  {}", util::hostname())));
        println!("{}", p.dim("  wsp machine add <name> <ssh-target>"));
        return 0;
    }

    let w = machines.iter().map(|m| m.name.len()).max().unwrap_or(4).max(4);
    let ssh_w = machines.iter().map(|m| m.ssh.len()).max().unwrap_or(3).max(3);
    println!("{}", p.dim(&format!("{:<w$}  {:<ssh_w$}  {}", "NAME", "SSH", "STATE")));
    for m in &machines {
        let (state, mut note) = state_of(&p, m, live.get(&m.name));
        // Why, not just that — but cut, because an ssh failure is a sentence
        // and a half and this is one cell of a list. `show` prints it whole,
        // which is where you go when forty characters were not enough.
        if let Some(err) = live.get(&m.name).map(|l| l.error.as_str()).filter(|e| !e.is_empty()) {
            note = format!("{note} · {}", util::truncate(err, 44));
        }
        println!(
            "{}  {}  {}{}",
            p.bold(&format!("{:<w$}", m.name)),
            p.dim(&format!("{:<ssh_w$}", m.ssh)),
            state,
            if note.is_empty() { String::new() } else { format!("  {}", p.dim(&note)) },
        );
    }
    // The seat has no record of its own — it is where you are, not somewhere
    // you reach — but a list of machines that leaves out the one you are on
    // reads as if it were missing. A footer rather than a row: a hostname is
    // routinely longer than every machine name put together, and widening the
    // whole table to fit one cell that is not really in it is the wrong trade.
    println!("{}", p.dim(&format!("seat  {}", util::hostname())));
    0
}

pub fn show(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp machine show <name>");
        return 2;
    };
    let Some(m) = find(store, &needle) else {
        eprintln!("wsp: no machine matching `{needle}`");
        return 1;
    };
    let live = store.machine_live(&m.name);

    if args.json() {
        println!("{}", machine_json(&m, live.as_ref()));
        return 0;
    }

    let p = Paint::new();
    let (state, note) = state_of(&p, &m, live.as_ref());
    println!("{}  {}{}", p.bold(&m.name), state, if note.is_empty() { String::new() } else { format!("  {}", p.dim(&note)) });
    println!();
    println!("ssh       {}", m.ssh);
    println!("herdr     {}", m.herdr_sock);
    let box_line = [m.os.as_str(), m.arch.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
    if !box_line.is_empty() {
        println!("box       {box_line}");
    }
    println!("status    {}", m.status);
    if !m.added.is_empty() {
        println!("added     {}", m.added);
    }
    if let Some(l) = &live {
        println!("tunnel    {}", if l.tunnel.is_empty() { "—" } else { &l.tunnel });
        if !l.socket.is_empty() {
            println!("socket    {}", util::contract(std::path::Path::new(&l.socket)));
        }
        if !l.herdr_version.is_empty() {
            println!("version   {}", l.herdr_version);
        }
        if !l.error.is_empty() {
            println!("error     {}", p.red(&l.error));
        }
    } else {
        println!("{}", p.dim("tunnel    — the daemon has not reported on this machine"));
    }
    if !m.body.trim().is_empty() {
        println!();
        println!("{}", m.body.trim_end());
    }
    0
}

pub fn set(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp machine set <name> ssh=… herdr_sock=… os=… arch=… status=active|retired");
        return 2;
    };
    let Some(mut m) = find(store, &needle) else {
        eprintln!("wsp: no machine matching `{needle}`");
        return 1;
    };

    let mut changed = Vec::new();
    for kv in args.rest.iter().skip(2) {
        let Some((k, v)) = kv.split_once('=') else {
            eprintln!("wsp: `{kv}` is not key=value");
            return 2;
        };
        match k {
            "ssh" => m.ssh = v.to_string(),
            "herdr_sock" => m.herdr_sock = v.to_string(),
            "os" => m.os = v.to_string(),
            "arch" => m.arch = v.to_string(),
            "status" => match v {
                "active" | "retired" => m.status = v.to_string(),
                _ => {
                    eprintln!("wsp: status is `active` or `retired`, not `{v}`");
                    return 2;
                }
            },
            other => {
                eprintln!("wsp: machines have no `{other}` — ssh, herdr_sock, os, arch, status");
                return 2;
            }
        }
        changed.push(k.to_string());
    }
    if changed.is_empty() {
        eprintln!("wsp: nothing to set — try `wsp machine show {}`", m.name);
        return 2;
    }

    if let Err(e) = store.save_machine(&m) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.log_event("machine-set", json!({ "name": m.name, "fields": changed }));
    store.git_commit(&format!("wsp: machine set {} {}", m.name, changed.join(" ")));

    if args.json() {
        println!("{}", machine_json(&m, store.machine_live(&m.name).as_ref()));
    } else {
        println!("{} {}", m.name, Paint::new().dim(&changed.join(" ")));
    }
    0
}

/// Retire a machine, or with `--force` remove it entirely.
///
/// Retiring is the default because the record is what makes a machine that has
/// gone away a row with a last-seen on it rather than a hole: the agents that
/// ran there are in the log, and their pane ids still carry its name. Deleting
/// the file is available for the machine you added by mistake thirty seconds
/// ago, which is the only case where there is nothing to keep.
pub fn rm(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.get(1).cloned() else {
        eprintln!("usage: wsp machine rm <name> [--force]");
        return 2;
    };
    let Some(mut m) = find(store, &needle) else {
        eprintln!("wsp: no machine matching `{needle}`");
        return 1;
    };
    let p = Paint::new();

    if !args.has("force") {
        if !m.is_active() {
            println!("{}", p.dim(&format!("{} is already retired — --force removes the record", m.name)));
            return 0;
        }
        m.status = "retired".into();
        if let Err(e) = store.save_machine(&m) {
            eprintln!("wsp: write failed: {e}");
            return 1;
        }
        // The liveness record goes with it: the daemon will stop dialling a
        // retired machine, so whatever it last said would sit there for ever
        // claiming a connection that nothing is holding up.
        store.clear_machine_live(&m.name);
        store.log_event("machine-retired", json!({ "name": m.name }));
        store.git_commit(&format!("wsp: retire machine {}", m.name));
        if args.json() {
            println!("{}", json!({ "retired": m.name }));
        } else {
            println!("retired {} {}", m.name, p.dim("— still listed, no longer dialled"));
        }
        return 0;
    }

    let path = store.machine_path(&m.name);
    if let Err(e) = std::fs::remove_file(&path) {
        eprintln!("wsp: {}: {e}", path.display());
        return 1;
    }
    // Removed by hand rather than through a save, so the commit has to be told
    // about it.
    store.wrote(path);
    store.clear_machine_live(&m.name);
    store.log_event("machine-removed", json!({ "name": m.name }));
    store.git_commit(&format!("wsp: machine rm {}", m.name));

    if args.json() {
        println!("{}", json!({ "removed": m.name }));
    } else {
        println!("removed machine {}", m.name);
    }
    0
}

/// The one column that says how a machine is doing, and the only place the
/// three states are turned into words.
///
/// Three states and not two. "Retired" is a decision, "connected" is the
/// daemon answering, and the middle one — reachable false, or no report at all
/// — is *unreachable*, which is emphatically not "has nothing running on it".
/// Everything in this design that can go badly wrong goes wrong by collapsing
/// those two, so they do not share a rendering here either.
fn state_of(p: &Paint, m: &Machine, live: Option<&MachineLive>) -> (String, String) {
    if !m.is_active() {
        return (p.dim("retired"), String::new());
    }
    match live {
        Some(l) if l.reachable => (
            p.green("connected"),
            if l.herdr_version.is_empty() { String::new() } else { format!("herdr {}", l.herdr_version) },
        ),
        Some(l) => {
            let seen = match l.last_seen.as_str() {
                "" => "never seen".to_string(),
                s => format!("last seen {} ago", util::duration_human(util::since(s))),
            };
            (p.yellow("offline"), seen)
        }
        // No record at all is not the same as a failed dial, and saying
        // "offline" here would blame the machine for the daemon not running.
        None => (p.dim("—"), "no daemon report yet".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Paint {
        // Under `cargo test` stdout is not a terminal, so nothing is painted
        // and these compare bare words.
        Paint::new()
    }

    /// The hazard the whole executor design is built around, at the one place a
    /// person actually reads it. A machine the daemon has never reported on, a
    /// machine that answered and then stopped, and a machine that answers — if
    /// any two of those three render the same, the list is lying about the one
    /// thing it exists to say.
    #[test]
    fn unreachable_offline_and_never_reported_are_three_different_answers() {
        let p = plain();
        let m = Machine::new("mb2", "mb2");

        let (none, note) = state_of(&p, &m, None);
        assert!(note.contains("no daemon report"), "silence is the daemon's, not the machine's");

        let (up, _) = state_of(&p, &m, Some(&MachineLive { reachable: true, ..Default::default() }));
        let (down, why) = state_of(
            &p,
            &m,
            Some(&MachineLive { reachable: false, last_seen: util::now_iso(), ..Default::default() }),
        );
        assert!(why.starts_with("last seen"), "an offline row says how long, not just that");

        let mut retired = m.clone();
        retired.status = "retired".into();
        let (gone, _) = state_of(&p, &retired, Some(&MachineLive { reachable: true, ..Default::default() }));
        assert_eq!(gone.trim(), "retired", "a retired machine reads as retired however live it is");

        let words = [none.trim(), up.trim(), down.trim(), gone.trim()];
        for (i, a) in words.iter().enumerate() {
            for b in words.iter().skip(i + 1) {
                assert_ne!(a, b, "two states rendering the same is the confusion this guards");
            }
        }
    }

    /// A machine that has never answered has no last-seen to report, and
    /// "last seen 56y ago" — the epoch, dressed as a duration — would be worse
    /// than saying nothing.
    #[test]
    fn a_machine_that_never_answered_says_so_rather_than_dating_itself_to_the_epoch() {
        let (_, why) = state_of(
            &plain(),
            &Machine::new("mb2", "mb2"),
            Some(&MachineLive { reachable: false, last_seen: String::new(), ..Default::default() }),
        );
        assert_eq!(why, "never seen");
    }
}

fn machine_json(m: &Machine, live: Option<&MachineLive>) -> serde_json::Value {
    json!({
        "name": m.name,
        "ssh": m.ssh,
        "os": m.os,
        "arch": m.arch,
        "herdr_sock": m.herdr_sock,
        "status": m.status,
        "added": m.added,
        "live": live.map(|l| l.to_value()),
    })
}
