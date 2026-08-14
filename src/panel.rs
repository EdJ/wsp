//! `wsp panel` — the sidebar replacement.
//!
//! herdr's sidebar lists workspaces (really: open folders) and, beneath them,
//! agents — a view that falls out of how herdr is built rather than how work is
//! organised. This panel inverts that: the spine is the project tree, tasks
//! hang off projects, and agents attach to tasks. A workspace is only ever a
//! destination, never a heading.
//!
//! It keeps the two things herdr's sidebar is genuinely good at — live agent
//! status and jumping to a terminal — by subscribing to the same event stream
//! and calling `workspace.focus`/`pane.focus` on Enter.
//!
//! No TUI crate: we own one pane, so a string buffer plus ANSI is enough, and
//! the dependency list stays at `serde_json`.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::herdr;
use crate::model::{Status, Task};
use crate::resolve::{self, Counts, Index};
use crate::store::Store;
use crate::util;

const ACCENT: &str = "\x1b[38;2;95;191;164m";
const WARN: &str = "\x1b[38;2;224;138;75m";
const MUTED: &str = "\x1b[38;2;125;140;150m";
const DIMC: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const INV: &str = "\x1b[7m";
const OFF: &str = "\x1b[0m";

const MAX_TASKS_PER_PROJECT: usize = 6;

/// Stands in for "no project" wherever a project id is used as a key. Project
/// ids are slugs, so the parentheses keep it out of their namespace.
const INBOX_KEY: &str = "(inbox)";

/// What the viewer has folded, unfolded, or asked to see more of. Held by the
/// event loop and handed to `collect`, which is otherwise a pure function of
/// the store plus herdr.
#[derive(Default)]
struct View {
    /// Projects whose children and tasks are hidden.
    collapsed: HashSet<String>,
    /// Projects (or the inbox) showing past `MAX_TASKS_PER_PROJECT`.
    expanded: HashSet<String>,
    /// Include `done` tasks, and the projects that hold only those.
    show_done: bool,
}

