//! `wsp view` — the detail pane.
//!
//! The panel answers "what is there"; this answers "what is this". It runs in a
//! pane of its own beside the panel, and follows whatever the panel last opened
//! — so pressing `↵` costs one pane for the whole session rather than one per
//! thing you look at, and the pane you are reading never moves.
//!
//! It shares the panel's `Line`/`Style` model, which means the same frame can be
//! painted to a terminal or dropped into the storyboard, and the two views
//! cannot drift apart on colour.

use std::io::Write;
use std::time::Duration;

use serde_json::json;

use crate::herdr;
use crate::model::{Status, Task};
use crate::panel::{self, line, Line, Style};
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util;

/// What a detail pane is pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Focus {
    Task(String),
    Project(String),
    Nothing,
}

impl Focus {
    fn from_json(v: &serde_json::Value) -> Focus {
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        match v.get("kind").and_then(|x| x.as_str()) {
            Some("task") if !id.is_empty() => Focus::Task(id),
            Some("project") if !id.is_empty() => Focus::Project(id),
            _ => Focus::Nothing,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            Focus::Task(id) => json!({ "kind": "task", "id": id }),
            Focus::Project(id) => json!({ "kind": "project", "id": id }),
            Focus::Nothing => json!({}),
        }
    }
}

/// Where the panel leaves the target for the view to pick up. Keyed by
/// workspace, so two workspaces can be reading different things at once.
fn focus_path(store: &Store) -> std::path::PathBuf {
    store.state.join("detail.json")
}

fn read_all(store: &Store) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(focus_path(store))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub(crate) fn set_focus(store: &Store, workspace: &str, focus: &Focus) {
    let mut all = read_all(store);
    all.insert(workspace.to_string(), focus.to_json());
    let _ = std::fs::create_dir_all(&store.state);
    let _ = crate::store::write_atomic(
        &focus_path(store),
        &serde_json::to_string_pretty(&serde_json::Value::Object(all)).unwrap_or_default(),
    );
}

pub(crate) fn get_focus(store: &Store, workspace: &str) -> Focus {
    read_all(store).get(workspace).map(Focus::from_json).unwrap_or(Focus::Nothing)
}

// ---- rendering ----------------------------------------------------------

