//! `wsp panel storyboard` — render the panel without a terminal.
//!
//! Two kinds of scene, mirroring the pair that works well on the Daisy side:
//! *fixtures* pin a hand-built state and catch layout regressions, and *flows*
//! push scripted keys through the real reducer so the frame is a consequence of
//! input rather than an assertion about it.
//!
//! Nothing here talks to herdr or the store. That is the point: the frames come
//! out the same on a laptop with nothing running.

use std::collections::BTreeMap;

use serde_json::json;

use crate::herdr;
use crate::model::{Project, Task};
use crate::input::Key;
use crate::panel::{self, Snapshot};

// ---- fixture building ---------------------------------------------------

fn project(id: &str, parent: Option<&str>) -> Project {
    let mut p = Project::new(id);
    p.parent = parent.map(|s| s.to_string());
    p
}

fn task(id: &str, title: &str, proj: Option<&str>, status: &str) -> Task {
    let mut t = Task::new(title, id);
    t.project = proj.map(|s| s.to_string());
    t.status_raw = status.to_string();
    t
}

fn workspace(id: &str, label: &str, focused: bool) -> herdr::Workspace {
    herdr::Workspace {
        id: id.to_string(),
        label: label.to_string(),
        focused,
        ..Default::default()
    }
}

fn agent(pane: &str, ws: &str, state: &str, title: &str) -> herdr::Pane {
    herdr::Pane {
        pane_id: pane.to_string(),
        workspace_id: ws.to_string(),
        agent: "claude".into(),
        agent_status: state.to_string(),
        title: title.to_string(),
        ..Default::default()
    }
}

/// A pane with nobody driving it. The panel could not see these at all before,
/// because a shell is not an agent.
fn shell(pane: &str, ws: &str, cwd: &str) -> herdr::Pane {
    herdr::Pane {
        pane_id: pane.to_string(),
        workspace_id: ws.to_string(),
        cwd: cwd.to_string(),
        ..Default::default()
    }
}

/// A small but complete world: nested projects, every task status, an agent
/// bound to a task, an idle agent on a `doing` task (the "needs you" case), an
/// unclaimed agent, a project past the task cap, and two inbox items.
fn world() -> Snapshot {
    let mut projects = vec![
        project("audio", None),
        project("vst", Some("audio")),
        project("trance", Some("vst")),
        project("verb", Some("vst")),
        project("meta", None),
        project("tooling", Some("meta")),
        project("wsp", Some("tooling")),
    ];
    // Roots, so a shell's cwd can put it somewhere without anyone claiming it.
    projects[2].roots = vec!["~/claude/trance".into()];
    projects[3].roots = vec!["~/claude/reverb".into()];
    projects[6].roots = vec!["~/claude/wsp".into()];

    let mut tasks = vec![
        task("t-001", "Apply reverb fixes from the tuning table", Some("trance"), "doing"),
        task("t-002", "Plan demo video strategy", Some("trance"), "doing"),
        // Doing, with an idle agent on it — the one combination that raises the
        // "needs you" arrow, because the work is live and the process is not.
        {
            let mut t = task("t-003", "Retune the early reflections", Some("verb"), "doing");
            t.body = "## Overview\nThe tail is right and the first 40 ms is not. Early reflections \
are arriving too clean for the room the rest of the patch implies.\n\n\
## Details\n- compare against the plate at 2.4s RT60\n\
- the diffusion stage is suspect, not the tank\n\n\
## Log\n- 2026-08-14 claimed by pane w2:p1\n"
                .into();
            t
        },
        task("t-004", "Ship the release notes", Some("verb"), "review"),
        task("t-006", "Waiting on the tuning table decision", Some("verb"), "blocked"),
        task("t-005", "Design workspace management system", Some("tooling"), "doing"),
        // A decomposed task: what an agent given direction on t-005 would
        // make for itself, and the case the tree has to render without the
        // parent and its children coming apart.
        {
            let mut t = task("t-051", "Draft the store schema", Some("tooling"), "done");
            t.parent = Some("t-005".into());
            t
        },
        {
            let mut t = task("t-052", "Wire the daemon to the socket", Some("tooling"), "doing");
            t.parent = Some("t-005".into());
            t
        },
        task("t-020", "Buy new monitor stand", None, "todo"),
        task("t-021", "Renew domain for joltsite", None, "todo"),
        task("t-030", "Retired: old sidebar experiment", Some("verb"), "done"),
    ];
    // Enough to overflow the per-project cap and produce a `n more` row.
    for i in 1..=8 {
        tasks.push(task(
            &format!("t-1{i:02}"),
            &format!("Panel work item number {i}"),
            Some("wsp"),
            "todo",
        ));
    }

    let workspaces = vec![
        workspace("w0", "orchestrator", true),
        workspace("w1", "Trance Video", false),
        workspace("w2", "Verb UI", false),
        workspace("w3", "Easter", false),
        workspace("w4", "Trance Lite", false),
        workspace("w5", "panel work", false),
    ];

    let agents = vec![
        agent("w1:p1", "w1", "working", "Trance Video"),
        agent("w2:p1", "w2", "idle", "Verb UI"),
        agent("w3:p1", "w3", "working", "◐ Unclaimed explorer"),
        // Shells: no agent, placed purely by where they are standing.
        shell("w4:p1", "w4", "~/claude/trance"),
        shell("w5:p1", "w5", "~/claude/wsp"),
        // The orchestrator's own home — resolves to no project on purpose.
        shell("w0:p1", "w0", "~/claude"),
        // One of ours, which must never appear as work.
        herdr::Pane { label: panel::PANEL_LABEL.into(), pane_id: "w0:p2".into(), workspace_id: "w0".into(), ..Default::default() },
    ];

    // w1's agent is claimed to a doing task; w2's is claimed to a doing task
    // but sitting idle, which is what raises the "needs you" arrow.
    let mut bindings = BTreeMap::new();
    bindings.insert("w1:p1".to_string(), json!({ "task_id": "t-001" }));
    bindings.insert("w2:p1".to_string(), json!({ "task_id": "t-003" }));

    Snapshot {
        projects,
        tasks,
        bindings,
        pins: BTreeMap::new(),
        workspaces,
        panes: agents,
    }
}