enum Msg {
    Key(Key),
    /// Carries the workspace the event was about, when it named one.
    Herdr(Option<String>),
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Key {
    Up,
    Down,
    Enter,
    Left,
    Right,
    Digit(u8),
    Char(char),
    Quit,
}

#[derive(Debug, Clone)]
struct AgentRef {
    pane: String,
    workspace: String,
    state: String,
    where_: String,
}

#[derive(Debug, Clone)]
enum Row {
    Project { id: String, depth: usize, counts: Counts, collapsed: bool, live: usize },
    Task {
        /// Carried so a row can name the task it stands for. Nothing acts on a
        /// task yet; this is what the edit keys will dispatch on.
        #[allow(dead_code)]
        id: String,
        title: String,
        depth: usize,
        status: Status,
        agent: Option<AgentRef>,
        needs_you: bool,
    },
    /// `key` is the project the hidden tasks belong to, or `INBOX_KEY`.
    More { key: String, depth: usize, n: usize },
    Section { label: String },
    Agent { agent: AgentRef, title: String },
}

impl Row {
    fn selectable(&self) -> bool {
        !matches!(self, Row::Section { .. })
    }
    fn agent(&self) -> Option<&AgentRef> {
        match self {
            Row::Task { agent: Some(a), .. } => Some(a),
            Row::Agent { agent, .. } => Some(agent),
            _ => None,
        }
    }
}

struct Ui {
    rows: Vec<Row>,
    agents_total: usize,
    needs: usize,
    inbox: usize,
    blocked: usize,
    sel: usize,
    message: Option<(String, Instant)>,
    self_focused: bool,
    show_done: bool,
}

// ---- data ---------------------------------------------------------------

fn task_sort_key(t: &Task, has_agent: bool, needs_you: bool) -> (u8, u8, u8, String) {
    (
        u8::from(!needs_you),
        u8::from(!has_agent),
        t.status().rank(),
        t.id.clone(),
    )
}

/// Rows for the tasks of one project — or, when `project` is `None`, the tasks
/// belonging to no project at all. The inbox went unrendered for a while
/// because it had no equivalent of the project walk; sharing this makes that
/// class of omission impossible.
fn task_rows(
    tasks: &[Task],
    project: Option<&str>,
    depth: usize,
    view: &View,
    agent_for_task: &dyn Fn(&str) -> Option<AgentRef>,
    rows: &mut Vec<Row>,
    needs: &mut usize,
) {
    let mut mine: Vec<(&Task, Option<AgentRef>, bool)> = tasks
        .iter()
        .filter(|t| t.project.as_deref() == project)
        .filter(|t| view.show_done || t.status().is_open())
        .map(|t| {
            let a = agent_for_task(&t.id);
            let needs_you =
                a.as_ref().map(|a| a.state == "idle").unwrap_or(false) && t.status() == Status::Doing;
            (t, a, needs_you)
        })
        .collect();
    mine.sort_by_key(|(t, a, n)| task_sort_key(t, a.is_some(), *n));

    let key = project.unwrap_or(INBOX_KEY);
    let shown = if view.expanded.contains(key) {
        mine.len()
    } else {
        mine.len().min(MAX_TASKS_PER_PROJECT)
    };

    for (t, a, needs_you) in mine.iter().take(shown) {
        if *needs_you {
            *needs += 1;
        }
        rows.push(Row::Task {
            id: t.id.clone(),
            title: t.title.clone(),
            depth,
            status: t.status(),
            agent: a.clone(),
            needs_you: *needs_you,
        });
    }
    if mine.len() > shown {
        rows.push(Row::More { key: key.to_string(), depth, n: mine.len() - shown });
    }
}

fn collect(store: &Store, view: &View, self_ws: Option<&str>) -> Ui {
    let index = Index::new(store.projects());
    let tasks = store.tasks();
    let counts = resolve::counts_by_project(&index, &tasks);
    let bindings = store.bindings();
    let pins = store.pins();

    let workspaces = herdr::workspaces().unwrap_or_default();
    let agents = herdr::agents().unwrap_or_default();
    let self_focused = self_ws
        .and_then(|id| workspaces.iter().find(|w| w.id == id))
        .map(|w| w.focused)
        .unwrap_or(true);

    let ws_label = |id: &str| -> String {
        workspaces.iter().find(|w| w.id == id).map(|w| w.label.clone()).unwrap_or_default()
    };

    // pane -> task id
    let bound_task_of_pane = |pane: &str| -> Option<String> {
        bindings
            .get(pane)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
    };
    // task id -> the agent working it
    let agent_for_task = |task_id: &str| -> Option<AgentRef> {
        agents.iter().find_map(|a| {
            if bound_task_of_pane(&a.pane_id).as_deref() == Some(task_id) {
                Some(AgentRef {
                    pane: a.pane_id.clone(),
                    workspace: a.workspace_id.clone(),
                    state: a.agent_status.clone(),
                    where_: ws_label(&a.workspace_id),
                })
            } else {
                None
            }
        })
    };

    // Which projects are worth showing: any with open tasks, or a live agent.
    let mut live_by_project: std::collections::BTreeMap<String, usize> = Default::default();
    for a in &agents {
        let bound_project = bound_task_of_pane(&a.pane_id)
            .and_then(|id| tasks.iter().find(|t| t.id == id))
            .and_then(|t| t.project.clone());
        let r = resolve::resolve(
            &index,
            &pins,
            bound_project,
            Some(&a.workspace_id),
            Some(&ws_label(&a.workspace_id)),
            Some(&a.cwd),
        );
        if let Some(p) = r.project {
            for id in std::iter::once(p.clone()).chain(index.ancestors(&p)) {
                *live_by_project.entry(id).or_insert(0) += 1;
            }
        }
    }

    // Quiet branches stay folded away, but a project holding only finished work
    // must reappear the moment `show_done` is on — otherwise the toggle looks
    // broken on exactly the projects it exists to reveal.
    let interesting = |id: &str| -> bool {
        let c = counts.get(id).copied().unwrap_or_default();
        c.open > 0 || live_by_project.contains_key(id) || (view.show_done && c.done > 0)
    };

    let mut rows: Vec<Row> = Vec::new();
    let mut needs = 0;

    // Depth-first over the project tree, skipping quiet branches.
    fn walk(
        index: &Index,
        parent: Option<&str>,
        depth: usize,
        rows: &mut Vec<Row>,
        counts: &std::collections::BTreeMap<String, Counts>,
        live: &std::collections::BTreeMap<String, usize>,
        view: &View,
        tasks: &[Task],
        interesting: &dyn Fn(&str) -> bool,
        agent_for_task: &dyn Fn(&str) -> Option<AgentRef>,
        needs: &mut usize,
    ) {
        for p in index.children(parent) {
            if !interesting(&p.id) {
                continue;
            }
            let is_collapsed = view.collapsed.contains(&p.id);
            rows.push(Row::Project {
                id: p.id.clone(),
                depth,
                counts: counts.get(&p.id).copied().unwrap_or_default(),
                collapsed: is_collapsed,
                live: live.get(&p.id).copied().unwrap_or(0),
            });
            if is_collapsed {
                continue;
            }

            // This project's own tasks, attention first.
            task_rows(tasks, Some(&p.id), depth + 1, view, agent_for_task, rows, needs);

            walk(
                index, Some(&p.id), depth + 1, rows, counts, live, view, tasks, interesting,
                agent_for_task, needs,
            );
        }
    }

    walk(
        &index,
        None,
        0,
        &mut rows,
        &counts,
        &live_by_project,
        view,
        &tasks,
        &interesting,
        &agent_for_task,
        &mut needs,
    );

    // Unparented tasks. These are real work with a real id — they just have
    // nowhere in the tree to hang, so they get their own heading rather than
    // living on as a number in the footer.
    let inbox_open =
        tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count();
    let inbox_any = tasks
        .iter()
        .any(|t| t.project.is_none() && (view.show_done || t.status().is_open()));
    if inbox_any {
        rows.push(Row::Section { label: format!("inbox  {inbox_open}") });
        task_rows(&tasks, None, 1, view, &agent_for_task, &mut rows, &mut needs);
    }

    // Agents not attached to any task — the panel's other job is making these
    // visible, because they are the ones nobody has decided about.
    let unattached: Vec<&herdr::Agent> = agents
        .iter()
        .filter(|a| bound_task_of_pane(&a.pane_id).is_none())
        .collect();
    if !unattached.is_empty() {
        rows.push(Row::Section { label: format!("unattached  {}", unattached.len()) });
        for a in unattached {
            rows.push(Row::Agent {
                agent: AgentRef {
                    pane: a.pane_id.clone(),
                    workspace: a.workspace_id.clone(),
                    state: a.agent_status.clone(),
                    where_: ws_label(&a.workspace_id),
                },
                title: if a.title.is_empty() { ws_label(&a.workspace_id) } else { a.title.clone() },
            });
        }
    }

    Ui {
        rows,
        agents_total: agents.len(),
        needs,
        inbox: tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count(),
        blocked: tasks.iter().filter(|t| t.status() == Status::Blocked).count(),
        sel: 0,
        message: None,
        self_focused,
        show_done: view.show_done,
    }
}

// ---- rendering ----------------------------------------------------------


fn vis_len(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            if c == 'm' {
                esc = false;
            }
        } else if c == '\x1b' {
            esc = true;
        } else {
            n += 1;
        }
    }
    n
}

