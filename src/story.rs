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
        // Long on purpose, and not unusually so: the titles in the real store
        // run to a median of sixty-four characters and a tenth of them past a
        // hundred, against the twenty-five a row can draw. A fixture where
        // every title fits is a fixture that cannot show what F is for.
        task(
            "t-002",
            "Plan the demo video: what it shows, in what order, and which of the three patches is worth the first thirty seconds",
            Some("trance"),
            "doing",
        ),
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
        // A label that matches no project, for a pane that stands nowhere.
        workspace("w6", "scratch", false),
    ];

    let agents = vec![
        agent("w1:p1", "w1", "working", "Trance Video"),
        agent("w2:p1", "w2", "idle", "Verb UI"),
        agent("w3:p1", "w3", "working", "◐ Unclaimed explorer"),
        // The other half of the dock: an agent that has stopped and holds
        // nothing. It is standing in a project root, so `f` knows where to send
        // it without anyone having said.
        {
            let mut a = agent("w4:p2", "w4", "idle", "spare hands");
            a.cwd = "~/claude/trance".into();
            a
        },
        // And the case that is actually commonest: an idle agent that resolves
        // to nothing at all. herdr reports where a pane's *shell* started, and
        // an agent launched from the directory above every checkout is standing
        // in no project by that measure — which is why `f` has to be able to
        // ask rather than refuse.
        agent("w6:p1", "w6", "idle", ""),
        // Stopped on a task it has parked with a question on it. herdr calls
        // this idle exactly as it calls the one above idle, and they are not
        // the same thing at all: this one is waiting on an answer that is
        // written down, and that one is waiting for anything to do.
        agent("w3:p2", "w3", "idle", "waiting on the tuning table"),
        // An agent herdr has no state for yet — a pane that has started and
        // not said anything since. Common for a few seconds, and worth its own
        // mark rather than being rounded down to idle.
        {
            let mut a = agent("w5:p2", "w5", "", "just started");
            a.cwd = "~/claude/wsp".into();
            a
        },
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
    // Holding the blocked task, which is what makes its idleness mean
    // something different from anybody else's.
    bindings.insert("w3:p2".to_string(), json!({ "task_id": "t-006" }));

    // Empty on purpose. Standing direction is what `f` writes when it has to
    // ask, so a world that starts with one would hide the question.
    let mandates = BTreeMap::new();

    Snapshot {
        projects,
        tasks,
        bindings,
        pins: BTreeMap::new(),
        mandates,
        // No `claimed_at` anywhere: a live claim prints how long it has been
        // held, and a fixture that says `356d` is a fixture whose age is
        // showing. The panel draws the duration when there is one and says
        // nothing when there is not, which is what these frames show.
        claims: BTreeMap::new(),
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
        Key::KillLine => "^U".into(),
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

    /// The same hunt for a named task. A scene about what a row *says* has to
    /// name the row it means, for the reason `to_pane` does: a count of presses
    /// describes the fixture it was written against and nothing else.
    fn to_task(&mut self, id: &str) -> &mut Self {
        let want = panel::Target::Task(id.to_string());
        loop {
            if self.ui.selected_target() == want {
                return self;
            }
            let before = self.ui.selected_index();
            self.key(Key::Down);
            if self.ui.selected_index() == before {
                panic!("no row for task {id}");
            }
        }
    }

    /// Press `Down` until the cursor is on a particular pane's row. A scene
    /// that means a *specific* agent — the spare one, the one standing nowhere
    /// — has to say which: what a count of presses lands on changes the moment
    /// the fixture gains a pane, and the caption underneath would go on
    /// describing the row that used to be there.
    fn to_pane(&mut self, pane: &str) -> &mut Self {
        let want = panel::Target::Pane(pane.to_string());
        loop {
            if self.ui.selected_target() == want {
                return self;
            }
            let before = self.ui.selected_index();
            self.key(Key::Down);
            if self.ui.selected_index() == before {
                panic!("no row for pane {pane}");
            }
        }
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

    /// The same hunt upwards, for the rows above wherever a verb left the
    /// cursor — the dock is at the foot, and everything a pick is looking for
    /// is above it.
    fn up_to(&mut self, want: panel::RowKind) -> &mut Self {
        loop {
            if self.ui.selected_kind() == want {
                return self;
            }
            let before = self.ui.selected_index();
            self.key(Key::Up);
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
            .scene("The other group", "Shells that resolve nowhere sit at the foot, after the work — a pane with nobody in it is a fact about a place, so it stays in the tree, where places are. Nothing can be added here; herdr owns panes."),
    );

    out.push(
        Driver::new(&w)
            .to_pane("w2:p1")
            .scene("The agents, always on", "Under a rule of its own at the foot: the agents, five of them, in the order the strip is drawn in — what wants you, what is free, what is busy. Pinned, so the tree above scrolls and this does not, because who has stopped is the question you ask between reading anything else and it must not be a keystroke away. The heading counts them all, so the sixth is never silently absent, and 1-9 start here rather than in the tree: a digit you can always see is worth more than one in row order."),
    );

    out.push(
        Driver::new(&w)
            .keys(&[Key::Char('G'), Key::Right])
            .scene("Opening the rest in place", "G to the last row, which is the section's own `⋯`, and `→` opens the tail where it stands — the same gesture as a project past the six-task cap. For anyone who would rather not leave the tree at all; `w` is the other door, and it gives the same agents three lines each instead of one."),
    );

    out.push(
        Driver::new(&w)
            .to_pane("w4:p2")
            .key(Key::Char('c'))
            .scene("Handing work to an idle agent", "`○` is an agent that has stopped holding nothing — a person's worth of attention going spare, and the row the section exists to keep on screen: it sorts above the busy ones for exactly that reason, since there is nothing to do about an agent that is working. `c` on it turns the tree into the picker: choose what it takes."),
    );

    out.push(
        Driver::new(&w)
            .to_pane("w4:p2")
            .key(Key::Char('c'))
            .up_to(panel::RowKind::More)
            .key(Key::Enter)
            .scene("Reaching past the cap", "Every project stops at six tasks and puts the rest behind `⋯`, so a hunt for work to hand over runs into one. `↵` inside a pick takes the row it lands on — and on a row it cannot take but can *open*, it opens it: the tail here, a folded project the same way. The pick is still running; the two tasks that were out of reach are now rows like any other."),
    );

    out.push(
        Driver::new(&w)
            .to_pane("w4:p2")
            .key(Key::Char('f'))
            .scene("Letting it choose for itself", "The other half of the same idea. `c` hands over a task you picked; `f` hands over a *project* and lets the agent pick inside it — the panel types `wsp next` into the pane and leaves. The project comes from the same chain the agent's own `wsp where` would use, so the panel can never send a pane somewhere it would disagree it is. Shells are refused and a working agent is left alone: a sentence typed into the wrong pane is a command."),
    );

    out.push(
        Driver::new(&w)
            .to_pane("w6:p1")
            .key(Key::Char('f'))
            .scene("Asking where it works", "The commonest pane of all: an idle agent standing in no project, because herdr reports where a pane's *shell* started and that is one directory above every checkout. `f` asks rather than refusing — and the answer is written down as a mandate, so the next `f` on that pane goes straight out. Picking a project for an idle agent *is* standing direction; there was never anything else it could mean."),
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
            .key(Key::Char('w'))
            .scene(
                "The agents, not the work",
                "`w` puts every running agent in place of the tree, ordered by what it is waiting for rather than by what has to be done — the one question the tree cannot answer, because an agent with nothing to do has no work to be filed under. The marks are the header strip's, one per row: ← stopped on live work and waiting on you, ■ stopped on a task parked with a question, ● running, ○ spare, · not saying. herdr reports only working or idle; which of the four an idle agent is comes from the task in its hands, which is the half the store knows. The project on the right is what the tree would have said by where it drew the row. Every key still means what it means — ↵ jumps to the terminal, `c` hands it a task, 1-9 are the same hotkeys.",
            ),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('w'))
            .down_to(panel::RowKind::Agent)
            .key(Key::Char('c'))
            .scene(
                "Handing work over from the agents view",
                "The agents view is where you notice an agent with nothing to do, and it is the one view with no work in it to give one. So `c` brings the tree back on its way into the pick — the same switch `R` and `w` already make on each other. The question in the footer is unchanged; what changed is that there is now something on screen that can answer it.",
            ),
    );

    out.push(
        Driver::new(&w)
            .keys(&[Key::Char('w'), Key::Char('c')])
            .scene(
                "Handing work over from the census",
                "`c` on a row in the agents view is `c` on a pane row anywhere else: the tree comes back as the picker and the agent takes whatever you land on. Which is the point of putting the agents in one list — you go looking for who is free, and you are already standing on the row that hands them something.",
            ),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('i'))
            .scene("Showing ids", "i puts the id in front of each title — the thing you type at a shell, next to the thing you read. Off by default because thirteen characters of `t-260815-004` on every row is most of a narrow pane, and only the last three of them differ. The suffix is what `wsp start 004` resolves, so the suffix is what shows, unless another open task shares it and the date is what separates them."),
    );

    out.push(
        Driver::new(&w)
            .to_task("t-002")
            .key(Key::Char('F'))
            .scene("The title in full", "F docks the selected row's title under the tree, wrapped, where the row itself has room for a quarter of it. Reading the rest used to mean ↵, which opens a second pane and takes the cursor out of the tree — a lot of ceremony for one sentence. Three lines whatever is selected, so the tree does not step up and down as the cursor passes between a short title and a long one; six when the title needs them, because a focus panel that cut the title would fail on exactly the rows it exists for."),
    );

    out.push(
        Driver::new(&w)
            .to_task("t-002")
            .key(Key::Char('F'))
            .keys(&[Key::Down; 3])
            .scene("Reading down the tree", "It follows the cursor, so scrolling is how you read: every row on the way past says what it is in full, without opening anything. Short titles leave the rest of the dock blank rather than closing it up — the height is what keeps the rows above from moving while you scroll."),
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
            .to_task("t-002")
            .key(Key::Char('e'))
            .scene("Retitling", "e opens the field holding the whole title, caret at the end — a retitle is nearly always a correction, and an empty field means retyping the sixty characters you meant to keep, from a row too narrow to have shown them to you. The line scrolls to its end, `^U` clears it, and ↵ on an untouched title does nothing rather than writing the title it already has."),
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
        // A still frame has no edit tab, so no section menu.
        columns: Vec::new(),
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

    /// The label wins because it is the only one of the three anybody keeps
    /// current. An agent's terminal title is its opening prompt frozen, so a
    /// pane three tasks on still answered with the first thing it was asked —
    /// confidently, and wrongly, which is the failure a blank would not be.
    #[test]
    fn a_pane_is_named_by_the_name_somebody_is_still_maintaining() {
        use crate::panel::pane_name;

        // `claim` and `wsp say` both write the label, so it answers first.
        assert_eq!(pane_name("reading the claim guard", "◑ Pick up wsp 041", "wsp"), "reading the claim guard");

        // Never named by wsp: the title is the best thing left.
        assert_eq!(pane_name("", "◑ Pick up wsp 041", "wsp"), "◑ Pick up wsp 041");

        // A shell has neither, and where it stands still says something.
        assert_eq!(pane_name("", "", "Trance Video"), "Trance Video");
        assert_eq!(pane_name("", "", ""), "");

        // Whitespace is not a name. A label of spaces used to win outright and
        // draw an agent row with nothing on it at all.
        assert_eq!(pane_name("   ", "◑ Pick up wsp 041", "wsp"), "◑ Pick up wsp 041");
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

    /// Drive the panel the way a person would and hand back what it is
    /// showing: the reducer, then the rebuild the live loop does for it.
    fn showing(snap: &Snapshot, keys: &[Key]) -> (panel::Ui, panel::View) {
        let mut view = panel::View::default();
        let mut ui = ui_of(snap, &view);
        for k in keys {
            if let panel::Effect::Refetch = panel::apply_key(*k, &mut ui, &mut view) {
                panel::refetch_into(&mut ui, snap, &mut view, Some("w0"));
            }
        }
        (ui, view)
    }

    /// Every row of the agents view leads to a terminal, which is what makes
    /// `↵`, `c` and the 1-9 hotkeys go on working inside it. A list of agents
    /// with a project heading in it would put the cursor somewhere none of the
    /// three verbs mean anything.
    #[test]
    fn the_agents_view_is_every_agent_and_nothing_else() {
        let w = world();
        let (mut ui, _view) = showing(&w, &[Key::Char('w')]);

        let running =
            w.panes.iter().filter(|p| !p.agent.is_empty() && p.label != panel::PANEL_LABEL).count();
        let mut kinds = Vec::new();
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            kinds.push(ui.selected_kind());
        }
        assert_eq!(
            kinds.iter().filter(|k| **k == panel::RowKind::Agent).count(),
            running,
            "one agent row per running agent, and no more"
        );
        // Everything else in the view is one of those agents' own lines.
        assert!(kinds.iter().all(|k| matches!(k, panel::RowKind::Agent | panel::RowKind::Detail)));

        // And the cursor only ever lands on the agents: the lines beneath one
        // are the row above said at length, and stopping on them would be the
        // same pane three times over.
        let (mut ui, mut view) = showing(&w, &[Key::Char('w')]);
        for _ in 0..ui.rows_for_test() {
            assert_eq!(ui.selected_kind(), panel::RowKind::Agent, "at row {}", ui.selected_index());
            assert!(matches!(ui.selected_target(), panel::Target::Pane(_)));
            panel::apply_key(Key::Down, &mut ui, &mut view);
        }

        // And back again. `w` is a view, not a door.
        let (ui, _) = showing(&w, &[Key::Char('w'), Key::Char('w')]);
        assert_eq!(ui.rows_for_test(), ui_of(&w, &panel::View::default()).rows_for_test());
    }

    /// herdr says `idle` for four of the panes in the fixture and means four
    /// different things by it. Which one comes from the task in the agent's
    /// hands, which is the half of the answer the store is holding.
    #[test]
    fn what_an_idle_agent_is_waiting_for_comes_from_what_it_holds() {
        let w = world();
        let (ui, _view) = showing(&w, &[Key::Char('w')]);
        let lines: Vec<String> =
            (0..ui.rows_for_test()).map(|i| panel::render_row_for_test(&ui, i, W).text()).collect();
        let row = |name: &str| -> String {
            lines.iter().find(|l| l.contains(name)).unwrap_or_else(|| panic!("no row for {name}")).clone()
        };

        // Idle, holding a task that is still doing: you are the blocker.
        assert!(row("Verb UI").contains(panel::glyph::NEEDS_YOU));
        // Idle, holding a task parked with a question on it — waiting on an
        // answer that is at least written down.
        assert!(row("waiting on the tuning").contains(panel::glyph::BLOCKED));
        // Idle, holding nothing at all: a person's worth of attention going spare.
        assert!(row("spare hands").contains(panel::glyph::IDLE));
        // No state from herdr yet, and nothing here pretends otherwise.
        assert!(row("just started").contains(panel::glyph::QUIET));
        assert!(row("Trance Video").contains(panel::glyph::WORKING));

        // What wants you is at the top, because that is what the list is read
        // for. Sorted by state, so the first row is never a working agent.
        assert!(lines[0].contains(panel::glyph::NEEDS_YOU), "{}", lines[0]);
    }

    /// The strip is the census, not a summary of whatever the tree is showing.
    /// A header that went quiet under a filter is one you learn to distrust.
    #[test]
    fn the_header_strip_is_the_same_whatever_the_view() {
        let w = world();
        let marks = |ui: &panel::Ui, view: &panel::View| -> String {
            panel::frame(ui, view, W, H)[0]
                .text()
                .chars()
                .filter(|c| "←■●○·".contains(*c))
                .collect()
        };
        // One mark per agent, ordered as the list is: wants you, blocked,
        // spare, working, quiet.
        let want = "←■○○●●·";

        let (ui, view) = showing(&w, &[]);
        assert_eq!(marks(&ui, &view), want, "at rest");
        let (ui, view) = showing(&w, &[Key::Char('R')]);
        assert_eq!(marks(&ui, &view), want, "under the review filter");
        let (ui, view) = showing(&w, &[Key::Char('w')]);
        assert_eq!(marks(&ui, &view), want, "in the agents view");

        // Nobody running: the strip has nothing to say and says that instead of
        // drawing a bare zero.
        let (ui, view) = showing(&quiet_world(), &[]);
        assert!(panel::frame(&ui, &view, W, H)[0].text().contains("no agents"));
    }

    /// The lines under an agent are that agent said at length, so a click on
    /// one lands on the row they belong to rather than on nothing. Doing
    /// nothing would be defensible for a rule or a blank; here the pointer is
    /// on the agent's own words.
    #[test]
    fn clicking_an_agents_own_lines_selects_the_agent() {
        let w = world();
        let (mut ui, mut view) = showing(&w, &[Key::Char('w')]);
        // The first agent's second line, two screen rows under the header.
        let head = 2;
        let detail = head + 1;
        assert_eq!(panel::row_at(&ui, &view, W, H, detail), Some(1), "the line under the first");
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 2, detail), panel::Hit::Select);
        assert_eq!(ui.selected_index(), 0, "the agent it belongs to");
        assert_eq!(ui.selected_kind(), panel::RowKind::Agent);
    }

    /// The strip is a row of single columns, and each one is an agent. Nothing
    /// else on the panel is clickable without being a row, so the arithmetic
    /// that draws it has to be the arithmetic that reads the click — otherwise
    /// pointing at the `←` focuses the pane beside the one you meant.
    #[test]
    fn clicking_a_mark_in_the_strip_goes_to_that_agent() {
        let w = world();
        let (mut ui, mut view) = showing(&w, &[]);

        // Column by column, against the census the strip was drawn from.
        for (i, (_, agent)) in ui.census_for_test().into_iter().enumerate() {
            let hit = panel::click(&mut ui, &mut view, W, H, 4 + i, 0);
            match hit {
                panel::Hit::Focus(a) => assert_eq!(a.pane(), agent.pane(), "mark {i}"),
                other => panic!("mark {i} should go to a terminal, got {other:?}"),
            }
        }
        // The name is not a mark, and neither is the gap before the total.
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 0, 0), panel::Hit::Nothing);
        assert_eq!(panel::click(&mut ui, &mut view, W, H, W - 1, 0), panel::Hit::Nothing);
        // And clicking one does not move the cursor: there is no row to move to.
        assert_eq!(ui.selected_index(), 0);
    }

    /// A strip too long for the pane ends in `⋯`, and the rest of the agents is
    /// exactly what the agents view is — so the mark that stands for them opens
    /// it rather than doing nothing.
    #[test]
    fn clicking_the_rest_of_a_clipped_strip_opens_the_agents() {
        let w = world();
        let (mut ui, mut view) = showing(&w, &[]);
        // Narrow enough that seven marks cannot fit beside the name and total.
        let narrow = 10;
        let clipped = panel::frame(&ui, &view, narrow, H)[0].text();
        assert!(clipped.contains(panel::glyph::MORE), "{clipped}");
        let at = clipped.chars().position(|c| c.to_string() == panel::glyph::MORE).unwrap();
        assert_eq!(panel::click(&mut ui, &mut view, narrow, H, at, 0), panel::Hit::Rest);
    }

    /// The section keeps a few on screen and says how many there are, so the
    /// ones it cannot fit are never silently absent.
    #[test]
    fn the_section_keeps_five_and_counts_them_all() {
        let w = world();
        let (mut ui, mut view) = showing(&w, &[]);
        let census = ui.census_for_test().len();
        assert!(census > 5, "the fixture has to outrun the cap to test it");

        let docked = ui.dock_for_test();
        let mut agents = 0;
        for i in ui.rows_for_test() - docked..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_kind() == panel::RowKind::Agent {
                agents += 1;
            }
        }
        assert_eq!(agents, 5, "five agents pinned, and a `⋯` for the rest");
        // The heading counts every one of them, whatever it could draw.
        let head = panel::render_row_for_test(&ui, ui.rows_for_test() - docked, W);
        assert!(head.text().contains("agents") && head.text().contains(&census.to_string()));

        // `→` on the `⋯` opens the tail in place, exactly as a project's does.
        panel::apply_key(Key::Char('G'), &mut ui, &mut view);
        if let panel::Effect::Refetch = panel::apply_key(Key::Right, &mut ui, &mut view) {
            panel::refetch_into(&mut ui, &w, &mut view, Some("w0"));
        }
        let docked = ui.dock_for_test();
        let mut agents = 0;
        for i in ui.rows_for_test() - docked..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_kind() == panel::RowKind::Agent {
                agents += 1;
            }
        }
        assert_eq!(agents, census, "all of them, once asked");
    }

    /// A digit is an address for a terminal. The pinned rows take them first
    /// because they are the rows that are always on screen — and a pane drawn
    /// twice, under its task and again in the section, must not spend two.
    #[test]
    fn one_terminal_takes_one_digit_and_the_pinned_ones_go_first() {
        let w = world();
        let (mut ui, _view) = showing(&w, &[]);
        // Which row carries which digit, read off the drawn lines.
        let mut seen: Vec<(u8, usize, String)> = Vec::new();
        for i in 0..ui.rows_for_test() {
            let line = panel::render_row_for_test(&ui, i, W).text();
            let Some(d) = line.chars().next().and_then(|c| c.to_digit(10)) else { continue };
            ui.select_for_test(i);
            let panel::Target::Pane(p) = ui.selected_target() else { continue };
            seen.push((d as u8, i, p));
        }
        let panes: Vec<&String> = seen.iter().map(|(_, _, p)| p).collect();
        let mut uniq = panes.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(panes.len(), uniq.len(), "a terminal took two digits: {seen:?}");

        // 1 is in the section at the foot, not on the first agent in the tree
        // — and the tree row for that same pane carries no digit of its own.
        let tree = ui.rows_for_test() - ui.dock_for_test();
        let (_, at, _) = seen.iter().find(|(d, _, _)| *d == 1).expect("no 1");
        assert!(*at >= tree, "the first digit belongs to the pinned section");
    }

    /// Two switches, one state. A filter left on under a view that does not use
    /// it is a footer saying `review only` over a list of panes.
    #[test]
    fn the_agents_view_and_the_review_filter_each_put_the_other_away() {
        let w = world();
        let foot = |ui: &panel::Ui, view: &panel::View| panel::frame(ui, view, W, H)[H - 2].text();

        let (ui, view) = showing(&w, &[Key::Char('R')]);
        assert!(foot(&ui, &view).contains("review only"));

        let (ui, view) = showing(&w, &[Key::Char('R'), Key::Char('w')]);
        let f = foot(&ui, &view);
        assert!(f.contains("agents") && !f.contains("review only"), "{f}");

        let (ui, view) = showing(&w, &[Key::Char('w'), Key::Char('R')]);
        let f = foot(&ui, &view);
        assert!(f.contains("review only") && !f.contains("agents"), "{f}");
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

    /// Ed: "overscrolling resets the position to that of the last agent, when
    /// scrolling down, making it very hard to use". This is why.
    ///
    /// The cursor is kept across a rebuild by what the row *is*, never by the
    /// slot it was in — but the panel draws a pane twice on purpose, under the
    /// task it claimed and again in the section pinned at the foot, and the
    /// first row matching is always the one in the tree. So scrolling down off
    /// the end of the tree put the cursor in the dock, and a quarter of a
    /// second later the rebuild moved it up to wherever that agent's work
    /// happened to sit. Scrolling down jumped the view up, and went on doing it
    /// for as long as the cursor stayed there.
    #[test]
    fn a_cursor_in_the_dock_is_not_dragged_back_up_to_the_tree() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        let tree = ui.rows_for_test() - ui.dock_for_test();

        // Down until it is over the line. This is the overscroll: the tree has
        // run out and the dock is what is under the cursor.
        for _ in 0..ui.rows_for_test() {
            if ui.selected_index() >= tree {
                break;
            }
            panel::apply_key(Key::Down, &mut ui, &mut view);
        }
        assert!(ui.selected_index() >= tree, "the cursor walks off the tree into the dock");

        // The fixture only tests anything if this pane is one of the ones drawn
        // twice — otherwise there is nothing for the search to pick wrongly.
        let target = ui.selected_target();
        let drawn = ui.rows_for_target(&target);
        assert!(
            drawn.len() > 1 && drawn[0] < tree,
            "this pane has to appear in the tree as well as the dock: {drawn:?}",
        );

        let was = ui.selected_index();
        panel::refetch_into(&mut ui, &w, &mut view, Some("w0"));
        assert_eq!(ui.selected_index(), was, "a rebuild left the cursor where it was");
        assert!(
            ui.selected_index() >= ui.rows_for_test() - ui.dock_for_test(),
            "and left it in the dock rather than on the tree copy of the same pane",
        );
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
                // The focus dock is the harder of the two: the map is a fixed
                // block, and this one is as tall as whatever the cursor is on,
                // so it changes height under the very keypress being swept.
                for focus in [false, true] {
                    let mut view = panel::View::default();
                    view.set_help_for_test(help);
                    view.set_focus_for_test(focus);
                    let mut ui = ui_of(&w, &view);
                    for n in 0..26 {
                        let drawn = panel::frame(&ui, &view, W, h);
                        for y in 0..h {
                            let Some(i) = panel::row_at(&ui, &view, W, h, y) else { continue };
                            let want = panel::spans_of(&panel::render_row_for_test(&ui, i, W));
                            let got = panel::spans_of(&drawn[y]);
                            assert_eq!(
                                got, want,
                                "h={h} help={help} focus={focus} after {n} down: screen row {y} maps to tree row {i}, not what is drawn"
                            );
                        }
                        panel::apply_key(Key::Down, &mut ui, &mut view);
                    }
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
            // Whichever of these the cursor happens to be parked on is a click
            // that activates, and this is a test about the other kind. Asked
            // rather than assumed: which screen row the cursor lands on after
            // fourteen presses is a fact about the fixture, and the fixture
            // gains a pane whenever the panel gains a case worth drawing.
            if i == ui.selected_index() {
                continue;
            }
            let (mut after_ui, mut after_view) = (ui.clone(), view.clone());
            assert_eq!(panel::click(&mut after_ui, &mut after_view, W, H, 0, y), panel::Hit::Select);
            assert_eq!(
                panel::row_at(&after_ui, &after_view, W, H, y),
                Some(i),
                "clicking screen row {y} moved tree row {i} out from under the pointer"
            );
            // And the row it selected is the row that was clicked, so the
            // second click activates rather than selecting something else.
            assert_eq!(panel::click(&mut after_ui, &mut after_view, W, H, 0, y), panel::Hit::Activate);
        }
    }

    /// The wheel moves the selection, the way `j`/`k` do — the tree scrolls by
    /// holding the cursor near the middle, so moving the cursor *is* the
    /// scroll. A view offset of its own left the highlight behind, which is
    /// not what the panel has ever done.
    #[test]
    fn the_wheel_moves_the_selection() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        let start = ui.selected_index();
        panel::wheel(&mut ui, &mut view, false);
        assert!(ui.selected_index() > start, "down should move the cursor down");
        let mid = ui.selected_index();
        panel::wheel(&mut ui, &mut view, true);
        assert!(ui.selected_index() < mid, "up should move it back");
    }

    /// And the cursor is on the pane throughout, which is what makes the next
    /// keystroke continue from where you are looking.
    #[test]
    fn scrolling_keeps_the_cursor_on_the_pane() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        let visible = |ui: &panel::Ui, view: &panel::View| {
            (0..H).any(|y| panel::row_at(ui, view, W, H, y) == Some(ui.selected_index()))
        };
        for n in 0..12 {
            panel::wheel(&mut ui, &mut view, false);
            assert!(visible(&ui, &view), "after {} scrolls down", n + 1);
        }
        for n in 0..20 {
            panel::wheel(&mut ui, &mut view, true);
            assert!(visible(&ui, &view), "after {} scrolls up", n + 1);
        }
    }

    /// What the focus dock is drawing, read off the frame the way you would
    /// read it off the pane: the lines between the last two rules.
    fn dock_text(frame: &[panel::Line], w: usize) -> String {
        let rule = "─".repeat(w);
        let rules: Vec<usize> =
            frame.iter().enumerate().filter(|(_, l)| l.text() == rule).map(|(i, _)| i).collect();
        assert!(rules.len() >= 2, "no focus dock: only {} rules", rules.len());
        let (top, bottom) = (rules[rules.len() - 2], rules[rules.len() - 1]);
        frame[top + 1..bottom]
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The whole point: the sentence the row had to cut is on the pane.
    #[test]
    fn the_focus_dock_holds_the_title_the_row_cut() {
        let w = world();
        let mut d = Driver::new(&w);
        d.to_task("t-002").key(Key::Char('F'));
        let title = "Plan the demo video: what it shows, in what order, and which of the three \
                     patches is worth the first thirty seconds";

        let drawn = panel::frame(&d.ui, &d.view, W, H);
        assert_eq!(dock_text(&drawn, W), title);

        // And the row it came from cannot say it, which is why the dock exists.
        let row = panel::render_row_for_test(&d.ui, d.ui.selected_index(), W).text();
        assert!(row.contains('…'), "the row is not truncated: {row}");
    }

    /// It follows the cursor — that is the difference between this and opening
    /// the task. Whatever the cursor lands on, the dock is saying it.
    #[test]
    fn the_focus_dock_follows_the_cursor() {
        let w = world();
        let mut d = Driver::new(&w);
        d.key(Key::Char('F'));
        for n in 0..20 {
            d.key(Key::Down);
            let drawn = panel::frame(&d.ui, &d.view, W, H);
            let want = panel::full_text_for_test(&d.ui, d.ui.selected_index());
            assert!(
                dock_text(&drawn, W).starts_with(&want),
                "after {} down the dock says something else",
                n + 1
            );
        }
    }

    /// Three lines whatever is selected, six when the title needs them. The
    /// floor is what keeps the tree still while the cursor moves: a dock that
    /// shrank to fit every short title would hand the tree a row back and take
    /// it away again on every keypress.
    #[test]
    fn the_focus_dock_keeps_its_floor_and_grows_only_for_a_long_title() {
        let w = world();
        let rows_of = |id: &str| {
            let mut d = Driver::new(&w);
            d.to_task(id).key(Key::Char('F'));
            let drawn = panel::frame(&d.ui, &d.view, W, H);
            let rule = "─".repeat(W);
            let rules: Vec<usize> =
                drawn.iter().enumerate().filter(|(_, l)| l.text() == rule).map(|(i, _)| i).collect();
            rules[rules.len() - 1] - rules[rules.len() - 2] - 1
        };
        assert_eq!(rows_of("t-020"), 3, "a four-word title still gets three lines");
        assert_eq!(rows_of("t-002"), 4, "a title that needs four lines gets four");
    }

    /// It costs the tree exactly what it takes, and gives it back when it goes.
    #[test]
    fn the_focus_dock_takes_its_rows_from_the_tree() {
        let w = world();
        let on_screen = |d: &Driver| {
            (0..H).filter(|y| panel::row_at(&d.ui, &d.view, W, H, *y).is_some()).count()
        };
        let mut d = Driver::new(&w);
        d.to_task("t-002");
        let before = on_screen(&d);
        d.key(Key::Char('F'));
        assert_eq!(on_screen(&d), before - 5, "a four-line dock and its rule");
        d.key(Key::Char('F'));
        assert_eq!(on_screen(&d), before, "F again gives the rows back");
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

    // ---- telling an agent -----------------------------------------------

    /// Put the cursor on the row for a pane. By target rather than by counting
    /// presses: a fixture that gains a task must not silently move the row a
    /// test thought it was aiming at.
    fn on_pane(ui: &mut panel::Ui, pane: &str) {
        let want = panel::Target::Pane(pane.to_string());
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == want {
                return;
            }
        }
        panic!("no row for pane {pane}");
    }

    fn press(ui: &mut panel::Ui, view: &mut panel::View, c: char) -> panel::Effect {
        panel::apply_key(Key::Char(c), ui, view)
    }

    /// Open the agents section past the five it keeps on screen, the way `→` on
    /// its `⋯` does. A test that wants the sixth agent has to ask for it exactly
    /// as a person would — and the last overflow row in the tree is the
    /// section's, since it is the last thing in the list.
    fn show_all_agents(snap: &Snapshot, ui: &mut panel::Ui, view: &mut panel::View) {
        let mut probe = ui.clone();
        let more = (0..ui.rows_for_test())
            .filter(|i| {
                probe.select_for_test(*i);
                probe.selected_kind() == panel::RowKind::More
            })
            .next_back()
            .expect("the agents section has no overflow row");
        ui.select_for_test(more);
        step(snap, ui, view, Key::Right);
    }

    /// A key and the rebuild the live loop does for it. `press` is enough while
    /// a test only asks what a key returned; anything that asks which rows are
    /// there afterwards has to go through this, because the folds decide the
    /// rows and the reducer only says that they moved.
    fn step(snap: &Snapshot, ui: &mut panel::Ui, view: &mut panel::View, k: Key) -> panel::Effect {
        let e = panel::apply_key(k, ui, view);
        if matches!(e, panel::Effect::Refetch) {
            panel::refetch_into(ui, snap, view, Some("w0"));
        }
        e
    }

    fn has(ui: &panel::Ui, want: &panel::Target) -> bool {
        let mut probe = ui.clone();
        (0..ui.rows_for_test()).any(|i| {
            probe.select_for_test(i);
            probe.selected_target() == *want
        })
    }

    /// Put the cursor on the first row of a kind, without counting presses: a
    /// count rots the moment the fixture gains a task.
    fn on_kind(ui: &mut panel::Ui, want: panel::RowKind) {
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_kind() == want {
                return;
            }
        }
        panic!("no {want:?} row");
    }

    /// Handing work to an agent is a hunt through the tree, and every project
    /// in it stops at six tasks with the rest behind a `⋯`. Inside a pick `↵`
    /// was the pick's own key and nothing else, so the one row whose entire
    /// purpose is to be opened was the one row the pick refused — and any task
    /// past the cap could not be handed to anybody.
    #[test]
    fn the_overflow_row_still_opens_while_a_pick_is_hunting() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w4:p2");
        press(&mut ui, &mut view, 'c');

        // Eight tasks in `wsp` against a cap of six: the last two are behind ⋯.
        let past_the_cap = panel::Target::Task("t-108".into());
        assert!(!has(&ui, &past_the_cap), "the tail starts folded away");

        on_kind(&mut ui, panel::RowKind::More);
        step(&w, &mut ui, &mut view, Key::Enter);
        assert!(has(&ui, &past_the_cap), "↵ on ⋯ has to open the tail, mid-pick or not");

        // And the pick is still live: opening a row is not choosing one.
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_target() == past_the_cap {
                break;
            }
        }
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["claim", "t-108", "--pane", "w4:p2"]);
            }
            _ => panic!("the pick outlived the row it opened, and should claim"),
        }
    }

    /// The same key on a folded project. A pick cannot take one — a pane takes
    /// a task — and refusing there left the tree's own carets as the only way
    /// in, which is a second thing to know at the moment you are hunting for a
    /// row rather than reading a key map.
    #[test]
    fn a_folded_project_opens_under_a_pick_too() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_kind(&mut ui, panel::RowKind::Project);
        step(&w, &mut ui, &mut view, Key::Left);
        let under = panel::Target::Task("t-001".into());
        assert!(!has(&ui, &under), "folded, so its work is away");

        on_pane(&mut ui, "w4:p2");
        press(&mut ui, &mut view, 'c');
        on_kind(&mut ui, panel::RowKind::Project);
        step(&w, &mut ui, &mut view, Key::Enter);
        assert!(has(&ui, &under), "↵ opens it, because the pick is looking for what is inside");
    }

    /// `w` puts the agents in place of the tree, which is exactly where you
    /// notice an agent with nothing to do — and it is the one view with no work
    /// in it to hand over. Starting the pick there aimed it at a list of panes:
    /// every row refused, and no key brought the tree back.
    #[test]
    fn picking_for_an_agent_brings_the_work_back_into_view() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        step(&w, &mut ui, &mut view, Key::Char('w'));
        assert!(!has(&ui, &panel::Target::Task("t-020".into())), "the agents, not the work");

        // `c`: which task does it take?
        on_pane(&mut ui, "w4:p2");
        step(&w, &mut ui, &mut view, Key::Char('c'));
        assert!(has(&ui, &panel::Target::Task("t-020".into())), "a pick needs work to aim at");
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Task("t-020".into()) {
                break;
            }
        }
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["claim", "t-020", "--pane", "w4:p2"]);
            }
            _ => panic!("the pick should claim the task it landed on"),
        }
    }

    /// And `f` on an agent standing nowhere, which asks the same question one
    /// level up: it lands on a project, and the agents view has no projects
    /// either.
    #[test]
    fn sending_an_agent_looking_brings_the_projects_back_into_view() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        step(&w, &mut ui, &mut view, Key::Char('w'));
        on_pane(&mut ui, "w6:p1");
        step(&w, &mut ui, &mut view, Key::Char('f'));
        assert!(
            has(&ui, &panel::Target::Project("verb".into())),
            "the pick asks which project, so the projects have to be there",
        );
    }

    /// Put the cursor on a row by what it is, the way [`on_pane`] does.
    fn on_target(ui: &mut panel::Ui, want: panel::Target) {
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == want {
                return;
            }
        }
        panic!("no row for {want:?}");
    }

    /// `O` and `S` are one command with one flag between them, and the flag is
    /// the whole difference between a place to work and a colleague. The panel
    /// works out neither the title nor the root: `wsp spawn` resolves both from
    /// the store, which is what stopped `O` opening a workspace in the panel's
    /// own directory for every task under a project whose root is its parent's.
    #[test]
    fn o_and_s_are_the_same_verb_with_and_without_somebody_in_it() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_target(&mut ui, panel::Target::Task("t-003".into()));
        match press(&mut ui, &mut view, 'O') {
            panel::Effect::Spawn { argv, .. } => assert_eq!(argv, ["spawn", "t-003"]),
            _ => panic!("O on a task opens a workspace for it"),
        }
        match press(&mut ui, &mut view, 'S') {
            panel::Effect::Spawn { argv, .. } => {
                assert_eq!(argv, ["spawn", "t-003", "--agent"])
            }
            _ => panic!("S on a task puts an agent on it"),
        }

        // A project takes both too — a workspace rooted in its checkout, and an
        // agent standing in it with nothing claimed. `-p`, because a project id
        // and a task id are resolved differently and guessing between them is
        // the panel's job here rather than the CLI's: the panel knows which row
        // it is standing on.
        on_target(&mut ui, panel::Target::Project("verb".into()));
        match press(&mut ui, &mut view, 'S') {
            panel::Effect::Spawn { argv, .. } => {
                assert_eq!(argv, ["spawn", "-p", "verb", "--agent"])
            }
            _ => panic!("S on a project opens one there"),
        }

        // The inbox is a heading, not a place. Neither key has anything to make
        // a workspace out of, and saying so beats opening one in the dark.
        on_target(&mut ui, panel::Target::Inbox);
        assert!(matches!(press(&mut ui, &mut view, 'S'), panel::Effect::None));
        assert!(matches!(press(&mut ui, &mut view, 'O'), panel::Effect::None));
    }

    /// The dock's own verb: the panel works out what the pane is for and types
    /// `wsp next` into it. The project is named in the sentence rather than
    /// left to the agent's own resolution, so the two can never disagree.
    #[test]
    fn f_sends_an_idle_agent_looking_in_its_own_project() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w4:p2");
        match press(&mut ui, &mut view, 'f') {
            panel::Effect::Tell(t) => {
                assert_eq!(t.pane, "w4:p2");
                assert!(t.text.contains("wsp next -p trance"), "said: {}", t.text);
            }
            _ => panic!("f on an idle unassigned agent should tell it something"),
        }
    }

    /// Standing direction beats the directory. A pane mandated to one project
    /// while sitting in another's checkout is *for* the mandate — and the panel
    /// has to agree with the answer `wsp where` would give inside that pane, or
    /// it sends an agent to work somewhere it will refuse to be.
    #[test]
    fn a_mandate_beats_where_the_pane_is_standing() {
        let mut w = world();
        w.mandates.insert("w4".into(), json!({ "project": "verb" }));
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w4:p2");
        match press(&mut ui, &mut view, 'f') {
            panel::Effect::Tell(t) => {
                assert!(t.text.contains("wsp next -p verb"), "said: {}", t.text);
            }
            _ => panic!("a mandated pane is still tellable"),
        }
    }

    /// Two panes nothing may be typed into: a shell would run the sentence as a
    /// command, and a working agent's prompt may not be a prompt at all.
    #[test]
    fn nothing_is_typed_into_a_shell_or_a_busy_agent() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_pane(&mut ui, "w5:p1"); // a shell standing in ~/claude/wsp
        assert!(matches!(press(&mut ui, &mut view, 'f'), panel::Effect::None));

        show_all_agents(&w, &mut ui, &mut view);
        on_pane(&mut ui, "w3:p1"); // an agent working, holding no task
        assert!(matches!(press(&mut ui, &mut view, 'f'), panel::Effect::None));

        on_pane(&mut ui, "w1:p1"); // an agent already holding t-001
        assert!(matches!(press(&mut ui, &mut view, 'f'), panel::Effect::None));
    }

    /// The other door to the same place. A claim is a fact in the store and
    /// nothing at all in the pane it names, so handing a task to an idle agent
    /// has to carry the sentence that tells it.
    #[test]
    fn claiming_onto_an_idle_agent_tells_it_what_it_now_holds() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        // `c` on a task turns the tree into the picker; the pick lands on a pane.
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Task("t-020".into()) {
                break;
            }
        }
        press(&mut ui, &mut view, 'c');
        on_pane(&mut ui, "w4:p2");
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, then, .. } => {
                assert_eq!(argv, vec!["claim", "t-020", "--pane", "w4:p2"]);
                let t = then.expect("an idle agent handed a task is told about it");
                assert_eq!(t.pane, "w4:p2");
                assert!(t.text.contains("t-020"), "said: {}", t.text);
            }
            _ => panic!("the pick should run a claim"),
        }
    }

    /// The commonest pane there is: an agent that resolves to no project,
    /// because herdr reports where its *shell* started and that is one
    /// directory above every checkout. Refusing there made `f` useless, so it
    /// asks — and writes the answer down as a mandate, which is what stops it
    /// asking twice.
    #[test]
    fn an_agent_that_stands_nowhere_is_asked_where_it_works() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w6:p1");

        // No sentence yet — the panel does not know what to say.
        assert!(matches!(press(&mut ui, &mut view, 'f'), panel::Effect::None));

        // The tree is now the picker, and a project answers it.
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Project("verb".into()) {
                break;
            }
        }
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, then, .. } => {
                assert_eq!(argv, vec!["mandate", "verb", "-w", "w6"]);
                let t = then.expect("and it is told to go and look");
                assert_eq!(t.pane, "w6:p1");
                assert!(t.text.contains("wsp next -p verb"), "said: {}", t.text);
            }
            _ => panic!("the pick should set the mandate"),
        }
    }

    /// The pick refuses what it cannot answer with. Landing on a task or the
    /// inbox is not a project, and `wsp next` scoped to neither is `wsp next`
    /// over everything — which is not what was asked.
    #[test]
    fn only_a_project_answers_where_it_works() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w6:p1");
        press(&mut ui, &mut view, 'f');
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Task("t-020".into()) {
                break;
            }
        }
        assert!(matches!(panel::apply_key(Key::Enter, &mut ui, &mut view), panel::Effect::None));
    }

    /// A pick has no status to refuse on: a `Target::Task` is an id and
    /// nothing else, so the tree offers blocked and finished work exactly as it
    /// offers anything else. The CLI is the only half that can tell, and
    /// carrying `--force` is what turns its refusal into the next question
    /// instead of a message that scrolls out of the footer.
    #[test]
    fn a_claim_from_the_panel_carries_the_override_it_might_need() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Task("t-020".into()) {
                break;
            }
        }
        press(&mut ui, &mut view, 'c');
        on_pane(&mut ui, "w4:p2");
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, escalate, .. } => {
                assert_eq!(argv, vec!["claim", "t-020", "--pane", "w4:p2"]);
                assert_eq!(
                    escalate.expect("a refused claim is worth asking about"),
                    vec!["claim", "t-020", "--pane", "w4:p2", "--force"]
                );
            }
            _ => panic!("the pick should run a claim"),
        }
    }

    /// And the picks that cannot be refused carry nothing. A mandate the CLI
    /// always accepts would only ever escalate into a y/n nobody sees.
    #[test]
    fn a_pick_with_nothing_to_refuse_on_offers_no_override() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w6:p1");
        press(&mut ui, &mut view, 'f');
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Project("verb".into()) {
                break;
            }
        }
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, escalate, .. } => {
                assert_eq!(argv, vec!["mandate", "verb", "-w", "w6"]);
                assert!(escalate.is_none());
            }
            _ => panic!("the pick should set the mandate"),
        }
    }

    /// The claim still lands on a busy agent — it is only the typing that is
    /// withheld, because the store is safe to change and a pane in the middle
    /// of something is not.
    #[test]
    fn claiming_onto_a_busy_agent_still_claims_and_says_nothing() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Task("t-020".into()) {
                break;
            }
        }
        press(&mut ui, &mut view, 'c');
        // Past the section's cap, so the pick has to open it — which is the
        // other half of the same rule: every agent is reachable from inside a
        // pick, or some of them can never be handed anything.
        show_all_agents(&w, &mut ui, &mut view);
        on_pane(&mut ui, "w3:p1");
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, then, .. } => {
                assert_eq!(argv, vec!["claim", "t-020", "--pane", "w3:p1"]);
                assert!(then.is_none(), "a working agent is not typed into");
            }
            _ => panic!("the pick should run a claim"),
        }
    }

    /// A project that holds nothing keeps its row. The quiet-branch filter is
    /// about work you are not looking at, and a project with no work at all is
    /// not that: hiding it takes the row `a`, `X` and `O` are pressed on out of
    /// the tree, so retiring a project's last task leaves the project itself
    /// unreachable from the panel that removed it.
    #[test]
    fn a_project_left_empty_keeps_its_row() {
        // Nobody running: an agent standing in a project keeps its row up on
        // its own, and this is about the row a project has when it has only
        // itself.
        let mut w = quiet_world();
        // What `X` on the last two tasks under `trance` leaves behind.
        w.tasks.retain(|t| t.project.as_deref() != Some("trance"));

        let view = panel::View::default();
        let ui = ui_of(&w, &view);
        assert!(
            !ui.rows_for_target(&panel::Target::Project("trance".into())).is_empty(),
            "the project the tasks were removed from is still in the tree"
        );

        // And the filter it is not: a project whose work is all finished goes
        // on being folded away until `show_done` asks for it.
        let mut w = quiet_world();
        for t in w.tasks.iter_mut().filter(|t| t.project.as_deref() == Some("trance")) {
            t.status_raw = "done".into();
        }
        let ui = ui_of(&w, &view);
        assert!(
            ui.rows_for_target(&panel::Target::Project("trance".into())).is_empty(),
            "finished work is quiet, not empty"
        );
    }

    /// Put the cursor on a task without counting presses, for a test that is
    /// about a particular title rather than about a particular row.
    fn on_task(ui: &mut panel::Ui, id: &str) {
        let want = panel::Target::Task(id.to_string());
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_target() == want {
                return;
            }
        }
        panic!("no row for task {id}");
    }

    /// A retitle is nearly always a correction — a word swapped, a clause
    /// added — and the prompt used to open empty, so keeping fifty-nine of
    /// sixty characters meant retyping all sixty. From a row that was too
    /// narrow to show you the title in the first place.
    #[test]
    fn retitling_opens_holding_the_title_it_is_changing() {
        let w = world();
        let title = w.tasks.iter().find(|t| t.id == "t-002").expect("t-002").title.clone();

        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_task(&mut ui, "t-002");
        panel::apply_key(Key::Char('e'), &mut ui, &mut view);

        // The title outruns the pane by a long way, so what is on screen is
        // its tail — the end being where the caret is, and where typing goes.
        let shown = panel::frame(&ui, &view, W, H)[H - 1].text();
        let tail: String = title.chars().skip(title.chars().count() - 12).collect();
        assert!(shown.contains(&tail), "the prompt should hold the title: {shown}");

        // Sent back untouched it is not a rename at all. The value is already
        // in the buffer, so `↵` would otherwise write the title the task has —
        // a log line, an event and a commit recording a keystroke.
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::None => {}
            _ => panic!("an unchanged title should not run a rename"),
        }

        // Changed by one character, and it runs — carrying the whole title,
        // not the character that was typed.
        on_task(&mut ui, "t-002");
        panel::apply_key(Key::Char('e'), &mut ui, &mut view);
        panel::apply_key(Key::Char('!'), &mut ui, &mut view);
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv[0], "rename");
                assert_eq!(argv[1], "t-002");
                assert_eq!(argv[2], format!("{title}!"));
            }
            _ => panic!("a changed title should run a rename"),
        }

        // And `ctrl-u` is the way out of a value you did not want: one key,
        // against sixty backspaces.
        on_task(&mut ui, "t-002");
        panel::apply_key(Key::Char('e'), &mut ui, &mut view);
        panel::apply_key(Key::KillLine, &mut ui, &mut view);
        let shown = panel::frame(&ui, &view, W, H)[H - 1].text();
        assert!(!shown.contains(&tail), "ctrl-u should clear the line: {shown}");
    }
}