/// The same world with nothing running — what the panel looks like before any
/// agent exists, which is most people's first sight of it.
fn quiet_world() -> Snapshot {
    let mut s = world();
    s.panes.clear();
    s.bindings.clear();
    s
}

// ---- scenes -------------------------------------------------------------

struct Scene {
    title: String,
    caption: String,
    /// The keys that produced this frame, as the reader would press them.
    gesture: String,
    /// What a subcommand aimed at the cursor would act on.
    target: String,
    html: String,
}

/// Reads the seam back out in the store's own vocabulary, so a scene shows
/// what `add` or `done` would be pointed at from that row.
fn target_label(t: &panel::Target) -> String {
    match t {
        panel::Target::Project(id) => format!("project {id}"),
        panel::Target::Task(id) => format!("task {id}"),
        panel::Target::Inbox => "the inbox".into(),
        panel::Target::Unattached => "loose agents".into(),
        panel::Target::Pane(p) => format!("pane {p}"),
        panel::Target::Overflow(k) => format!("hidden tail of {k}"),
        panel::Target::Nothing => "nothing".into(),
    }
}

fn key_name(k: Key) -> String {
    match k {
        Key::Up => "↑".into(),
        Key::Down => "↓".into(),
        Key::Left => "←".into(),
        Key::Right => "→".into(),
        Key::Enter => "↵".into(),
        Key::Char(c) => c.to_string(),
        Key::Esc => "esc".into(),
        Key::Home => "⇱".into(),
        Key::End => "⇲".into(),
        Key::Backspace => "\u{232b}".into(),
        Key::Interrupt => "^C".into(),
        Key::Click { x, y } => format!("click {x},{y}"),
        Key::Wheel { up } => if up { "wheel ↑".into() } else { "wheel ↓".into() },
    }
}

/// `↓ ↓ ↓ A` reads worse than `↓ ×3  A` once a flow runs to eighteen presses.
fn compress(keys: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < keys.len() {
        let mut n = 1;
        while i + n < keys.len() && keys[i + n] == keys[i] {
            n += 1;
        }
        out.push(if n > 1 { format!("{} ×{n}", keys[i]) } else { keys[i].clone() });
        i += n;
    }
    out.join("   ")
}

const W: usize = 34;
const H: usize = 26;

/// Holds a panel mid-flow. Every transition goes through the real reducer, so
/// a scene can only show a state the live panel can actually reach.
struct Driver<'a> {
    snap: &'a Snapshot,
    view: panel::View,
    ui: panel::Ui,
    log: Vec<String>,
}