fn fitline(s: &str, w: usize) -> String {
    let len = vis_len(s);
    if len < w {
        format!("{s}{}", " ".repeat(w - len))
    } else {
        s.to_string()
    }
}

fn state_icon(state: &str) -> String {
    match state {
        "working" => format!("{ACCENT}●{OFF}"),
        "idle" => format!("{MUTED}○{OFF}"),
        _ => format!("{DIMC}·{OFF}"),
    }
}

fn render_row(row: &Row, w: usize, num: Option<u8>) -> String {
    match row {
        Row::Project { id, depth, counts, collapsed, live } => {
            let indent = " ".repeat(*depth);
            let caret = if *collapsed { "▸" } else { "▾" };
            let left = format!("{indent}{DIMC}{caret}{OFF} {BOLD}{id}{OFF}");

            // A done-ratio bar reads as empty until work starts landing, so the
            // right-hand column carries live workload instead: open, in flight,
            // blocked.
            let mut parts: Vec<String> = Vec::new();
            if counts.open > 0 {
                parts.push(format!("{DIMC}{}{OFF}", counts.open));
            }
            if counts.doing > 0 {
                parts.push(format!("{ACCENT}▸{}{OFF}", counts.doing));
            }
            if counts.blocked > 0 {
                parts.push(format!("{WARN}■{}{OFF}", counts.blocked));
            }
            if counts.done > 0 && counts.open == 0 {
                parts.push(format!("{DIMC}✓{OFF}"));
            }
            if *live > 0 {
                parts.push(format!("{ACCENT}●{live}{OFF}"));
            }
            let right = parts.join(" ");
            let gap = w.saturating_sub(vis_len(&left) + vis_len(&right)).max(1);
            format!("{left}{}{right}", " ".repeat(gap))
        }
        Row::Task { title, depth, status, agent, needs_you, .. } => {
            let indent = " ".repeat(*depth);
            let lead = match agent {
                Some(a) => state_icon(&a.state),
                None => match status {
                    Status::Blocked => format!("{WARN}■{OFF}"),
                    Status::Review => format!("{MUTED}◆{OFF}"),
                    Status::Done => format!("{DIMC}✓{OFF}"),
                    _ => format!("{DIMC}·{OFF}"),
                },
            };
            let key = match num {
                Some(n) => format!("{DIMC}{n}{OFF}"),
                None => " ".into(),
            };
            let flag = if *needs_you { format!(" {WARN}←{OFF}") } else { String::new() };
            let avail = w.saturating_sub(vis_len(&indent) + 5 + vis_len(&flag));
            let body = util::truncate(title, avail.max(4));
            let colour = if *needs_you {
                WARN
            } else if *status == Status::Done {
                DIMC
            } else if agent.is_some() {
                ""
            } else {
                MUTED
            };
            format!("{key}{indent} {lead} {colour}{body}{OFF}{flag}")
        }
        Row::More { depth, n, .. } => {
            let indent = " ".repeat(*depth);
            format!(" {indent} {DIMC}⋯{OFF} {MUTED}{n} more{OFF}")
        }
        Row::Section { label } => format!("{DIMC}{label}{OFF}"),
        Row::Agent { agent, title } => {
            let key = match num {
                Some(n) => format!("{DIMC}{n}{OFF}"),
                None => " ".into(),
            };
            let icon = state_icon(&agent.state);
            let left = format!("{key} {icon} {MUTED}{}{OFF}", util::truncate(title, w.saturating_sub(6).max(4)));
            let right = format!("{DIMC}{}{OFF}", util::truncate(&agent.where_, 10));
            let gap = w.saturating_sub(vis_len(&left) + vis_len(&right));
            if gap >= 2 {
                format!("{left}{}{right}", " ".repeat(gap))
            } else {
                left
            }
        }
    }
}

