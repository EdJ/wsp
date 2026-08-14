//! wsp — workspace and task control plane for herdr.
//!
//! Durable facts (projects, tags, tasks) live in `~/wsp` as Markdown + git.
//! Live facts (panes, agent status) come from herdr's socket. This binary
//! joins them, and pushes the join back into herdr's sidebar as metadata
//! tokens.

use std::collections::HashMap;

mod cmd_agent;
mod cmd_project;
mod cmd_task;
mod daemon;
mod fm;
mod herdr;
mod model;
mod panel;
mod resolve;
mod story;
mod store;
mod sync;
mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Flags that never consume the following token.
const BOOL_FLAGS: &[&str] = &[
    "json", "all", "force", "verbose", "quiet", "yes", "clear", "tree", "inbox", "open", "done",
    "help", "version", "no-commit", "closed", "here",
];

pub struct Args {
    pub cmd: String,
    pub rest: Vec<String>,
    flags: HashMap<String, Vec<String>>,
}

impl Args {
    fn parse(argv: Vec<String>) -> Args {
        let mut positional: Vec<String> = Vec::new();
        let mut flags: HashMap<String, Vec<String>> = HashMap::new();
        let mut i = 0;
        while i < argv.len() {
            let a = argv[i].clone();
            if let Some(body) = a.strip_prefix("--") {
                if body.is_empty() {
                    // `--` ends flag parsing
                    positional.extend(argv[i + 1..].iter().cloned());
                    break;
                }
                let (name, inline) = match body.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (body.to_string(), None),
                };
                let entry = flags.entry(name.clone()).or_default();
                if let Some(v) = inline {
                    entry.push(v);
                } else if BOOL_FLAGS.contains(&name.as_str()) {
                    entry.push("true".into());
                } else if i + 1 < argv.len() && !argv[i + 1].starts_with('-') {
                    entry.push(argv[i + 1].clone());
                    i += 1;
                } else {
                    entry.push("true".into());
                }
            } else if a.len() >= 2 && a.starts_with('-') && !a[1..].starts_with(|c: char| c.is_ascii_digit()) {
                let name = expand_short(&a[1..]);
                let entry = flags.entry(name.clone()).or_default();
                if BOOL_FLAGS.contains(&name.as_str()) {
                    entry.push("true".into());
                } else if i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
                    entry.push(argv[i + 1].clone());
                    i += 1;
                } else {
                    entry.push("true".into());
                }
            } else {
                positional.push(a);
            }
            i += 1;
        }

        let cmd = if positional.is_empty() { String::new() } else { positional.remove(0) };
        Args { cmd, rest: positional, flags }
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }
    pub fn get(&self, name: &str) -> Option<String> {
        self.flags.get(name).and_then(|v| v.first().cloned())
    }
    pub fn all(&self, name: &str) -> Vec<String> {
        self.flags
            .get(name)
            .map(|v| {
                v.iter()
                    .flat_map(|s| s.split(',').map(|x| x.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn json(&self) -> bool {
        self.has("json")
    }
    /// Remaining positionals joined — titles, notes, reasons.
    pub fn text(&self, from: usize) -> String {
        self.rest.iter().skip(from).cloned().collect::<Vec<_>>().join(" ")
    }
}

fn expand_short(s: &str) -> String {
    match s {
        "p" => "project".into(),
        "t" => "tag".into(),
        "s" => "status".into(),
        "a" => "all".into(),
        "v" => "verbose".into(),
        "j" => "json".into(),
        "w" => "workspace".into(),
        other => other.to_string(),
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = Args::parse(argv);

    if args.has("version") || args.cmd == "version" {
        println!("wsp {VERSION}");
        return;
    }
    if args.cmd.is_empty() || args.cmd == "help" || args.has("help") {
        help();
        return;
    }

    if args.has("no-commit") {
        std::env::set_var("WSP_NO_COMMIT", "1");
    }

    let store = store::Store::open();
    if !store.exists() && args.cmd != "init" && args.cmd != "doctor" {
        eprintln!(
            "wsp: no store at {}. Run `wsp init` first.",
            util::contract(&store.root)
        );
        std::process::exit(2);
    }

    let code = match args.cmd.as_str() {
        "init" => cmd_project::init(&store, &args),

        "project" | "proj" | "p" => cmd_project::dispatch(&store, &args),
        "projects" => cmd_project::list(&store, &args),
        "tree" => cmd_project::tree(&store, &args),

        "add" | "new" => cmd_task::add(&store, &args),
        "ls" | "list" => cmd_task::list(&store, &args),
        "inbox" => cmd_task::inbox(&store, &args),
        "show" | "cat" => cmd_task::show(&store, &args),
        "note" => cmd_task::note(&store, &args),
        "start" | "doing" => cmd_task::set_status(&store, &args, model::Status::Doing),
        "done" | "close" => cmd_task::done(&store, &args),
        "block" => cmd_task::block(&store, &args),
        "review" => cmd_task::set_status(&store, &args, model::Status::Review),
        "reopen" | "todo" => cmd_task::set_status(&store, &args, model::Status::Todo),
        "mv" | "move" => cmd_task::mv(&store, &args),
        "tag" => cmd_task::tag(&store, &args),
        "next" => cmd_task::next(&store, &args),
        "edit" => cmd_task::edit(&store, &args),
        "rename" => cmd_task::rename(&store, &args),
        "rm" | "remove" => cmd_task::rm(&store, &args),
        "archive" => cmd_task::archive(&store, &args),

        "claim" => cmd_agent::claim(&store, &args),
        "release" => cmd_agent::release(&store, &args),
        "pin" => cmd_agent::pin(&store, &args),
        "unpin" => cmd_agent::unpin(&store, &args),
        "where" => cmd_agent::where_am_i(&store, &args),
        "wip" | "status" => cmd_agent::wip(&store, &args),
        "sync" => cmd_agent::sync_once(&store, &args),
        "hook" => cmd_agent::hook(&store, &args),
        "doctor" => cmd_agent::doctor(&store, &args),
        "daemon" => daemon::run(&store, args.has("verbose")),
        "panel" => match args.rest.first().map(|s| s.as_str()) {
            Some("install") => panel::install(&store, &args),
            Some("uninstall" | "remove") => panel::uninstall(&store, &args),
            Some("storyboard") => story::run(&args),
            _ => panel::run(&store),
        },

        other => {
            eprintln!("wsp: unknown command `{other}`. Try `wsp help`.");
            2
        }
    };
    std::process::exit(code);
}

fn help() {
    let p = util::Paint::new();
    let h = |s: &str| p.bold(s);
    println!(
        r#"{name} {VERSION} — workspace and task control plane for herdr

{projects}
  wsp init                          create the store at ~/wsp
  wsp project add <slug> [--name N] [--parent P] [--tag T]… [--root PATH]…
  wsp project ls [--tag T]          list projects
  wsp tree                          hierarchy with open counts
  wsp project show <id>             brief, tags, roots, tasks, agents
  wsp project set <id> k=v…         name/parent/status/brief/tags/roots
  wsp project rm <id> [--force]     remove; --force orphans what it held

{tasks}
  wsp add "title" [-p proj] [-t tag]… [--prio high] [--ref PATH]
  wsp ls [-p proj] [-t tag] [-s status] [--all]
  wsp inbox                         tasks with no project
  wsp show <id>                     full task, including notes
  wsp start|review|reopen <id>      move through the workflow
  wsp done <id>                     complete
  wsp block <id> "reason"           park it, and say why
  wsp note <id> "text"              append to the log
  wsp mv <id> -p proj               reassign
  wsp tag <id> +dsp -ui             adjust tags
  wsp next [-p proj]                highest-priority actionable task
  wsp archive [--all]               sweep done tasks older than 30d

{agents}
  wsp claim <id>                    bind this pane to a task
  wsp release                       unbind this pane
  wsp pin <proj> [-w ws]            pin a workspace to a project
  wsp where                         what project am I in, and why
  wsp wip                           everything in flight, with agents

{plumbing}
  wsp panel                         the sidebar replacement (runs in a pane)
  wsp panel install [--all]         split it into a workspace, or all of them
  wsp panel uninstall [-w ws]       take it back out
  wsp sync [--force]                push tokens to herdr once
  wsp daemon [-v]                   events + refresh loop (herdr [[startup]])
  wsp hook <event>                  herdr event-hook entrypoint
  wsp doctor                        integrity check

Ids accept a bare suffix (003) or a unique title substring.
Every command takes --json. Set WSP_HOME to relocate the store."#,
        name = h("wsp"),
        projects = h("PROJECTS"),
        tasks = h("TASKS"),
        agents = h("AGENTS"),
        plumbing = h("PLUMBING"),
    );
}