impl<'a> Driver<'a> {
    fn new(snap: &'a Snapshot) -> Driver<'a> {
        let view = panel::View::default();
        let ui = panel::collect(snap, &view, Some("w0"));
        Driver { snap, view, ui, log: Vec::new() }
    }

    fn key(&mut self, k: Key) -> &mut Self {
        self.log.push(key_name(k));
        // The reducer may ask for a refetch; offline that just means rebuilding
        // the rows from the same snapshot, exactly as the live loop does.
        if let panel::Effect::Refetch = panel::apply_key(k, &mut self.ui, &mut self.view) {
            panel::refetch_into(&mut self.ui, self.snap, &mut self.view, Some("w0"));
        }
        self
    }

    fn keys(&mut self, ks: &[Key]) -> &mut Self {
        for k in ks {
            self.key(*k);
        }
        self
    }

    /// Move off the current row first, then hunt. For reaching the *second*
    /// group when the cursor already opens on the first.
    fn down_to_next(&mut self, want: panel::RowKind) -> &mut Self {
        self.key(Key::Down);
        self.down_to(want)
    }

    /// Press `Down` until the cursor sits on a row of `want`. Bounded by the
    /// cursor going nowhere, so a `want` that is not present terminates rather
    /// than spins.
    fn down_to(&mut self, want: panel::RowKind) -> &mut Self {
        loop {
            if self.ui.selected_kind() == want {
                return self;
            }
            let before = self.ui.selected_index();
            self.key(Key::Down);
            if self.ui.selected_index() == before {
                return self;
            }
        }
    }

    fn scene(&self, title: &str, caption: &str) -> Scene {
        Scene {
            title: title.to_string(),
            caption: caption.to_string(),
            gesture: if self.log.is_empty() {
                "opens here".to_string()
            } else {
                compress(&self.log)
            },
            target: target_label(&self.ui.selected_target()),
            html: panel::to_html(&panel::frame(&self.ui, &self.view, W, H), W),
        }
    }
}

fn scenes() -> Vec<Scene> {
    let w = world();
    let q = quiet_world();
    let mut out = Vec::new();

    out.push(Driver::new(&q).scene(
        "Cold start",
        "No agents running — the durable half on its own. This is most people's first sight of the panel.",
    ));

    out.push(Driver::new(&w).scene(
        "Live",
        "Three agents. One working a task (●), one idle on a task that is still doing — which raises the ← asking for you — and one unclaimed at the foot. The cursor opens on the inbox, because unfiled work is what you triage before reading anything that already has a home.",
    ));

    out.push(
        Driver::new(&w)
            .keys(&[Key::Down, Key::Down, Key::Down])
            .scene("Moving down", "j × 3. Section headings are skipped; the cursor only rests on rows that lead somewhere."),
    );

    out.push(
        Driver::new(&w)
            .keys(&[Key::Down; 14])
            .scene("Halfway down", "Once the list outruns the pane the cursor is held near the middle, not pushed against the bottom edge — so what you are about to reach stays on screen beside what you have already passed. Only the two ends break it, where there is nothing further to show."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Project)
            .key(Key::Left)
            .scene("Project folded", "← on a project hides its tasks and every project beneath it — audio takes vst and trance down with it."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::More)
            .scene("On the overflow row", "wsp holds more tasks than the cap, so the tail collapses. The row is selectable — reaching it took no counting, the driver pressed down until it arrived."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::More)
            .key(Key::Enter)
            .scene("Expanded", "↵ on the overflow row opens the tail in place, and the cursor holds its ground."),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Left)
            .scene("Inbox folded", "← folds a group exactly as it folds a project. The count stays on the heading, so closing it loses nothing — and the tree starts where it would have anyway."),
    );

