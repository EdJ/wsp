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

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::herdr;
use crate::model::{valid_machine_name, Machine, Task};
use crate::place_herdr;
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
    // Written down here rather than defaulted in the model, and here rather
    // than left empty for the tunnel to fill in: a command is allowed to know
    // which backend it is adding a machine for, and a record that says where it
    // forwards is one a person can correct with `set` before the first dial.
    // The empty case still works — see `tunnel::backend_at` — it just cannot be
    // read out of the file.
    m.backend_at = place_herdr::mirrored_socket();
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
    println!("  {}", p.dim(&format!("forwards {} — wsp machine set {} backend_at=… if that is not where it is", m.backend_at, m.name)));
    0
}

/// Everything the machines view reads, gathered into one value.
///
/// The same bargain [`crate::panel::Snapshot`] and [`crate::overlap::World`]
/// make, and for the same reason: this view joins a file the store holds with
/// an answer herdr gives, and while it did both itself the only way to see a
/// frame was to have a daemon, a tunnel and a second box. The three states it
/// exists to tell apart — never reported, answered and stopped, answering — are
/// exactly the ones nobody can stand up on demand.
pub(crate) struct Fleet {
    pub machines: Vec<Machine>,
    pub live: BTreeMap<String, MachineLive>,
    /// Every agent there is, `@machine`-qualified. Empty when herdr is not
    /// answering, which is a machines list without the agents on it rather than
    /// no machines list — the durable half of a machine is a file and does not
    /// need a socket to be read.
    pub agents: Vec<herdr::Pane>,
    pub bindings: BTreeMap<String, Value>,
    pub tasks: Vec<Task>,
    /// This seat's hostname, in the snapshot rather than read where it is
    /// drawn. A fixture that called `util::hostname` would render the name of
    /// whichever box the test ran on, which is a frame that differs between two
    /// machines for a reason that has nothing to do with the code.
    pub seat: String,
}

impl Fleet {
    /// The live join: the store for what machines exist and what the daemon
    /// last saw, herdr for who is on them.
    pub(crate) fn live(store: &Store) -> Fleet {
        Fleet {
            machines: store.machines(),
            live: store.machines_live(),
            agents: match herdr::available() {
                true => herdr::agents().unwrap_or_default(),
                false => Vec::new(),
            },
            bindings: store.bindings(),
            tasks: store.tasks(),
            seat: util::hostname(),
        }
    }
}

/// One agent, as a machines list wants it: which pane, what it is, what it is
/// holding.
struct Standing {
    machine: String,
    pane: String,
    agent: String,
    state: String,
    holding: String,
}

/// Every agent there is, partitioned by the machine it is on.
///
/// A partition of one list rather than a query per machine: [`herdr::agents`]
/// already fans out and comes back `@machine`-qualified, so where each one is
/// is written on it. `""` is this seat.
fn standing(f: &Fleet) -> Vec<Standing> {
    f.agents
        .iter()
        .map(|a| {
            let held = f
                .bindings
                .get(&a.pane_id)
                .and_then(|b| b.get("task_id"))
                .and_then(|t| t.as_str())
                .and_then(|id| f.tasks.iter().find(|t| t.id == id));
            Standing {
                machine: herdr::host_of(&a.pane_id).unwrap_or("").to_string(),
                pane: a.pane_id.clone(),
                agent: match a.agent.is_empty() {
                    true => "a shell".into(),
                    false => a.agent.clone(),
                },
                state: a.agent_status.clone(),
                holding: match held {
                    Some(t) => t.title.clone(),
                    None if !a.title.is_empty() => format!("({})", a.title),
                    None => String::new(),
                },
            }
        })
        .collect()
}

/// The machines view: what exists, whether it is answering, and who is on it.
///
/// A command and not a panel view, for now. The panel is the other surface and
/// wants the same three states and the same partition; this is the half that
/// does not need the panel to hold still to be written. See wsp-054.
pub fn list(store: &Store, args: &Args) -> i32 {
    let f = Fleet::live(store);
    match args.json() {
        true => println!("{}", list_json(&f)),
        false => {
            for l in list_lines(&f, &Paint::new()) {
                println!("{l}");
            }
        }
    }
    0
}

/// Partition the agents by machine. `""` is this seat.
fn agents_on<'a>(here: &'a [Standing], name: &str) -> Vec<&'a Standing> {
    here.iter().filter(|s| s.machine == name).collect()
}

fn agents_json(rows: &[&Standing]) -> Value {
    json!(rows
        .iter()
        .map(|s| json!({
            "pane": s.pane, "agent": s.agent, "state": s.state, "holding": s.holding,
        }))
        .collect::<Vec<_>>())
}