/// Break `text` to `w` columns on word boundaries, falling back to a hard cut
/// for a single word longer than the pane is wide.
fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(8);
    let mut out: Vec<String> = Vec::new();
    for para in text.split('\n') {
        if para.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for word in para.split_whitespace() {
            if word.chars().count() > w {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut rest: Vec<char> = word.chars().collect();
                while rest.len() > w {
                    out.push(rest[..w].iter().collect());
                    rest = rest[w..].to_vec();
                }
                cur = rest.into_iter().collect();
                continue;
            }
            let need = if cur.is_empty() { word.chars().count() } else { cur.chars().count() + 1 + word.chars().count() };
            if need > w {
                out.push(std::mem::take(&mut cur));
                cur = word.to_string();
            } else {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

const LABEL_W: usize = 9;

/// `label   value` — the shape every field in this view takes.
fn field(label: &str, value: &str, style: Style) -> Line {
    let mut l = Line::default();
    l.push(Style::Dim, util::pad(label, LABEL_W));
    if value.is_empty() {
        l.push(Style::Dim, "—");
    } else {
        l.push(style, value.to_string());
    }
    l
}

fn status_style(s: Status) -> Style {
    match s {
        Status::Blocked => Style::Warn,
        Status::Doing => Style::Accent,
        Status::Review => Style::Muted,
        Status::Done => Style::Dim,
        _ => Style::Plain,
    }
}

fn heading(text: &str) -> Line {
    line(Style::Dim, text.to_string())
}

/// Everything the view reads. Same shape of decision as the panel's Snapshot:
/// taking the data in means a fixture can render a frame with nothing running.
#[allow(dead_code)]
pub(crate) struct Ctx {
    pub tasks: Vec<Task>,
    pub index: Index,
    pub claims: std::collections::BTreeMap<String, serde_json::Value>,
    pub worked: std::collections::BTreeMap<String, serde_json::Value>,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    pub panes: Vec<herdr::Pane>,
}

impl Ctx {
    fn live(store: &Store) -> Ctx {
        Ctx {
            tasks: store.tasks(),
            index: Index::new(store.projects()),
            claims: store.claims(),
            worked: store.worked(),
            bindings: store.bindings(),
            panes: herdr::panes().unwrap_or_default(),
        }
    }

    /// The pane working this task, if the live join says so.
    fn pane_for(&self, task: &str) -> Option<&herdr::Pane> {
        let pane_id = self.bindings.iter().find_map(|(pane, b)| {
            (b.get("task_id").and_then(|t| t.as_str()) == Some(task)).then(|| pane.clone())
        })?;
        self.panes.iter().find(|p| p.pane_id == pane_id)
    }
}

pub(crate) fn frame(ctx: &Ctx, focus: &Focus, w: usize, h: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    match focus {
        Focus::Nothing => {
            out.push(line(Style::Bold, "wsp"));
            out.push(line(Style::Dim, "─".repeat(w)));
            out.push(Line::default());
            for l in wrap("Nothing open. Press ↵ on a row in the panel.", w) {
                out.push(line(Style::Muted, l));
            }
        }
        Focus::Task(id) => task_frame(ctx, id, w, &mut out),
        Focus::Project(id) => project_frame(ctx, id, w, &mut out),
    }

    // A hint pinned to the bottom. `W` is worth naming here because it acts on
    // panes other than this one, which is not a thing you would guess.
    let footer = 2;
    out.truncate(h.saturating_sub(footer));
    while out.len() < h.saturating_sub(footer) {
        out.push(Line::default());
    }
    out.push(line(Style::Dim, "─".repeat(w)));
    out.push(line(
        Style::Dim,
        "h/l left or right · W save and close · q close, discarding",
    ));
    out
}

fn task_frame(ctx: &Ctx, id: &str, w: usize, out: &mut Vec<Line>) {
    let Some(t) = ctx.tasks.iter().find(|t| t.id == id) else {
        out.push(line(Style::Warn, format!("no task {id}")));
        return;
    };

    let mut head = Line::default();
    head.push(Style::Dim, "task ");
    head.push(Style::Bold, t.id.clone());
    out.push(head);
    out.push(line(Style::Dim, "─".repeat(w)));

    for l in wrap(&t.title, w) {
        out.push(line(Style::Plain, l));
    }
    out.push(Line::default());

    out.push(field("status", t.status().as_str(), status_style(t.status())));
    out.push(field("priority", t.priority().as_str(), Style::Plain));
    out.push(field("project", t.project.as_deref().unwrap_or("(inbox)"), Style::Plain));
    // What it is part of. A sub-task read on its own is missing the one thing
    // that says why it exists.
    if let Some(p) = t.parent.as_ref().and_then(|id| ctx.tasks.iter().find(|x| &x.id == id)) {
        out.push(field("under", &format!("{}  {}", p.id, p.title), Style::Muted));
    }

    // Own tags plus everything inherited from the project chain — the whole
    // point of inheritance is that it is invisible until something shows it.
    let inherited: Vec<String> = t
        .project
        .as_deref()
        .map(|p| ctx.index.effective_tags(p))
        .unwrap_or_default();
    let mut tags = t.tags.clone();
    for i in inherited {
        if !tags.contains(&i) {
            tags.push(i);
        }
    }
    out.push(field("tags", &tags.join(" "), Style::Muted));

    let age = util::age_days(&t.updated);
    out.push(field(
        "touched",
        &if age <= 0 { "today".to_string() } else { format!("{age}d ago") },
        if age > 30 { Style::Warn } else { Style::Plain },
    ));
    if !t.refs.is_empty() {
        out.push(field("refs", &t.refs.join("  "), Style::Muted));
    }

    // The join, spelled out: the durable claim and the live pane are different
    // facts and the panel only ever shows the second.
    out.push(Line::default());
    match ctx.claims.get(&t.id) {
        Some(c) => out.push(field("claimed", &crate::cmd_agent::claim_line(c), Style::Accent)),
        None => {
            out.push(field("claimed", "", Style::Dim));
            // The ghost: an agent works several tasks in sequence, and this is
            // the one it walked away from. Only once the claim is gone — while
            // one is live this is the shift before it, and the live fact wins
            // the line.
            if let Some(w) = ctx.worked.get(&t.id) {
                out.push(field("worked", &crate::cmd_agent::worked_line(w), Style::Muted));
            }
        }
    }
    match ctx.pane_for(&t.id) {
        Some(p) => {
            let what = if p.agent.is_empty() { "shell".to_string() } else { p.agent.clone() };
            out.push(field("pane", &format!("{} · {what} {}", p.pane_id, p.agent_status), Style::Plain));
        }
        None => out.push(field("pane", "", Style::Dim)),
    }

    // What is under it. The panel shows this as indentation and a count; here
    // there is room to say which children, and what state each is in — which
    // is the question you open a parent to ask.
    let kids = resolve::children_of(&ctx.tasks, &t.id);
    if !kids.is_empty() {
        let under = resolve::counts_under(&ctx.tasks, &t.id);
        out.push(Line::default());
        out.push(heading(&format!("sub-tasks · {} of {} open", under.open, kids.len())));
        for k in kids {
            let mut l = Line::default();
            l.push(Style::Dim, util::pad(k.status().as_str(), LABEL_W));
            l.push(status_style(k.status()), util::truncate(&k.title, w.saturating_sub(LABEL_W)));
            out.push(l);
        }
    }

    // The prose, in the order it is written: what this is, then what the work
    // needs. Both are optional and an empty one draws nothing rather than an
    // empty heading.
    for name in ["Overview", "Details"] {
        if let Some(text) = t.section(name) {
            out.push(Line::default());
            out.push(heading(&name.to_lowercase()));
            for l in wrap(&text, w) {
                if l.trim().is_empty() {
                    out.push(Line::default());
                } else {
                    out.push(line(Style::Plain, l));
                }
            }
        }
    }

    // The log, newest first — the reason the body exists is to be read after
    // the fact, and after the fact the last line is the one that matters.
    let log: Vec<&str> = t
        .body
        .lines()
        .filter(|l| l.trim_start().starts_with("- "))
        .collect();
    if !log.is_empty() {
        out.push(Line::default());
        out.push(heading("log"));
        for entry in log.iter().rev() {
            let text = entry.trim_start().trim_start_matches("- ");
            let mut first = true;
            for l in wrap(text, w.saturating_sub(2)) {
                let mut ln = Line::default();
                ln.push(Style::Dim, if first { "· " } else { "  " });
                ln.push(Style::Muted, l);
                out.push(ln);
                first = false;
            }
        }
    }
}

fn project_frame(ctx: &Ctx, id: &str, w: usize, out: &mut Vec<Line>) {
    let Some(p) = ctx.index.get(id).cloned() else {
        out.push(line(Style::Warn, format!("no project {id}")));
        return;
    };

    let mut head = Line::default();
    head.push(Style::Dim, "project ");
    head.push(Style::Bold, p.id.clone());
    out.push(head);
    out.push(line(Style::Dim, "─".repeat(w)));

    if p.name != p.id {
        for l in wrap(&p.name, w) {
            out.push(line(Style::Plain, l));
        }
        out.push(Line::default());
    }

    out.push(field("parent", p.parent.as_deref().unwrap_or("—"), Style::Plain));
    out.push(field("status", &p.status, Style::Plain));
    out.push(field("tags", &ctx.index.effective_tags(&p.id).join(" "), Style::Muted));
    if !p.roots.is_empty() {
        out.push(field("roots", &p.roots.join("  "), Style::Muted));
    }
    if !p.brief.is_empty() {
        out.push(Line::default());
        for l in wrap(&p.brief, w) {
            out.push(line(Style::Muted, l));
        }
    }

    // A project's prose, same as a task's — it is the same machinery now, and
    // a project is the natural home for "what is this and why".
    for name in ["Overview", "Details"] {
        if let Some(text) = p.section(name) {
            out.push(Line::default());
            out.push(heading(&name.to_lowercase()));
            for l in wrap(&text, w) {
                if l.trim().is_empty() {
                    out.push(Line::default());
                } else {
                    out.push(line(Style::Plain, l));
                }
            }
        }
    }

    let counts = resolve::counts_by_project(&ctx.index, &ctx.tasks);
    let c = counts.get(&p.id).copied().unwrap_or_default();
    out.push(Line::default());
    let mut totals = Line::default();
    totals.push(Style::Dim, util::pad("work", LABEL_W));
    totals.push(Style::Plain, format!("{} open", c.open));
    if c.doing > 0 {
        totals.push(Style::Dim, " · ");
        totals.push(Style::Accent, format!("{} doing", c.doing));
    }
    if c.blocked > 0 {
        totals.push(Style::Dim, " · ");
        totals.push(Style::Warn, format!("{} blocked", c.blocked));
    }
    if c.done > 0 {
        totals.push(Style::Dim, format!(" · {} done", c.done));
    }
    out.push(totals);

    let kids = ctx.index.children(Some(&p.id));
    if !kids.is_empty() {
        out.push(Line::default());
        out.push(heading("under it"));
        for k in kids {
            let kc = counts.get(&k.id).copied().unwrap_or_default();
            let mut l = Line::default();
            l.push(Style::Dim, "· ");
            l.push(Style::Plain, k.id.clone());
            let right = line(Style::Dim, if kc.open > 0 { kc.open.to_string() } else { "—".into() });
            l.pad(w.saturating_sub(l.width() + right.width()).max(1));
            l.spans.extend(right.spans);
            out.push(l);
        }
    }

    // Its own tasks, attention first — the same order the panel uses.
    let mut own: Vec<&Task> = ctx
        .tasks
        .iter()
        .filter(|t| t.project.as_deref() == Some(p.id.as_str()) && t.status().is_open())
        .collect();
    own.sort_by_key(|t| (t.status().rank(), t.priority().rank(), t.id.clone()));
    if !own.is_empty() {
        out.push(Line::default());
        out.push(heading("tasks"));
        for t in own {
            let mut l = Line::default();
            l.push(status_style(t.status()), format!("{} ", glyph_for(t.status())));
            l.push(Style::Plain, util::truncate(&t.title, w.saturating_sub(2)));
            out.push(l);
        }
    }
}

fn glyph_for(s: Status) -> &'static str {
    use panel::glyph as g;
    match s {
        Status::Blocked => g::BLOCKED,
        Status::Review => g::REVIEW,
        Status::Doing => g::DOING,
        Status::Done => g::DONE,
        _ => g::QUIET,
    }
}

// ---- closing the editors --------------------------------------------------

/// What to send an editor to make it save and quit.
///
/// Driving another program by keystroke is a guess, so the guess is written
/// down where it can be corrected rather than buried. `vi` is the default
/// because that is what an unset `$EDITOR` means.
///
/// Each sequence leads with that editor's "abort whatever is pending", which
/// is doing more work than it looks like. `q:` opens vim's command-line
/// window, and `Esc` does not close it — the buffer just takes `:wq` as text.
/// `Ctrl-C` leaves it, and also drops insert mode, a half-typed `:` command, a
/// pending operator and a "Press ENTER" prompt.
///
/// The quit is `wqa`, not `wq`: someone who has split inside their editor
/// would otherwise close one window and leave the pane sitting there.
fn save_and_quit_keys(editor: &str, force: bool) -> Option<(&'static str, String)> {
    let name = editor
        .rsplit('/')
        .next()
        .unwrap_or(editor)
        .split_whitespace()
        .next()
        .unwrap_or("");
    let bang = if force { "!" } else { "" };
    match name {
        "" | "vi" | "vim" | "nvim" | "view" | "hx" | "helix" => {
            Some(("\x03", format!("\x1b:wqa{bang}\r")))
        }
        "nano" | "pico" => Some(("\x03", "\x0f\r\x18".into())),
        "emacs" | "emacsclient" => Some(("\x07", "\x18\x13\x18\x03".into())),
        "micro" => Some(("\x1b", "\x13\x11".into())),
        _ => None,
    }
}

/// Focus the leftmost or rightmost pane below this one.
///
/// By position, not by name. Naming them — `o` for overview, `d` for details —
/// put the key for the left pane under the right hand and vice versa, which is
/// backwards every single time. `h` and `l` are left and right on the keyboard,
/// in vim, and in herdr's own `prefix+h/l`, so the same reach means the same
/// thing in all three.
///
/// The geometry comes from herdr rather than from the layout this code happens
/// to build, so it stays true if that layout ever changes.
fn focus_by_position(leftmost: bool) -> String {
    let Some(me) = herdr::Env::read().pane_id else {
        return "no pane id".into();
    };
    let sibs = siblings_of(&me);
    if sibs.is_empty() {
        return "nothing open beside this".into();
    }

    let rects = herdr::call("pane.layout", json!({ "pane_id": me }))
        .ok()
        .and_then(|r| r.get("layout").and_then(|l| l.get("panes").cloned()))
        .and_then(|p| p.as_array().cloned())
        .unwrap_or_default();
    let x_of = |id: &str| -> i64 {
        rects
            .iter()
            .find(|p| p.get("pane_id").and_then(|x| x.as_str()) == Some(id))
            .and_then(|p| p.get("rect"))
            .and_then(|r| r.get("x"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0)
    };

    let mut ordered: Vec<&herdr::Pane> = sibs.iter().collect();
    ordered.sort_by_key(|p| x_of(&p.pane_id));
    let target = if leftmost { ordered.first() } else { ordered.last() };
    match target {
        Some(p) => {
            let _ = herdr::call("pane.focus", json!({ "pane_id": p.pane_id }));
            String::new()
        }
        None => "nothing to focus".into(),
    }
}

/// What to send an editor to make it leave *without* saving — the counterpart
/// to `save_and_quit_keys`, and what `q` means once there is a `W` that saves.
fn discard_and_quit_keys(editor: &str) -> Option<(&'static str, &'static str)> {
    let name = editor
        .rsplit('/')
        .next()
        .unwrap_or(editor)
        .split_whitespace()
        .next()
        .unwrap_or("");
    match name {
        "" | "vi" | "vim" | "nvim" | "view" | "hx" | "helix" => Some(("\x03", "\x1b:qa!\r")),
        // nano asks; `n` is the answer to "save modified buffer?".
        "nano" | "pico" => Some(("\x03", "\x18n")),
        "emacs" | "emacsclient" => Some(("\x07", "\x18\x03n\r")),
        "micro" => Some(("\x1b", "\x11n")),
        _ => None,
    }
}

/// The panes this view shares a tab with, and whether they are editors we put
/// there. A view opened from the sidebar shares its tab with the panel and
/// whatever you were working in — closing *that* tab would take the sidebar
/// and your work pane with it, which is emphatically not what `q` means.
fn edit_tab_siblings(me: &str) -> Vec<herdr::Pane> {
    siblings_of(me)
        .into_iter()
        .filter(|p| p.label == "overview" || p.label == "details")
        .collect()
}

/// The outcome of asking the editors to leave.
enum Closing {
    /// Everything asked to go has gone, or there was nothing to ask.
    Done(String),
    /// These panes did not close. A second `W` forces them.
    Stuck(Vec<String>),
}

fn siblings_of(me: &str) -> Vec<herdr::Pane> {
    let Ok(panes) = herdr::panes() else { return Vec::new() };
    let Some(mine) = panes.iter().find(|p| p.pane_id == me) else { return Vec::new() };
    let tab = mine.tab_id.clone();
    panes.into_iter().filter(|p| p.tab_id == tab && p.pane_id != me).collect()
}

/// Tell every editor sharing this tab to save and quit, and check that they
/// did. The tab closes itself once the second one exits — that machinery
/// already exists — so this only has to get them to leave.
fn close_editors(force_these: &[String]) -> Closing {
    let env = herdr::Env::read();
    let Some(me) = env.pane_id else {
        return Closing::Done("no pane id — cannot find the editors".into());
    };

    // A second W after a stuck one: the user has now said it twice, so take
    // the panes down and accept losing whatever would not save.
    if !force_these.is_empty() {
        let mut gone = 0;
        for p in force_these {
            if herdr::call("pane.close", json!({ "pane_id": p })).is_ok() {
                gone += 1;
            }
        }
        return Closing::Done(format!("forced {gone} closed"));
    }

    let editor = std::env::var("EDITOR").unwrap_or_default();
    let targets: Vec<String> = siblings_of(&me).into_iter().map(|p| p.pane_id).collect();
    if targets.is_empty() {
        return Closing::Done("nothing open beside this".into());
    }
    let Some((abort, commit)) = save_and_quit_keys(&editor, false) else {
        return Closing::Done(format!("don't know how to save {editor} — quit it yourself"));
    };

    let send = |panes: &[String], text: &str| {
        for p in panes {
            let _ = herdr::call("pane.send_text", json!({ "pane_id": p, "text": text }));
        }
    };
    let still_open = |want: &[String]| -> Vec<String> {
        let live: Vec<String> = siblings_of(&me).into_iter().map(|p| p.pane_id).collect();
        want.iter().filter(|p| live.contains(p)).cloned().collect()
    };

    // Two sends, not one. Vim throws away pending type-ahead when it takes an
    // interrupt, so a Ctrl-C and the command in the same write means the
    // command is eaten and the editor just sits there — which is exactly the
    // stuck pane this was meant to prevent.
    send(&targets, abort);
    std::thread::sleep(Duration::from_millis(150));
    send(&targets, &commit);
    std::thread::sleep(Duration::from_millis(900));
    let left = still_open(&targets);
    if left.is_empty() {
        return Closing::Done(format!("saved {}", targets.len()));
    }

    // Something refused. Most often a buffer vim will not write without a
    // bang, or a modal it wanted an answer to. Try once harder before giving
    // up, since a stuck pane with no explanation is the worst outcome.
    if let Some((abort, forced)) = save_and_quit_keys(&editor, true) {
        send(&left, abort);
        std::thread::sleep(Duration::from_millis(150));
        send(&left, &forced);
        std::thread::sleep(Duration::from_millis(900));
    }
    let left = still_open(&targets);
    if left.is_empty() {
        Closing::Done("saved".into())
    } else {
        Closing::Stuck(left)
    }
}

// ---- the pane -----------------------------------------------------------

pub fn run(store: &Store, args: &crate::Args) -> i32 {
    let ws = args
        .get("workspace")
        .or_else(|| herdr::Env::read().workspace_id)
        .unwrap_or_else(|| "-".into());

    // An explicit id pins the view; otherwise it follows whatever the panel in
    // this workspace last opened.
    let pinned = args.rest.first().and_then(|needle| {
        store
            .find_task(needle)
            .map(|t| Focus::Task(t.id))
            .or_else(|| Index::new(store.projects()).find(needle).map(|p| Focus::Project(p.id.clone())))
    });

    print!("\x1b[?1049h\x1b[?25l");
    let _ = std::io::stdout().flush();
    panel::stty(&["raw", "-echo", "min", "0", "time", "1"]);

    // No event stream here: the view is a reader, and a second subscriber per
    // workspace would double herdr's fan-out for a pane that can wait. Instead
    // poll often and do nothing unless something moved — reading the target
    // and stat-ing the store is cheap, re-reading every task file is not.
    let started_as = panel::exe_stamp();
    let mut last = String::new();
    let mut seen: Option<(Focus, u64)> = None;
    let mut quit = false;
    let mut reload = false;
    // Panes a previous W could not get rid of. A second W closes them outright.
    let mut stuck: Vec<String> = Vec::new();
    let Ok(mut tty) = std::fs::File::open("/dev/tty") else { return 1 };

    while !quit && !reload {
        if started_as.is_some() && panel::exe_stamp() != started_as {
            reload = true;
            break;
        }
        let focus = pinned.clone().unwrap_or_else(|| get_focus(store, &ws));
        let fp = store.fingerprint();
        if seen.as_ref() != Some(&(focus.clone(), fp)) {
            seen = Some((focus.clone(), fp));
            let (w, h) = panel::term_size();
            let ctx = Ctx::live(store);
            let painted = panel::to_ansi(&frame(&ctx, &focus, w, h), w, h);
            if painted != last {
                print!("{painted}");
                let _ = std::io::stdout().flush();
                last = painted;
            }
        }

        use std::io::Read;
        let mut buf = [0u8; 1];
        match tty.read(&mut buf) {
            // `min 0 time 1` gives a 100ms read timeout, so this doubles as
            // the poll interval — no separate sleep, and a keypress is felt
            // immediately rather than after the rest of a tick.
            Ok(1) if buf[0] == b'q' || buf[0] == 3 => {
                // In an edit tab, `q` takes the whole thing down — leaving two
                // editors behind with the context gone is a tab that can only
                // confuse. `W` is the one that saves; this is its opposite, and
                // the footer says so.
                let me = herdr::Env::read().pane_id.unwrap_or_default();
                let editors = edit_tab_siblings(&me);
                if !editors.is_empty() {
                    let ed = std::env::var("EDITOR").unwrap_or_default();
                    if let Some((abort, discard)) = discard_and_quit_keys(&ed) {
                        for p in &editors {
                            let _ = herdr::call(
                                "pane.send_text",
                                json!({ "pane_id": p.pane_id, "text": abort }),
                            );
                        }
                        std::thread::sleep(Duration::from_millis(150));
                        for p in &editors {
                            let _ = herdr::call(
                                "pane.send_text",
                                json!({ "pane_id": p.pane_id, "text": discard }),
                            );
                        }
                        std::thread::sleep(Duration::from_millis(600));
                    }
                    // Close the tab whether or not they went quietly: `q` is a
                    // decision to be rid of this, and a pane that would not
                    // quit must not be able to veto it.
                    if let Ok(panes) = herdr::panes() {
                        if let Some(mine) = panes.iter().find(|p| p.pane_id == me) {
                            let _ = herdr::call(
                                "tab.close",
                                json!({ "tab_id": mine.tab_id }),
                            );
                        }
                    }
                }
                quit = true;
            }
            // Arrows say the same thing as h/l for anyone who does not think
            // in vim. `min 0 time 1` means the tail of the sequence is already
            // waiting, so reading it cannot block.
            Ok(1) if buf[0] == b'\x1b' => {
                let mut seq = [0u8; 2];
                if tty.read(&mut seq[..1]).is_ok() && seq[0] == b'[' && tty.read(&mut seq[1..]).is_ok() {
                    match seq[1] {
                        b'D' => {
                            focus_by_position(true);
                        }
                        b'C' => {
                            focus_by_position(false);
                        }
                        _ => {}
                    }
                }
            }
            Ok(1) if buf[0] == b'h' || buf[0] == b'l' => {
                let msg = focus_by_position(buf[0] == b'h');
                if !msg.is_empty() {
                    let (w, _) = panel::term_size();
                    let mut l = Line::default();
                    l.push(Style::Warn, util::truncate(&msg, w));
                    l.fit(w);
                    print!("\x1b[999;1H{}", panel::to_ansi(&[l], w, 1).trim_start_matches("\x1b[H\x1b[2J"));
                    let _ = std::io::stdout().flush();
                }
            }
            Ok(1) if buf[0] == b'W' => {
                let outcome = close_editors(&stuck);
                let msg = match outcome {
                    Closing::Done(m) => {
                        stuck.clear();
                        m
                    }
                    Closing::Stuck(panes) => {
                        let names: Vec<&str> =
                            panes.iter().map(|p| p.as_str()).collect();
                        stuck = panes.clone();
                        format!("{} would not close — W again to force", names.join(" "))
                    }
                };
                let (w, _) = panel::term_size();
                let mut l = Line::default();
                l.push(Style::Accent, util::truncate(&msg, w));
                l.fit(w);
                print!("\x1b[999;1H{}", panel::to_ansi(&[l], w, 1).trim_start_matches("\x1b[H\x1b[2J"));
                let _ = std::io::stdout().flush();
                // Force a repaint once they are gone.
                seen = None;
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }

    panel::stty(&["sane"]);
    print!("\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();

    if reload {
        if let Ok(exe) = std::env::current_exe() {
            use std::os::unix::process::CommandExt;
            let mut c = std::process::Command::new(exe);
            c.arg("view");
            if let Some(id) = args.rest.first() {
                c.arg(id);
            }
            let err = c.exec();
            eprintln!("wsp: could not reload: {err}");
            return 1;
        }
    }
    0
}