/// Digits 1-9 address rows that lead somewhere: a terminal.
fn hotkeys(rows: &[Row]) -> Vec<Option<u8>> {
    let mut out = Vec::with_capacity(rows.len());
    let mut n: u8 = 0;
    for r in rows {
        if r.agent().is_some() && n < 9 {
            n += 1;
            out.push(Some(n));
        } else {
            out.push(None);
        }
    }
    out
}

fn render(ui: &Ui, w: usize, h: usize) -> String {
    let mut lines: Vec<String> = Vec::new();

    let needs = if ui.needs > 0 {
        format!("{WARN}{} ←{OFF}", ui.needs)
    } else {
        format!("{DIMC}·{OFF}")
    };
    lines.push(format!(
        "{BOLD}wsp{OFF} {DIMC}·{OFF} {} {DIMC}agents ·{OFF} {}",
        ui.agents_total, needs
    ));
    lines.push(format!("{DIMC}{}{OFF}", "─".repeat(w)));

    let footer_rows = 3;
    let body_rows = h.saturating_sub(lines.len() + footer_rows);
    let keys = hotkeys(&ui.rows);

    // Keep the selection in view.
    let mut scroll = 0usize;
    if body_rows > 0 && ui.sel >= body_rows {
        scroll = ui.sel + 1 - body_rows;
    }

    for (i, row) in ui.rows.iter().enumerate().skip(scroll).take(body_rows) {
        let text = render_row(row, w, keys[i]);
        if i == ui.sel {
            lines.push(format!("{INV}{}{OFF}", fitline(&text, w)));
        } else {
            lines.push(text);
        }
    }
    while lines.len() < h.saturating_sub(footer_rows) {
        lines.push(String::new());
    }

    lines.push(format!("{DIMC}{}{OFF}", "─".repeat(w)));
    let blocked = if ui.blocked > 0 {
        format!("{WARN}blocked {}{OFF}", ui.blocked)
    } else {
        format!("{DIMC}blocked 0{OFF}")
    };
    let done = if ui.show_done { format!("  {ACCENT}+done{OFF}") } else { String::new() };
    lines.push(format!("{DIMC}inbox{OFF} {}  {}{}", ui.inbox, blocked, done));
    match &ui.message {
        Some((m, at)) if at.elapsed() < Duration::from_secs(4) => {
            lines.push(format!("{ACCENT}{}{OFF}", util::truncate(m, w)))
        }
        _ => lines.push(format!("{DIMC}1-9 go  ↵ open  ←→ fold  A done  q{OFF}")),
    }

    let mut out = String::from("\x1b[H\x1b[2J");
    for (i, l) in lines.iter().take(h).enumerate() {
        out.push_str(&format!("\x1b[{};1H", i + 1));
        out.push_str(&fitline(l, w));
    }
    out
}