fn list_json(f: &Fleet) -> Value {
    let here = standing(f);
    let rows: Vec<_> = f
        .machines
        .iter()
        .map(|m| {
            let mut v = machine_json(m, f.live.get(&m.name));
            v["agents"] = agents_json(&agents_on(&here, &m.name));
            v
        })
        .collect();
    json!({
        "seat": f.seat,
        "machines": rows,
        "seat_agents": agents_json(&agents_on(&here, "")),
    })
}

/// The list as text, one line per element and nothing printed.
///
/// Returned rather than written so the three states this view exists to tell
/// apart can be read back by a test. Standing up the live inputs for even one
/// of them takes a daemon, a tunnel and a second box; all three at once, in one
/// frame, is not a thing anybody can arrange on demand.
fn list_lines(f: &Fleet, p: &Paint) -> Vec<String> {
    let here = standing(f);
    let mut out = Vec::new();

    // An agent line, indented under whatever it is standing on.
    let under = |out: &mut Vec<String>, rows: Vec<&Standing>| {
        let w = rows.iter().map(|s| s.pane.len()).max().unwrap_or(0);
        for s in rows {
            out.push(format!(
                "  {}  {}",
                p.dim(&format!("{:<w$}", s.pane)),
                p.dim(&format!(
                    "{} · {}{}",
                    s.agent,
                    s.state,
                    match s.holding.is_empty() {
                        true => String::new(),
                        false => format!(" · {}", util::truncate(&s.holding, 48)),
                    }
                )),
            ));
        }
    };

    // The seat has no record of its own — it is where you are, not somewhere
    // you reach — but a list of machines that leaves out the one you are on
    // reads as if it were missing, and the agents on it are half of what this
    // list is for.
    let seat = |out: &mut Vec<String>| out.push(p.bold(&format!("seat  {}", f.seat)));

    if f.machines.is_empty() {
        out.push(p.dim("no machines — this seat only"));
        out.push(p.dim("  wsp machine add <name> <ssh-target>"));
        seat(&mut out);
        under(&mut out, agents_on(&here, ""));
        return out;
    }

    let w = f.machines.iter().map(|m| m.name.len()).max().unwrap_or(4).max(4);
    let ssh_w = f.machines.iter().map(|m| m.ssh.len()).max().unwrap_or(3).max(3);
    out.push(p.dim(&format!("{:<w$}  {:<ssh_w$}  {}", "NAME", "SSH", "STATE")));
    for m in &f.machines {
        let (state, mut note) = state_of(p, m, f.live.get(&m.name));
        // Why, not just that — but cut, because an ssh failure is a sentence
        // and a half and this is one cell of a list. `show` prints it whole,
        // which is where you go when forty characters were not enough.
        if let Some(err) = f.live.get(&m.name).map(|l| l.error.as_str()).filter(|e| !e.is_empty()) {
            note = format!("{note} · {}", util::truncate(err, 44));
        }
        out.push(format!(
            "{}  {}  {}{}",
            p.bold(&format!("{:<w$}", m.name)),
            p.dim(&format!("{:<ssh_w$}", m.ssh)),
            state,
            if note.is_empty() { String::new() } else { format!("  {}", p.dim(&note)) },
        ));
        under(&mut out, agents_on(&here, &m.name));
    }
    seat(&mut out);
    under(&mut out, agents_on(&here, ""));
    out
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
    // A record written by hand can leave this out, and then what the tunnel
    // will actually forward to is the backend's own default. Printed as such
    // rather than left blank: this is the field a machine that will not come up
    // is usually wrong about, and a blank line here would send you looking for
    // the answer somewhere there isn't one.
    match m.backend_at.is_empty() {
        true => println!("backend   {}", p.dim(&format!("{} — the backend's default", place_herdr::mirrored_socket()))),
        false => println!("backend   {}", m.backend_at),
    }
    let box_line = [m.os.as_str(), m.arch.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
    if !box_line.is_empty() {
        println!("box       {box_line}");
    }
    // Printed whether or not it is set, because "no cap" is the state this
    // knob is in on every machine nobody has thought about, and a line that
    // only appears once somebody has thought about it is a line that never
    // tells you it is there. See `Machine::agents` for why the number is here.
    match m.agents {
        Some(n) => println!("agents    {n} at once"),
        None => println!("agents    {}", p.dim(&format!("no cap — wsp machine set {} agents=4", m.name))),
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
        eprintln!("usage: wsp machine set <name> ssh=… backend_at=… os=… arch=… agents=N status=active|retired");
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
            "backend_at" => m.backend_at = v.to_string(),
            // One release of a name nobody but this repository ever typed, kept
            // because it is in the README's executor section and the error it
            // would otherwise get names every field except the one you meant.
            "herdr_sock" => {
                eprintln!("wsp: `herdr_sock` is now `backend_at` — a machine records where its backend listens, whichever backend that is");
                return 2;
            }
            "os" => m.os = v.to_string(),
            "arch" => m.arch = v.to_string(),
            // How many agents this box will bear at once. The argument for it
            // being a fact about the machine rather than about the list that
            // wants it is on `Machine::agents`, and so is what it does not
            // cover — it counts agents, and `data-018` measured that builds
            // are what saturate.
            "agents" => match v.trim() {
                // `agents=` is how a number is taken back off. Absent is a
                // real state and not a zero: it means nobody has decided,
                // which is where every machine starts.
                "" | "none" | "-" => m.agents = None,
                // Rejected rather than read as either of its two meanings.
                // "drain this machine" is a plausible reading and so is "no
                // cap", and a machine silently taking one of them is how you
                // find out at 3am which one this build chose. Retiring is how
                // a machine is taken out of use, and it is already a verb.
                "0" => {
                    eprintln!("wsp: agents=0 would mean either `no cap` or `drain it`, so it means neither — `agents=` clears the cap, `wsp machine rm {}` retires the machine", m.name);
                    return 2;
                }
                n => match n.parse::<usize>() {
                    Ok(n) => m.agents = Some(n),
                    Err(_) => {
                        eprintln!("wsp: agents is a count, not `{n}`");
                        return 2;
                    }
                },
            },
            "status" => match v {
                "active" | "retired" => m.status = v.to_string(),
                _ => {
                    eprintln!("wsp: status is `active` or `retired`, not `{v}`");
                    return 2;
                }
            },
            other => {
                eprintln!("wsp: machines have no `{other}` — ssh, backend_at, os, arch, agents, status");
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

    // ---- the whole view, offline -----------------------------------------

    fn agent(pane: &str, agent: &str, state: &str, title: &str) -> herdr::Pane {
        herdr::Pane {
            pane_id: pane.to_string(),
            agent: agent.to_string(),
            agent_status: state.to_string(),
            title: title.to_string(),
            ..Default::default()
        }
    }

    /// A fleet in all three states at once, with agents on two of them.
    ///
    /// This is the frame nobody can arrange: it needs a machine the daemon has
    /// never reported on, a second that answered and stopped, a third
    /// answering, and agents standing on two boxes — one of them somebody
    /// else's. Buildable here because the view takes its inputs in.
    fn fleet() -> Fleet {
        let mut live = BTreeMap::new();
        live.insert(
            "mb2".to_string(),
            MachineLive { reachable: true, herdr_version: "0.4.1".into(), ..Default::default() },
        );
        live.insert(
            "rack".to_string(),
            MachineLive {
                reachable: false,
                last_seen: util::now_iso(),
                error: "ssh: connect to host rack port 22: Connection refused".into(),
                ..Default::default()
            },
        );

        let mut bindings = BTreeMap::new();
        bindings.insert("w1:p2@mb2".to_string(), json!({ "task_id": "t-001" }));

        Fleet {
            machines: vec![
                Machine::new("mb2", "mb2"),
                Machine::new("rack", "rack"),
                // No entry in `live`: added, and the daemon has not been round
                // to it yet.
                Machine::new("shed", "shed.local"),
            ],
            live,
            agents: vec![
                agent("w1:p2@mb2", "claude", "working", "renaming the thing"),
                agent("w3:p1@mb2", "", "idle", "zsh"),
                agent("w0:p6", "claude", "idle", "reading sync.rs"),
            ],
            bindings,
            tasks: vec![crate::model::Task::new("lift the machines view off herdr", "t-001")],
            seat: "seat-under-test".into(),
        }
    }

    /// The list renders with no daemon, no tunnel and no second box — which is
    /// the point of taking the inputs in, and was not possible before.
    ///
    /// Asserted on the shape rather than the exact text: what matters is that
    /// every machine is on it, each agent lands under the box it is standing
    /// on, and the seat's own agents are not attributed to a machine.
    #[test]
    fn the_machines_view_renders_with_nothing_running() {
        let out = list_lines(&fleet(), &plain());
        let text = out.join("\n");

        for name in ["mb2", "rack", "shed"] {
            assert!(out.iter().any(|l| l.starts_with(name)), "{name} is not on the list:\n{text}");
        }
        assert!(text.contains("seat  seat-under-test"), "the seat is a heading of its own:\n{text}");

        // Each agent under its own machine: `@mb2` on the id is where it is,
        // and an unqualified id is this seat.
        let row = |needle: &str| out.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no row for {needle}:\n{text}"));
        assert!(row("w1:p2@mb2") > row("mb2"), "an agent sits under its machine");
        assert!(row("w3:p1@mb2") < row("rack"), "…and above the next one");
        assert!(row("w0:p6") > row("seat"), "an unqualified pane is on the seat");

        // What it is holding, and the two fallbacks for a pane holding nothing.
        assert!(text.contains("lift the machines view off herdr"), "a bound pane shows its task:\n{text}");
        assert!(text.contains("(zsh)"), "an unbound pane falls back to its title:\n{text}");
        assert!(text.contains("a shell"), "a pane with no agent is a shell:\n{text}");

        // And the three states, in one frame, distinct.
        assert!(text.contains("connected"), "{text}");
        assert!(text.contains("offline"), "{text}");
        assert!(text.contains("no daemon report yet"), "{text}");
        // Why, and cut: an ssh failure is a sentence and a half, and this is
        // one cell of a list. `show` is where the whole of it is.
        assert!(text.contains("ssh: connect to host rack"), "an offline row says why:\n{text}");
        assert!(!text.contains("Connection refused"), "…and no more than a cell of it:\n{text}");
    }

    /// herdr not answering costs the list its agents and nothing else. This is
    /// the distinction the whole executor design is built around — "unreachable"
    /// is not "answering with nothing" — and the durable half of a machine is a
    /// file, which does not need a socket to be read.
    #[test]
    fn no_herdr_is_a_machines_list_without_the_agents_on_it() {
        let mut f = fleet();
        f.agents.clear();
        let out = list_lines(&f, &plain());

        for name in ["mb2", "rack", "shed"] {
            assert!(out.iter().any(|l| l.starts_with(name)), "{name} went with the socket");
        }
        assert!(out.iter().any(|l| l.contains("seat-under-test")), "so did the seat");
        assert!(!out.join("\n").contains("w1:p2"), "no agents, since none were reported");
    }

    // ---- the cap --------------------------------------------------------

    fn scratch(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    /// Setting it, clearing it, and the one value that is refused.
    ///
    /// `agents=0` is rejected rather than stored because it has two readings —
    /// *no limit* and *run nothing here* — and a machine that quietly picks
    /// one of them is a night that either did not start or did not stop. The
    /// error has to name both ways out, since a refusal that only says no is
    /// how somebody ends up writing the number down somewhere else.
    #[test]
    fn a_machine_takes_an_agent_cap_and_gives_it_back_but_will_not_take_zero() {
        let store = scratch("machine-agents");
        store.save_machine(&Machine::new("mb2", "mb2")).unwrap();
        let set_to = |v: &str| set(&store, &Args::synth("machine", &["set", "mb2", &format!("agents={v}")], &[]));

        assert_eq!(set_to("4"), 0);
        assert_eq!(store.machine("mb2").unwrap().agents, Some(4), "and it is on the file");

        assert_eq!(set_to("0"), 2, "zero is refused rather than guessed at");
        assert_eq!(store.machine("mb2").unwrap().agents, Some(4), "and changes nothing");

        assert_eq!(set_to("four"), 2, "a count is a count");
        assert_eq!(set_to(""), 0, "`agents=` is how the number is taken back off");
        assert_eq!(store.machine("mb2").unwrap().agents, None, "back to nobody having decided");
    }

    /// No machines at all is the common case — one seat, and whatever is on it
    /// — and it must still say what is running here.
    #[test]
    fn a_seat_with_no_machines_still_lists_its_own_agents() {
        let mut f = fleet();
        f.machines.clear();
        let text = list_lines(&f, &plain()).join("\n");
        assert!(text.contains("no machines"), "{text}");
        assert!(text.contains("wsp machine add"), "and what to do about it:\n{text}");
        assert!(text.contains("w0:p6"), "the seat's own agent is still the point:\n{text}");
        // The panes on machines that are gone from the list go with them.
        assert!(!text.contains("w1:p2@mb2"), "an agent on no listed machine is not the seat's:\n{text}");
    }
}

fn machine_json(m: &Machine, live: Option<&MachineLive>) -> serde_json::Value {
    json!({
        "name": m.name,
        "ssh": m.ssh,
        "os": m.os,
        "arch": m.arch,
        "backend_at": m.backend_at,
        // Null and not 0 when there is no cap: a governor asking for the
        // number wants to be able to tell "nobody has decided" from a number,
        // and `0` is the one answer that reads as an instruction.
        "agents": m.agents,
        "status": m.status,
        "added": m.added,
        "live": live.map(|l| l.to_value()),
    })
}