    out.push(
        Driver::new(&w)
            .down_to_next(panel::RowKind::Section)
            .scene("The other group", "Loose agents sit at the foot, after the work. Nothing can be added here — herdr owns agents — so the useful verb on this row is claim."),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('G'))
            .scene("At the foot", "G to the last row, which is in the dock. The cursor reports a pane to jump to rather than anything to edit — not every target is a thing you can change."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Agent)
            .keys(&[Key::Char('G'), Key::Char('c')])
            .scene("Handing work to an idle agent", "The dock holds every agent with no task, ruled off at the foot and pinned there — the tree above it scrolls and this does not. It is the one row you most need to see and the first the tree would push off the bottom, since the tree sorts by work and these panes have none. `c` on one turns the tree into the picker: choose what it takes."),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('A'))
            .scene("Showing done", "A brings finished work back, and with it any project holding nothing else."),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('R'))
            .scene(
                "Only what needs review",
                "`R` narrows the tree to work an agent has finished with and handed back — `review` is where an agent stops, and only a person says `done`. The project rows stay so each one is placed, and every key goes on meaning what it means: `d` closes it, `o` sends it back, `↵` opens it in the detail pane. Nothing else changes, which is why this is a filter and not a second pane. The footer says the filter is on, because one left up silently reads as an empty backlog.",
            ),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('i'))
            .scene("Showing ids", "i puts the id in front of each title — the thing you type at a shell, next to the thing you read. Off by default because thirteen characters of `t-260815-004` on every row is most of a narrow pane, and only the last three of them differ. The suffix is what `wsp start 004` resolves, so the suffix is what shows, unless another open task shares it and the date is what separates them."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .key(Key::Char('?'))
            .scene("Help", "? docks the key map under the tree, taking the rows it needs and no more. The cursor keeps its row and every key still works, so you can read `b` and press it on the task you are looking at. ? again puts it away. The verbs come first because a short pane cuts from the bottom, and movement is the half you can find by pressing an arrow."),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('?'))
            .keys(&[Key::Down; 14])
            .scene("Moving with it up", "The tree carries on underneath. It has fewer rows to work with, so it scrolls sooner — but the cursor is still held in the middle of what is left, rather than the map being allowed to push it off the bottom."),
    );

    // ---- management ----

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Project)
            .key(Key::Char('a'))
            .type_in("Retune the plate decay")
            .scene("Adding to a project", "a on a project opens a field in the footer. The cursor's row decides the scope, so this becomes `wsp add … -p audio` without asking which project."),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('a'))
            .type_in("Pick up milk")
            .scene("Adding to the inbox", "The same key on the inbox group. The scope is deliberately no project, which is what the inbox means."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .key(Key::Char('a'))
            .type_in("Check the diffusion stage")
            .scene("Adding under a task", "The same key on a task makes a sub-task of it — the footer says which. A sibling is one row away, on the project heading above; nothing else reaches beneath a task, and decomposing the work you are looking at is the commoner move."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .key(Key::Char('b'))
            .type_in("waiting on the tuning table")
            .scene("Blocking, with a reason", "b asks why. `wsp block` requires a reason and so does the panel — a blocked task that does not say why is the one you cannot act on later."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .key(Key::Char('m'))
            .scene("Moving a task", "m turns the tree itself into the picker. Navigation and folding still work, so you hunt for the destination the way you would read for it — then ↵ takes whatever the cursor is on."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .keys(&[Key::Char('m'), Key::Down, Key::Down])
            .scene("Landing somewhere valid", "Still picking. A project is a destination; so is the inbox, which unfiles the task. Anything else and ↵ says so rather than doing something surprising."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Project)
            .key(Key::Char('X'))
            .scene("Before removing", "X asks first, and the question carries the consequence — how many tasks would be displaced — because the answer changes with the row and you should not have to remember."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .key(Key::Char('c'))
            .scene("Claiming", "c from a task picks the agent that takes it. From an agent row it runs the other way — pick the task it moves to, which is how one agent hands itself from one piece of work to the next."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Agent)
            .key(Key::Char('c'))
            .scene("Migrating an agent", "The same key from the pane row. Landing on a task moves the agent to it: the task being left keeps its status — work underway with nobody on it is a real state — and gives up its claim, keeping the record of who had it. The cursor is on the pane row, and the pane row is what the tree carries to wherever it lands."),
    );

    out.extend(detail_scenes(&w));
    out
}

trait TypeIn {
    fn type_in(&mut self, text: &str) -> &mut Self;
}

impl TypeIn for Driver<'_> {
    /// Type a value a character at a time, through the same reducer a keyboard
    /// would reach — so a prompt that mishandles a space or a digit shows up
    /// here rather than in a pane.
    fn type_in(&mut self, text: &str) -> &mut Self {
        for c in text.chars() {
            self.key(Key::Char(c));
        }
        self
    }
}

/// The detail pane, rendered from the same fixture. It shares the panel's
/// Line/Style model, so a colour that drifts in one drifts visibly in both.
fn detail_scenes(w: &Snapshot) -> Vec<Scene> {
    use crate::detail::{self, Focus};
    // Claims for the two tasks an agent is on, and — on the task the Trance
    // agent left — the ghost it leaves behind. No `claimed_at`: a live claim
    // prints how long it has been held, and a fixture that says "356d" is a
    // fixture whose age is showing.
    let mut claims = BTreeMap::new();
    claims.insert("t-001".to_string(), json!({ "workspace_id": "w1", "workspace_label": "Trance Video" }));
    claims.insert("t-003".to_string(), json!({ "workspace_id": "w2", "workspace_label": "Verb UI" }));
    let mut worked = BTreeMap::new();
    worked.insert(
        "t-002".to_string(),
        json!({
            "workspace_id": "w1", "workspace_label": "Trance Video",
            "seconds": 11_520, "handed_to": "t-001", "reason": "handoff",
        }),
    );

    let ctx = detail::Ctx {
        tasks: w.tasks.clone(),
        index: crate::resolve::Index::new(w.projects.clone()),
        claims,
        worked,
        bindings: w.bindings.clone(),
        panes: w.panes.clone(),
    };
    let shot = |title: &str, caption: &str, focus: Focus| Scene {
        title: title.to_string(),
        caption: caption.to_string(),
        gesture: "↵".into(),
        target: match &focus {
            Focus::Task(id) => format!("task {id}"),
            Focus::Project(p) => format!("project {p}"),
            Focus::Nothing => "nothing".into(),
        },
        html: panel::to_html(&detail::frame(&ctx, &focus, W, H), W),
    };
    vec![
        shot("Detail: a task", "↵ on a task opens it here rather than folding something. Overview says what it is, Details carries the working material, and the log reads newest first — after the fact, the last line is the one that matters.", Focus::Task("t-003".into())),
        shot("Detail: after a handoff", "The task the Trance agent moved off. It is still doing — work underway with nobody on it is a true state, and usually means you are the blocker — but the claim has gone to the task the agent took up, and what is left is the trace: where it was worked, for how long, and what it was handed to.", Focus::Task("t-002".into())),
        shot("Detail: a parent", "A task with work decomposed under it — what an agent given direction on this one would make for itself. The panel shows the shape as indentation and a count; there is room here to say which children and what state each is in, which is the question you open a parent to ask.", Focus::Task("t-005".into())),
        shot("Detail: a project", "↵ on a project: rolled-up work, what sits under it, and its own tasks in the panel's order.", Focus::Project("trance".into())),
        shot("Detail: nothing yet", "The pane before anything is opened. It is a reader — it waits rather than guessing.", Focus::Nothing),
    ]
}

// ---- page ---------------------------------------------------------------

const CSS: &str = r#"
/* Neutrals carry a green bias taken from the panel's own accent, so the page
   and the specimen belong to the same world. */
:root {
  --ground:#F2F4F3; --surface:#FFFFFF; --ink:#171C1A; --sub:#5C6B66;
  --edge:#DCE3E0; --accent:#1E7A63; --rail:#8FA39C;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --ground:#101413; --surface:#171C1A; --ink:#E4EAE7; --sub:#8C9A95;
    --edge:#262E2B; --accent:#5FBFA4; --rail:#4A5954;
  }
}
:root[data-theme="dark"] {
  --ground:#101413; --surface:#171C1A; --ink:#E4EAE7; --sub:#8C9A95;
  --edge:#262E2B; --accent:#5FBFA4; --rail:#4A5954;
}

* { box-sizing:border-box; }
body {
  margin:0; padding:3rem 1.5rem 5rem; background:var(--ground); color:var(--ink);
  font:15px/1.6 ui-sans-serif,-apple-system,"Segoe UI",Roboto,sans-serif;
  -webkit-font-smoothing:antialiased;
}
.wrap { max-width:64rem; margin:0 auto; }

header { border-bottom:1px solid var(--edge); padding-bottom:1.75rem; margin-bottom:2.5rem; }
.eyebrow {
  font:600 11px/1 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  letter-spacing:.14em; text-transform:uppercase; color:var(--accent); margin:0 0 .9rem;
}
h1 {
  margin:0 0 .7rem; font:600 1.75rem/1.15 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  letter-spacing:-.02em; text-wrap:balance;
}
header p { margin:0; color:var(--sub); max-width:42rem; }

.scenes { display:flex; flex-direction:column; gap:2.25rem; }
.scene {
  display:grid; grid-template-columns:15rem minmax(0,1fr); gap:1.75rem;
  align-items:start; padding-bottom:2.25rem; border-bottom:1px solid var(--edge);
}
.scene:last-child { border-bottom:0; padding-bottom:0; }
@media (max-width:44rem) { .scene { grid-template-columns:1fr; gap:1rem; } }

.rail { display:flex; flex-direction:column; gap:.6rem; }
.rail h2 { margin:0; font-size:1rem; font-weight:600; letter-spacing:-.01em; }
.rail p { margin:0; color:var(--sub); font-size:.85rem; }
.rail p.tgt {
  padding-top:.55rem; border-top:1px dashed var(--edge); color:var(--rail);
  font-size:.78rem;
}
.rail p.tgt b {
  color:var(--accent); font-weight:600;
  font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
}
.gesture {
  display:inline-flex; align-self:start; gap:.1rem; padding:.3rem .55rem;
  border:1px solid var(--edge); border-radius:5px; color:var(--rail);
  font:12px/1.2 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  white-space:pre; background:var(--surface);
}

.frame { overflow-x:auto; }
/* The specimen keeps a terminal ground in both themes — it is a picture of a
   terminal, and recolouring it for a light page would misreport the product. */
pre.wsp {
  margin:0; padding:.85rem 1rem; border-radius:6px; width:max-content;
  background:#0D1110; color:#E4EAE7; border:1px solid var(--edge);
  font:12px/1.4 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  white-space:pre; font-variant-ligatures:none;
}
pre.wsp .p { color:inherit; }
pre.wsp .d { color:#66736E; }
pre.wsp .b { font-weight:700; }
pre.wsp .m { color:#7D8C96; }
pre.wsp .a { color:#5FBFA4; }
pre.wsp .w { color:#E08A4B; }
pre.wsp .sel { background:#E4EAE7; color:#0D1110; }
pre.wsp .sel .d, pre.wsp .sel .m, pre.wsp .sel .a, pre.wsp .sel .w,
pre.wsp .sel .p { color:#0D1110; }

footer {
  margin-top:3rem; padding-top:1.25rem; border-top:1px solid var(--edge);
  color:var(--sub); font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
}

h2.sec {
  margin:0 0 1.5rem; font:600 11px/1 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  letter-spacing:.14em; text-transform:uppercase; color:var(--accent);
}
.legend { margin-bottom:3.5rem; }
.lgroup {
  display:grid; grid-template-columns:15rem minmax(0,1fr); gap:1.75rem;
  align-items:start; padding-bottom:1.75rem; margin-bottom:1.75rem;
  border-bottom:1px solid var(--edge);
}
.lgroup:last-child { border-bottom:0; margin-bottom:0; }
@media (max-width:44rem) { .lgroup { grid-template-columns:1fr; gap:.9rem; } }
.lhead h3 { margin:0 0 .45rem; font-size:1rem; font-weight:600; letter-spacing:-.01em; }
.lhead p { margin:0; color:var(--sub); font-size:.85rem; }

.legend dl { margin:0; display:flex; flex-direction:column; gap:.5rem; }
.mark { display:grid; grid-template-columns:3.5rem minmax(0,1fr); gap:1rem; align-items:baseline; }
.mark dt { margin:0; }
.chip {
  display:inline-block; min-width:3.5rem; text-align:center; padding:.25rem .4rem;
  border-radius:5px; background:#0D1110; border:1px solid var(--edge);
  font:12px/1.3 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; color:#E4EAE7;
  white-space:pre;
}
.chip .p { color:inherit; }
.chip .d { color:#66736E; }
.chip .b { font-weight:700; }
.chip .m { color:#7D8C96; }
.chip .a { color:#5FBFA4; }
.chip .w { color:#E08A4B; }
.mark dd { margin:0; display:flex; flex-direction:column; gap:.1rem; }
.mark dd b { font-weight:600; font-size:.9rem; }
.mark dd span { color:var(--sub); font-size:.83rem; }
"#;


fn page(scenes: &[Scene]) -> String {
    let mut out = String::new();
    out.push_str("<title>wsp panel storyboard</title>\n<style>");
    out.push_str(CSS);
    out.push_str("</style>\n<div class=\"wrap\">\n");
    out.push_str(
        "<header>\
         <p class=\"eyebrow\">wsp · panel</p>\
         <h1>Every state the sidebar can reach</h1>\
         <p>Each frame below is the real renderer over a fixed fixture — no herdr, \
         no store, no terminal. The flows are produced by pushing the keys shown \
         through the same reducer the live panel runs, so a scene can only show a \
         state you could actually arrive at.</p>\
         </header>\n",
    );
    out.push_str("<section class=\"legend\"><h2 class=\"sec\">What the marks mean</h2>\n");
    for (group, blurb, marks) in panel::legend() {
        out.push_str(&format!(
            "<div class=\"lgroup\"><div class=\"lhead\"><h3>{group}</h3><p>{blurb}</p></div><dl>"
        ));
        for m in marks {
            out.push_str(&format!(
                "<div class=\"mark\"><dt><span class=\"chip\">{}</span></dt>\
                 <dd><b>{}</b><span>{}</span></dd></div>",
                panel::to_html_spans(&m.sample),
                m.name,
                m.note
            ));
        }
        out.push_str("</dl></div>\n");
    }
    out.push_str("</section>\n");

    out.push_str("<h2 class=\"sec\">Frames</h2>\n<div class=\"scenes\">\n");
    for s in scenes {
        out.push_str(&format!(
            "<section class=\"scene\">\
             <div class=\"rail\"><h2>{}</h2><span class=\"gesture\">{}</span><p>{}</p>\
             <p class=\"tgt\">cursor is on <b>{}</b></p></div>\
             <div class=\"frame\">{}</div>\
             </section>\n",
            s.title, s.gesture, s.caption, s.target, s.html
        ));
    }
    out.push_str("</div>\n");
    out.push_str(&format!(
        "<footer>{} scenes · rendered at {}×{} cells · wsp panel storyboard</footer>\n</div>\n",
        scenes.len(),
        W,
        H
    ));
    out
}

pub fn run(args: &crate::Args) -> i32 {
    let scenes = scenes();
    let html = page(&scenes);

    match args.get("out") {
        Some(path) => {
            let p = crate::util::expand(&path);
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match crate::store::write_atomic(&p, &html) {
                Ok(()) => {
                    println!("wrote {} ({} scenes)", p.display(), scenes.len());
                    0
                }
                Err(e) => {
                    eprintln!("wsp: {}: {e}", p.display());
                    1
                }
            }
        }
        None => {
            print!("{html}");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A click acts on the row under the pointer, or on nothing. Every case
    /// here is one where being off by a single line would act on the wrong
    /// task — silently, and looking exactly like a misplaced click.
    fn ui_of(snap: &Snapshot, view: &panel::View) -> panel::Ui {
        panel::collect(snap, view, Some("w0"))
    }

    #[test]
    fn the_first_tree_row_sits_under_the_title_and_its_rule() {
        let w = world();
        let view = panel::View::default();
        let ui = ui_of(&w, &view);
        assert_eq!(panel::row_at(&ui, &view, W, H, 0), None, "the title");
        assert_eq!(panel::row_at(&ui, &view, W, H, 1), None, "the rule under it");
        assert_eq!(panel::row_at(&ui, &view, W, H, 2), Some(0), "the first row");
        assert_eq!(panel::row_at(&ui, &view, W, H, 3), Some(1));
    }

    #[test]
    fn furniture_is_not_a_row() {
        let w = quiet_world();
        let view = panel::View::default();
        let ui = ui_of(&w, &view);
        // Far below anything drawn: the blank tail, the rules and the footer.
        assert_eq!(panel::row_at(&ui, &view, W, H, H - 1), None);
        assert_eq!(panel::row_at(&ui, &view, W, H, H - 2), None);
        assert_eq!(panel::row_at(&ui, &view, W, H, H - 3), None);
    }

    /// The tree scrolls once the cursor is past the middle, and a click has to
    /// go through the same offset the frame drew with — otherwise clicking
    /// works at the top of a list and acts on the wrong task further down.
    #[test]
    fn a_click_follows_the_scroll() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        for _ in 0..20 {
            panel::apply_key(Key::Down, &mut ui, &mut view);
        }
        let first = panel::row_at(&ui, &view, W, H, 2).expect("a row at the top");
        assert!(first > 0, "the tree has scrolled, so the top row is not row 0");
        // Whatever is at the top, the row below it is the next one along.
        assert_eq!(panel::row_at(&ui, &view, W, H, 3), Some(first + 1));
        // And the selected row is where the cursor actually is.
        let sel_y = (2..H).find(|y| panel::row_at(&ui, &view, W, H, *y) == Some(ui.selected_index()));
        assert!(sel_y.is_some(), "the selected row is on screen and clickable");
    }

    /// The invariant that matters, and the one the other tests here do not
    /// check: what `row_at` says is at screen row `y` must be what `frame`
    /// actually *drew* at `y`. Comparing the mapping against its own
    /// arithmetic proves only that it is self-consistent.
    fn assert_mapping_matches_frame(ui: &panel::Ui, view: &panel::View, at: &str) {
        let drawn = panel::frame(ui, view, W, H);
        for y in 0..H {
            let Some(i) = panel::row_at(ui, view, W, H, y) else { continue };
            let want = panel::spans_of(&panel::render_row_for_test(ui, i, W));
            let got = panel::spans_of(&drawn[y]);
            assert_eq!(got, want, "{at}: screen row {y} maps to tree row {i}, which is not what is drawn there");
        }
    }

    /// The fixture's own size is one configuration out of many. A click that
    /// aligns in a 26-row pane and not in a 40-row one is the same bug either
    /// way, and only the shape of the pane decides which you happen to have.
    #[test]
    fn the_mapping_matches_the_frame_at_every_size() {
        let w = world();
        for h in [10usize, 14, 20, 26, 33, 40, 60] {
            for help in [false, true] {
                let mut view = panel::View::default();
                view.set_help_for_test(help);
                let mut ui = ui_of(&w, &view);
                for n in 0..26 {
                    let drawn = panel::frame(&ui, &view, W, h);
                    for y in 0..h {
                        let Some(i) = panel::row_at(&ui, &view, W, h, y) else { continue };
                        let want = panel::spans_of(&panel::render_row_for_test(&ui, i, W));
                        let got = panel::spans_of(&drawn[y]);
                        assert_eq!(
                            got, want,
                            "h={h} help={help} after {n} down: screen row {y} maps to tree row {i}, not what is drawn"
                        );
                    }
                    panel::apply_key(Key::Down, &mut ui, &mut view);
                }
            }
        }
    }

    /// Ed: "when we scroll they no longer align as expected". This is why.
    /// Selecting recentres the tree, so the row you clicked slides out from
    /// under the pointer — and the second click of select-then-activate lands
    /// on whatever moved into its place.
    #[test]
    fn clicking_a_row_leaves_it_where_it_was() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        // Scroll into the middle of a list long enough to move.
        for _ in 0..14 {
            panel::apply_key(Key::Down, &mut ui, &mut view);
        }
        for y in [2usize, 3, 6, 9] {
            let Some(i) = panel::row_at(&ui, &view, W, H, y) else { continue };
            let (mut after_ui, mut after_view) = (ui.clone(), view.clone());
            assert_eq!(panel::click(&mut after_ui, &mut after_view, W, H, y), panel::Hit::Select);
            assert_eq!(
                panel::row_at(&after_ui, &after_view, W, H, y),
                Some(i),
                "clicking screen row {y} moved tree row {i} out from under the pointer"
            );
            // And the row it selected is the row that was clicked, so the
            // second click activates rather than selecting something else.
            assert_eq!(panel::click(&mut after_ui, &mut after_view, W, H, y), panel::Hit::Activate);
        }
    }

    #[test]
    fn the_mapping_matches_the_frame_at_rest() {
        let w = world();
        let view = panel::View::default();
        assert_mapping_matches_frame(&ui_of(&w, &view), &view, "at rest");
    }

    #[test]
    fn the_mapping_matches_the_frame_after_scrolling() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        for n in 0..24 {
            panel::apply_key(Key::Down, &mut ui, &mut view);
            assert_mapping_matches_frame(&ui, &view, &format!("after {} down", n + 1));
        }
    }

    /// Every row the frame draws must be reachable by clicking it, and no two
    /// screen rows may claim the same one.
    #[test]
    fn the_mapping_is_one_to_one_over_what_is_drawn() {
        let w = world();
        let view = panel::View::default();
        let ui = ui_of(&w, &view);
        let seen: Vec<usize> = (0..H).filter_map(|y| panel::row_at(&ui, &view, W, H, y)).collect();
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), seen.len(), "two screen rows mapped to one row");

        // Two blocks, not one: the tree scrolls and the dock is pinned beneath
        // it, so the indices jump once where the tree was truncated. Within a
        // block they must run consecutively, or a click is off by a line.
        let jumps = seen.windows(2).filter(|p| p[1] != p[0] + 1).count();
        assert!(jumps <= 1, "the mapping has more than one gap: {seen:?}");

        // And every row that can be clicked can be selected by clicking it.
        for (n, y) in (0..H).filter(|y| panel::row_at(&ui, &view, W, H, *y).is_some()).enumerate() {
            assert_eq!(panel::row_at(&ui, &view, W, H, y), Some(seen[n]));
        }
    }
}