// ---- terminal -----------------------------------------------------------

fn stty(args: &[&str]) {
    if let Ok(tty) = File::open("/dev/tty") {
        let _ = Command::new("stty")
            .args(args)
            .stdin(Stdio::from(tty))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn term_size() -> (usize, usize) {
    if let Ok(tty) = File::open("/dev/tty") {
        if let Ok(out) = Command::new("stty").arg("size").stdin(Stdio::from(tty)).output() {
            let s = String::from_utf8_lossy(&out.stdout);
            let mut it = s.split_whitespace();
            if let (Some(r), Some(c)) = (it.next(), it.next()) {
                if let (Ok(r), Ok(c)) = (r.parse::<usize>(), c.parse::<usize>()) {
                    return (c.max(16), r.max(6));
                }
            }
        }
    }
    (26, 40)
}

fn spawn_input(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let Ok(mut tty) = File::open("/dev/tty") else { return };
        let mut buf = [0u8; 1];
        loop {
            match tty.read(&mut buf) {
                Ok(1) => {
                    let key = match buf[0] {
                        b'q' | 3 => Key::Quit, // raw mode delivers ctrl-c as a byte
                        b'j' => Key::Down,
                        b'k' => Key::Up,
                        b'h' => Key::Left,
                        b'l' => Key::Right,
                        b'\r' | b'\n' => Key::Enter,
                        b'\x1b' => {
                            let mut seq = [0u8; 2];
                            if tty.read(&mut seq[..1]).is_ok()
                                && seq[0] == b'['
                                && tty.read(&mut seq[1..]).is_ok()
                            {
                                match seq[1] {
                                    b'A' => Key::Up,
                                    b'B' => Key::Down,
                                    b'C' => Key::Right,
                                    b'D' => Key::Left,
                                    _ => continue,
                                }
                            } else {
                                continue;
                            }
                        }
                        c if c.is_ascii_digit() && c != b'0' => Key::Digit(c - b'0'),
                        c => Key::Char(c as char),
                    };
                    if tx.send(Msg::Key(key)).is_err() {
                        return;
                    }
                }
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    });
}

fn spawn_events(tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        let tx2 = tx.clone();
        let res = herdr::subscribe(
            &[
                "workspace.created",
                "workspace.closed",
                "workspace.renamed",
                "workspace.focused",
                "pane.created",
                "pane.exited",
                "pane.agent_status_changed",
            ],
            move |_e, d| {
                let ws = d
                    .get("workspace_id")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                tx2.send(Msg::Herdr(ws)).is_ok()
            },
        );
        if res.is_err() {
            std::thread::sleep(Duration::from_secs(3));
        } else {
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

fn focus(agent: &AgentRef) {
    let _ = herdr::call("workspace.focus", json!({ "workspace_id": agent.workspace }));
    let _ = herdr::call("pane.focus", json!({ "pane_id": agent.pane }));
}

// ---- main loop ----------------------------------------------------------

pub fn run(store: &Store) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let self_ws = herdr::Env::read().workspace_id;

    let (tx, rx) = mpsc::channel::<Msg>();
    spawn_input(tx.clone());
    spawn_events(tx.clone());
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        });
    }

    print!("\x1b[?1049h\x1b[?25l");
    let _ = std::io::stdout().flush();
    stty(&["raw", "-echo"]);

    let code = event_loop(store, &rx, self_ws.as_deref());

    stty(&["sane"]);
    print!("\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();
    code
}

fn event_loop(store: &Store, rx: &Receiver<Msg>, self_ws: Option<&str>) -> i32 {
    let mut view = View::default();
    let mut ui = collect(store, &view, self_ws);
    let mut last = String::new();
    let mut dirty = false;
    let mut last_fetch = Instant::now();
    let mut last_fingerprint = store.fingerprint();

    let draw = |ui: &Ui, last: &mut String| {
        let (w, h) = term_size();
        let frame = render(ui, w, h);
        if frame != *last {
            print!("{frame}");
            let _ = std::io::stdout().flush();
            *last = frame;
        }
    };
    draw(&ui, &mut last);

    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => Msg::Tick,
            Err(RecvTimeoutError::Disconnected) => return 0,
        };

        let mut refetch = false;
        match msg {
            Msg::Key(Key::Quit) => return 0,
            Msg::Key(k) => {
                let n = ui.rows.len();
                match k {
                    Key::Down => {
                        let mut i = ui.sel;
                        while i + 1 < n {
                            i += 1;
                            if ui.rows[i].selectable() {
                                break;
                            }
                        }
                        ui.sel = i;
                    }
                    Key::Up => {
                        let mut i = ui.sel;
                        while i > 0 {
                            i -= 1;
                            if ui.rows[i].selectable() {
                                break;
                            }
                        }
                        ui.sel = i;
                    }
                    Key::Left | Key::Right => match ui.rows.get(ui.sel) {
                        Some(Row::Project { id, .. }) => {
                            if k == Key::Left {
                                view.collapsed.insert(id.clone());
                                // Folding a project forgets that it was showing
                                // its long tail, so unfolding starts clean.
                                view.expanded.remove(id);
                            } else {
                                view.collapsed.remove(id);
                            }
                            refetch = true;
                        }
                        Some(Row::More { key, .. }) if k == Key::Right => {
                            view.expanded.insert(key.clone());
                            refetch = true;
                        }
                        _ => {}
                    },
                    Key::Enter => match ui.rows.get(ui.sel) {
                        Some(Row::Project { id, collapsed: c, .. }) => {
                            if *c {
                                view.collapsed.remove(id);
                            } else {
                                view.collapsed.insert(id.clone());
                                view.expanded.remove(id);
                            }
                            refetch = true;
                        }
                        Some(Row::More { key, .. }) => {
                            view.expanded.insert(key.clone());
                            refetch = true;
                        }
                        Some(row) => match row.agent() {
                            Some(a) => focus(a),
                            None => {
                                ui.message = Some(("no agent on that task".into(), Instant::now()))
                            }
                        },
                        None => {}
                    },
                    Key::Digit(d) => {
                        let keys = hotkeys(&ui.rows);
                        if let Some(i) = keys.iter().position(|k| *k == Some(d)) {
                            if let Some(a) = ui.rows[i].agent() {
                                focus(a);
                            }
                        }
                    }
                    Key::Char('g') => ui.sel = 0,
                    Key::Char('G') => ui.sel = n.saturating_sub(1),
                    Key::Char('A') => {
                        view.show_done = !view.show_done;
                        ui.message = Some((
                            if view.show_done { "showing done" } else { "hiding done" }.into(),
                            Instant::now(),
                        ));
                        refetch = true;
                    }
                    Key::Char('r') => {
                        let mut cache = crate::sync::Cache::default();
                        let _ = crate::sync::sync(store, &mut cache, true);
                        ui.message = Some(("synced".into(), Instant::now()));
                        refetch = true;
                    }
                    Key::Char('?') => {
                        ui.message = Some((
                            "j/k move · ↵ fold/open · 1-9 jump · A done · r sync · q quit".into(),
                            Instant::now(),
                        ))
                    }
                    _ => {}
                }
            }
            Msg::Herdr(ws) => {
                // An event naming our own workspace means we are probably about
                // to be looked at: redraw now. Otherwise mark dirty and let the
                // tick decide, so twenty-odd background panels stay cheap.
                let concerns_us = ws.is_some() && ws.as_deref() == self_ws;
                if concerns_us || ui.self_focused {
                    std::thread::sleep(Duration::from_millis(250));
                    while rx.try_recv().is_ok() {}
                    refetch = true;
                } else {
                    dirty = true;
                }
            }
            Msg::Tick => {
                let interval = if ui.self_focused {
                    Duration::from_secs(5)
                } else {
                    Duration::from_secs(30)
                };
                let store_changed = store.fingerprint() != last_fingerprint;
                if (dirty || store_changed) && last_fetch.elapsed() >= interval {
                    refetch = true;
                    dirty = false;
                }
            }
        }

        if refetch {
            last_fetch = Instant::now();
            last_fingerprint = store.fingerprint();
            let sel = ui.sel;
            let msg = ui.message.take();
            ui = collect(store, &view, self_ws);
            ui.sel = sel.min(ui.rows.len().saturating_sub(1));
            if !ui.rows.is_empty() && !ui.rows[ui.sel].selectable() {
                if let Some(next) = (ui.sel..ui.rows.len()).find(|i| ui.rows[*i].selectable()) {
                    ui.sel = next;
                }
            }
            ui.message = msg;
        }
        draw(&ui, &mut last);
    }
}

// ---- install / uninstall ------------------------------------------------

fn panel_command() -> Vec<String> {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "wsp".into());
    vec![exe, "panel".into()]
}

fn panels_state(store: &Store) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(store.state.join("panels.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn save_panels(store: &Store, map: &std::collections::BTreeMap<String, String>) {
    let obj: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let _ = std::fs::create_dir_all(&store.state);
    let _ = crate::store::write_atomic(
        &store.state.join("panels.json"),
        &serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap_or_default(),
    );
}

struct PaneInfo {
    id: String,
    label: String,
}

fn list_panes(ws_id: &str) -> Result<Vec<PaneInfo>, String> {
    let r = herdr::call("pane.list", json!({ "workspace_id": ws_id })).map_err(|e| e.to_string())?;
    let arr = r.get("panes").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    Ok(arr
        .iter()
        // The server has honoured the filter, but a stray pane from another
        // workspace here would mean splitting the wrong tree.
        .filter(|p| p.get("workspace_id").and_then(|x| x.as_str()) == Some(ws_id))
        .filter_map(|p| {
            Some(PaneInfo {
                id: p.get("pane_id")?.as_str()?.to_string(),
                label: p.get("label").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `pane.split` starts the user's shell and takes no command, so the panel is
/// `exec`d over it — which also means quitting the panel closes the pane
/// instead of leaving a bare prompt behind.
fn launch_panel(pane: &str) {
    let cmd = panel_command().iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
    let _ = herdr::call("pane.rename", json!({ "pane_id": pane, "label": "wsp" }));
    let _ = herdr::call(
        "pane.send_text",
        json!({ "pane_id": pane, "text": format!("exec {cmd}\n") }),
    );
}

/// Install into one workspace. Returns Ok(true) when a panel was added,
/// Ok(false) when the workspace already had one.
///
/// This used to build a whole layout tree and hand it to `layout.apply`, with
/// the agent's pane re-referenced by id on the assumption that herdr would
/// carry it across. It does not: the tree is rebuilt from scratch and every
/// pane in it gets a fresh terminal, which killed every running agent. So:
/// `pane.split`, which adds one pane beside a target and touches nothing else.
pub fn install_one(store: &Store, ws_id: &str, ratio: f64) -> Result<bool, String> {
    let panes = list_panes(ws_id)?;
    let mut panels = panels_state(store);

    if let Some(existing) = panels.get(ws_id) {
        if panes.iter().any(|p| p.id == *existing) {
            return Ok(false);
        }
    }
    // A pane labelled `wsp` is ours even when the state file lost track of it
    // — a crash between the split and the save would otherwise install twice.
    if let Some(orphan) = panes.iter().find(|p| p.label == "wsp") {
        panels.insert(ws_id.to_string(), orphan.id.clone());
        save_panels(store, &panels);
        return Ok(false);
    }
    let Some(target) = panes.first() else {
        return Ok(false);
    };
    let before: HashSet<&str> = panes.iter().map(|p| p.id.as_str()).collect();

    // Splitting `right` at `ratio` leaves the *target* holding `ratio` and puts
    // the new pane in the remainder, so the sidebar arrives on the wrong side
    // at the wrong width. Swapping the two afterwards lands the panel in the
    // narrow left slot without disturbing either process.
    let res = herdr::call(
        "pane.split",
        json!({
            "direction": "right",
            "target_pane_id": target.id,
            "ratio": ratio,
            "focus": false,
        }),
    )
    .map_err(|e| e.to_string())?;

    let new_pane = res
        .get("pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let after = list_panes(ws_id).ok()?;
            after.into_iter().find(|p| !before.contains(p.id.as_str())).map(|p| p.id)
        })
        .ok_or_else(|| "split reported no new pane".to_string())?;

    let _ = herdr::call(
        "pane.swap",
        json!({ "source_pane_id": target.id, "target_pane_id": new_pane }),
    );
    launch_panel(&new_pane);

    panels.insert(ws_id.to_string(), new_pane);
    save_panels(store, &panels);
    Ok(true)
}

/// Auto-install for a newly created workspace — but only once the panel is
/// actually in use, so linking the plugin never surprises anyone.
pub fn install_if_adopted(store: &Store, ws_id: &str) {
    if panels_state(store).is_empty() {
        return;
    }
    let _ = install_one(store, ws_id, 0.22);
}

pub fn install(store: &Store, args: &crate::Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let ratio: f64 = args.get("ratio").and_then(|r| r.parse().ok()).unwrap_or(0.22);
    let only = args.get("workspace").or_else(|| herdr::Env::read().workspace_id);
    let all = args.has("all");

    // Without `--all` this installs into exactly one workspace. Run outside a
    // herdr pane there is no such workspace, and letting the filter fall
    // through would quietly install into every one of them.
    if !all && only.is_none() {
        eprintln!("wsp: not inside a herdr pane — pass --workspace <id> or --all");
        return 1;
    }

    let workspaces = herdr::workspaces().unwrap_or_default();
    let mut added = 0;
    let mut skipped: Vec<String> = Vec::new();

    for ws in &workspaces {
        if !all {
            if let Some(target) = &only {
                if &ws.id != target {
                    continue;
                }
            }
        }
        match install_one(store, &ws.id, ratio) {
            Ok(true) => added += 1,
            Ok(false) => {}
            Err(e) => skipped.push(format!("{}: {e}", ws.label)),
        }
    }

    if args.json() {
        println!("{}", json!({ "installed": added, "skipped": skipped }));
    } else {
        println!("panel installed in {added} workspace(s)");
        for sk in &skipped {
            println!("  skipped {sk}");
        }
    }
    0
}

pub fn uninstall(store: &Store, args: &crate::Args) -> i32 {
    let mut panels = panels_state(store);
    let only = args.get("workspace");
    let mut removed = 0;
    let targets: Vec<(String, String)> = panels
        .iter()
        .filter(|(ws, _)| only.as_ref().map(|o| *ws == o).unwrap_or(true))
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect();

    for (ws, pane) in targets {
        if herdr::call("pane.close", json!({ "pane_id": pane })).is_ok() {
            removed += 1;
        }
        panels.remove(&ws);
    }
    save_panels(store, &panels);

    if args.json() {
        println!("{}", json!({ "removed": removed }));
    } else {
        println!("panel removed from {removed} workspace(s)");
    }
    0
}
