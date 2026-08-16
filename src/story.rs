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

/// Neither `high` nor `low` is common, and a fixture where every task is
/// `normal` cannot show either mark — nor the thing they are for, which is one
/// task sitting above or below its neighbours in the same project.
fn prio(mut t: Task, level: &str) -> Task {
    t.priority_raw = level.to_string();
    t
}

/// Tags of a task's own, as against the ones it inherits from its project.
/// `t` can only change these, so a fixture with none cannot show what its
/// prompt opens holding.
fn tagged(mut t: Task, tags: &[&str]) -> Task {
    t.tags = tags.iter().map(|s| s.to_string()).collect();
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
    // A name that is not the slug, which in the real store is every project:
    // the tree draws `wsp` and `wsp project show` leads with this. `e` on a
    // project row changes this string and cannot change the other one.
    projects[6].name = "wsp control plane".into();
    // Tags live on projects at least as much as on tasks, and the picker
    // offers the whole vocabulary — so a fixture whose only tags are the ones
    // already on the task under test cannot show the half of it that matters:
    // adding one you have not typed.
    projects[2].tags = vec!["dsp".into(), "synth".into()];
    projects[6].tags = vec!["rust".into(), "herdr".into()];

    let mut tasks = vec![
        tagged(
            task("t-001", "Apply reverb fixes from the tuning table", Some("trance"), "doing"),
            &["dsp", "release"],
        ),
        // Long on purpose, and not unusually so: the titles in the real store
        // run to a median of sixty-four characters and a tenth of them past a
        // hundred, against the twenty-five a row can draw. A fixture where
        // every title fits is a fixture that cannot show what F is for.
        prio(
            task(
                "t-002",
                "Plan the demo video: what it shows, in what order, and which of the three patches is worth the first thirty seconds",
                Some("trance"),
                "doing",
            ),
            "low",
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
        let t = task(
            &format!("t-1{i:02}"),
            &format!("Panel work item number {i}"),
            Some("wsp"),
            "todo",
        );
        // One of the eight raised, so the list is not in id order and the row
        // that broke the order says why it did.
        tasks.push(if i == 5 { prio(t, "high") } else { t });
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
        let mut view = panel::View::default();
        let ui = panel::collect(snap, &view, Some("w0"));
        panel::place(&ui, &mut view, W, H);
        Driver { snap, view, ui, log: Vec::new() }
    }

    fn key(&mut self, k: Key) -> &mut Self {
        self.log.push(key_name(k));
        // The reducer may ask for a refetch; offline that just means rebuilding
        // the rows from the same snapshot, exactly as the live loop does.
        if let panel::Effect::Refetch = panel::apply_key(k, &mut self.ui, &mut self.view) {
            panel::refetch_into(&mut self.ui, self.snap, &mut self.view, Some("w0"));
        }
        // And the loop draws, which is where the view keeps its place. A
        // driver that skipped this would leave the tree deciding its offset
        // from the cursor alone, every key, for ever — the arrangement this
        // scrolling replaced.
        panel::place(&self.ui, &mut self.view, W, H);
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

    /// And for a named project, when the scene is about which one — the slug
    /// with a name behind it, the one this workspace is pinned to.
    fn to_project(&mut self, id: &str) -> &mut Self {
        let want = panel::Target::Project(id.to_string());
        loop {
            if self.ui.selected_target() == want {
                return self;
            }
            let before = self.ui.selected_index();
            self.key(Key::Down);
            if self.ui.selected_index() == before {
                panic!("no row for project {id}");
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

    fn scene(&mut self, title: &str, caption: &str) -> Scene {
        Scene {
            title: title.to_string(),
            caption: caption.to_string(),
            gesture: if self.log.is_empty() {
                "opens here".to_string()
            } else {
                compress(&self.log)
            },
            target: target_label(&self.ui.selected_target()),
            html: panel::to_html(&panel::frame(&self.ui, &mut self.view, W, H), W),
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
            .scene("Halfway down", "Fourteen rows of travel, and the tree has moved by four. The view has a position of its own and keeps it: the cursor crosses the pane, and only once it is within two rows of the foot does the tree give ground, one row at a time. Turning round costs nothing — the rows above the cursor are the ones already on screen."),
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
            .key(Key::Char('w'))
            .to_pane("w2:p1")
            .key(Key::Char('W'))
            .scene(
                "From the agent back to its work",
                "`w` answers who has stopped; the next question is always what they stopped *on*. `W` is that question: the tree comes back with the cursor already on the task the agent is holding, rather than at the top with the title to find by eye. It works on an agent row wherever one is drawn — this list, the section at the foot, the line under a claimed task — and it uncovers the task if the tree was holding it out of sight: the projects above it unfold, the cap comes off the list it is in, and a filter that would leave the task out goes off, in that order, stopping at the first that is enough. Each of those is a decision you made, so only the ones in the way are undone. An agent holding nothing is told so rather than moved — that is a pane to give work to, which is `f` or `c`.",
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
            .scene("Moving with it up", "The tree carries on underneath. It has fewer rows to work with, so the cursor runs out of pane sooner and the tree starts moving sooner — but the map is never allowed to push the cursor off the bottom, and the two rows of lookahead beyond it survive whatever the pane is down to."),
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
            .to_project("wsp")
            .key(Key::Char('e'))
            .scene("Renaming a project", "The same key on a project changes its *name*, which is not the string the row is drawn with: the tree is slugs, because a slug is short, unique and the thing you type at a shell, and every project in a real store has a longer name behind it than a thirty-four column pane can hold. So the field opens holding `wsp control plane` on a row that says `wsp`. It is the only rename a project has — nothing here moves a slug, since every task, pin and mandate refers to a project by it."),
    );

    out.push(
        Driver::new(&w)
            .to_task("t-001")
            .keys(&[Key::Char('t'), Key::Char(' '), Key::Down, Key::Down, Key::Char(' ')])
            .scene("Tagging", "t docks a picker: what the task carries first, then the rest of the vocabulary. Nothing is written until ↵, so the marks say what ↵ will do — ✓ carried, + coming, − going, · not carried. `synth` is dim and named with `trance` because the project lends it: it is on the task, `wsp show` prints it, and `wsp tag` cannot reach it. `dsp` is both the task's own and the project's, so removing it takes off the copy the task owns and the row says `trance` will put it straight back."),
    );

    out.push(
        Driver::new(&w)
            .to_task("t-004")
            .keys(&[Key::Char('t'), Key::Char('m'), Key::Char('i'), Key::Char('x')])
            .scene("Naming a tag that does not exist yet", "Typing narrows the list and doubles as the field for a tag nobody has used — one line for both, because they are one gesture. The last row offers to make what you typed, lowercased, so `DSP` and `dsp` never become two tags that read as one; ␣ takes it, and so does ↵, which then applies. ␣ is free to mean toggle because a tag with a space in it is not a tag."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .key(Key::Char('m'))
            .scene("Moving a task", "m turns the tree itself into the picker. Navigation and folding still work, so you hunt for the destination the way you would read for it — then ↵ takes whatever the cursor is on. A project row means the top level of that project; a task row means inside that task, as a sub-task of it. One question, asked at two scales, which is why they share a key."),
    );

    out.push(
        Driver::new(&w)
            .to_project("verb")
            .key(Key::Char('m'))
            .scene("Moving a project", "The same key one level up: a project moves under another project, sub-tree and all. Only a project answers — there is no row that means the top of the tree, so detaching one stays `wsp project set <id> parent=none` at a shell. Landing on itself, or on anything already beneath it, is refused by the CLI rather than by the panel, which has no index to ask; and it is refused rather than confirmed, because a project inside itself is a branch that vanishes from every list with its files still on disk."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .keys(&[Key::Char('m'), Key::Down, Key::Down])
            .scene("Landing somewhere valid", "Still picking. A project is a destination, and so is a task — it becomes a sub-task of the one you land on, and the project follows the parent rather than being asked about separately. So is the inbox, which unfiles it. A project row also detaches, because the top level of that project is what the row means: it is the way back out of a sub-tree, and without it a key that could only push work down is one you learn not to press. Anything else and ↵ says so rather than doing something surprising."),
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


/// Prose going into the page, as prose.
///
/// The frames are HTML this file built and go in raw; the captions beside them
/// are English and must not. A caption naming a command — `wsp project set
/// <id> parent=none` — was silently swallowed from `<id>` to the next `>`,
/// leaving a sentence about a flag that appeared to take no argument. Nothing
/// errors, and there is no rendering that says the words are missing.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

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
            "<div class=\"lgroup\"><div class=\"lhead\"><h3>{}</h3><p>{}</p></div><dl>",
            esc(group),
            esc(blurb),
        ));
        for m in marks {
            out.push_str(&format!(
                "<div class=\"mark\"><dt><span class=\"chip\">{}</span></dt>\
                 <dd><b>{}</b><span>{}</span></dd></div>",
                panel::to_html_spans(&m.sample),
                esc(m.name),
                esc(m.note),
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
            esc(&s.title),
            esc(&s.gesture),
            esc(&s.caption),
            esc(&s.target),
            s.html,
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

    /// The keyboard is in this pane, which is the state every click below is
    /// about unless it says otherwise. On a pane nobody is working in a click
    /// means one thing and only that — see
    /// [`a_click_on_a_pane_nobody_is_working_in_only_goes_there`].
    const WORKING_HERE: bool = true;

    /// Drive the panel the way a person would and hand back what it is
    /// showing: the reducer, then the rebuild the live loop does for it.
    fn showing(snap: &Snapshot, keys: &[Key]) -> (panel::Ui, panel::View) {
        let mut view = panel::View::default();
        let mut ui = ui_of(snap, &view);
        panel::place(&ui, &mut view, W, H);
        for k in keys {
            if let panel::Effect::Refetch = panel::apply_key(*k, &mut ui, &mut view) {
                panel::refetch_into(&mut ui, snap, &mut view, Some("w0"));
            }
            // The draw between one key and the next, which is where the view
            // keeps its place. Skipping it leaves a panel whose tree derives
            // its offset from the cursor every frame — the thing this replaced.
            panel::place(&ui, &mut view, W, H);
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

    /// Which rows the tree is made of, so a test can ask whether it is a tree
    /// at all — the agents view is agents and their own lines, and nothing else.
    fn kinds(ui: &mut panel::Ui) -> Vec<panel::RowKind> {
        let at = ui.selected_index();
        let mut out = Vec::new();
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            out.push(ui.selected_kind());
        }
        ui.select_for_test(at);
        out
    }

    /// `w` answers who has stopped. The next question is always what they
    /// stopped *on*, and `W` is that question: the tree comes back with the
    /// cursor already standing on the work, rather than at the top with the
    /// title to find by eye.
    #[test]
    fn shift_w_puts_the_cursor_on_the_work_the_agent_is_holding() {
        let w = world();
        let mut d = Driver::new(&w);
        d.key(Key::Char('w')).to_pane("w2:p1").key(Key::Char('W'));

        assert_eq!(d.ui.selected_target(), panel::Target::Task("t-003".into()));
        // And it is the tree that came back, not the list with a cursor moved
        // inside it: `W` is the way out of the agents view.
        assert!(kinds(&mut d.ui).contains(&panel::RowKind::Project));

        // An agent holding nothing has nowhere to be shown, and the tree is
        // the wrong place to go looking anyway. The view stays where it is.
        let mut d = Driver::new(&w);
        d.key(Key::Char('w')).to_pane("w4:p2").key(Key::Char('W'));
        assert_eq!(d.ui.selected_target(), panel::Target::Pane("w4:p2".into()));
        assert!(!kinds(&mut d.ui).contains(&panel::RowKind::Project));
    }

    /// And it shows the work wherever the tree had put it away. Three things
    /// hide a task — a folded project above it, the cap on a long list, a
    /// filter that leaves it out — and a jump that worked on none of them would
    /// be a key you could not trust to have done anything at all.
    #[test]
    fn shift_w_uncovers_the_task_whatever_the_tree_was_hiding_it_behind() {
        // Folded away, two projects above it.
        let w = world();
        let mut d = Driver::new(&w);
        d.to_project("audio").key(Key::Char('h'));
        assert!(!drawn_tasks(&mut d).contains(&"t-001".to_string()), "the fold already shows it");
        d.key(Key::Char('w')).to_pane("w1:p1").key(Key::Char('W'));
        assert_eq!(d.ui.selected_target(), panel::Target::Task("t-001".into()));

        // Past the cap on its project's list. A claimed task sorts to the top
        // of its own project, so the case that reaches the cap is a claimed
        // *sub*-task: the cap counts top-level work, and a child goes wherever
        // its parent went — here, into the `2 more` row at the foot of `wsp`.
        let mut w = world();
        let mut sub = task("t-109", "Panel work item 8, first half", Some("wsp"), "todo");
        sub.parent = Some("t-108".into());
        w.tasks.push(sub);
        w.bindings.insert("w5:p2".to_string(), json!({ "task_id": "t-109" }));
        let mut d = Driver::new(&w);
        assert!(!drawn_tasks(&mut d).contains(&"t-109".to_string()), "the cap already shows it");
        d.key(Key::Char('w')).to_pane("w5:p2").key(Key::Char('W'));
        assert_eq!(d.ui.selected_target(), panel::Target::Task("t-109".into()));

        // Finished, and the tree is not showing finished work. The filter goes
        // off, which the footer says in the same breath — `+done` is there to
        // be read the moment the tree changes shape.
        let mut w = world();
        w.bindings.insert("w5:p2".to_string(), json!({ "task_id": "t-030" }));
        let mut d = Driver::new(&w);
        d.key(Key::Char('w')).to_pane("w5:p2").key(Key::Char('W'));
        assert_eq!(d.ui.selected_target(), panel::Target::Task("t-030".into()));
        assert!(panel::frame(&d.ui, &mut d.view, W, H).iter().any(|l| l.text().contains("+done")));
    }

    /// Every task the tree is currently drawing, by id.
    fn drawn_tasks(d: &mut Driver) -> Vec<String> {
        let at = d.ui.selected_index();
        let mut out = Vec::new();
        for i in 0..d.ui.rows_for_test() {
            d.ui.select_for_test(i);
            if let panel::Target::Task(id) = d.ui.selected_target() {
                out.push(id);
            }
        }
        d.ui.select_for_test(at);
        out
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
        let marks = |ui: &panel::Ui, view: &mut panel::View| -> String {
            panel::frame(ui, view, W, H)[0]
                .text()
                .chars()
                .filter(|c| "←■●○·".contains(*c))
                .collect()
        };
        // One mark per agent, ordered as the list is: wants you, blocked,
        // spare, working, quiet.
        let want = "←■○○●●·";

        let (ui, mut view) = showing(&w, &[]);
        assert_eq!(marks(&ui, &mut view), want, "at rest");
        let (ui, mut view) = showing(&w, &[Key::Char('R')]);
        assert_eq!(marks(&ui, &mut view), want, "under the review filter");
        let (ui, mut view) = showing(&w, &[Key::Char('w')]);
        assert_eq!(marks(&ui, &mut view), want, "in the agents view");

        // Nobody running: the strip has nothing to say and says that instead of
        // drawing a bare zero.
        let (ui, mut view) = showing(&quiet_world(), &[]);
        assert!(panel::frame(&ui, &mut view, W, H)[0].text().contains("no agents"));
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
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 2, detail, WORKING_HERE), panel::Hit::Select);
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
            let hit = panel::click(&mut ui, &mut view, W, H, 4 + i, 0, WORKING_HERE);
            match hit {
                panel::Hit::Focus(a) => assert_eq!(a.pane(), agent.pane(), "mark {i}"),
                other => panic!("mark {i} should go to a terminal, got {other:?}"),
            }
        }
        // The name is not a mark, and neither is the gap before the total.
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 0, 0, WORKING_HERE), panel::Hit::Nothing);
        assert_eq!(panel::click(&mut ui, &mut view, W, H, W - 1, 0, WORKING_HERE), panel::Hit::Nothing);
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
        let clipped = panel::frame(&ui, &mut view, narrow, H)[0].text();
        assert!(clipped.contains(panel::glyph::MORE), "{clipped}");
        let at = clipped.chars().position(|c| c.to_string() == panel::glyph::MORE).unwrap();
        assert_eq!(panel::click(&mut ui, &mut view, narrow, H, at, 0, WORKING_HERE), panel::Hit::Rest);
    }

    /// Ed: "clicks should only perform their action if the pane was already
    /// focused, to avoid bouncing when clicking on an agent when the pane is
    /// unfocused but the agent is selected".
    ///
    /// The mouse reaches a pane the keyboard is not in — that is what makes the
    /// panel worth pointing at — and the panel answers a click by taking the
    /// keyboard. Acting as well is the bounce: point at the agent the cursor is
    /// already on, which is the one you have been watching and is why the cursor
    /// is there, and the click means `↵`. So focus arrives here and leaves again
    /// for that agent's terminal in the same gesture, and you are somewhere you
    /// did not decide to be, by way of a pane you were only looking at.
    ///
    /// Every shape of click is the same rule, and it has to be: which of them a
    /// pixel is depends on what the cursor is on and what is drawn under the
    /// pointer, so a panel that swallowed only the ones that jump would be one
    /// you had to read before you could predict your own first click.
    #[test]
    fn a_click_on_a_pane_nobody_is_working_in_only_goes_there() {
        let w = world();
        let nobody = false;

        // The agents view, cursor on the first of them: the case in the report.
        let (mut ui, mut view) = showing(&w, &[Key::Char('w')]);
        let on = ui.selected_index();
        let y = (0..H)
            .find(|&y| panel::row_at(&ui, &view, W, H, y) == Some(on))
            .expect("the selected row is on the pane");

        // With the keyboard here it is `↵` on that agent, which goes to its
        // terminal. That is the half of the gesture worth avoiding twice over.
        let (mut spare, mut spare_view) = (ui.clone(), view.clone());
        assert_eq!(
            panel::click(&mut spare, &mut spare_view, W, H, 0, y, WORKING_HERE),
            panel::Hit::Activate,
        );

        assert_eq!(panel::click(&mut ui, &mut view, W, H, 0, y, nobody), panel::Hit::Keyboard);
        assert_eq!(ui.selected_index(), on, "and the cursor stayed where it was");
        // And the click after it, which now has the keyboard, means what it says.
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 0, y, WORKING_HERE), panel::Hit::Activate);

        // A mark in the strip is one click to another terminal, so it is the
        // same jump by a shorter route; a row the cursor is not on would only
        // select, and does not do even that.
        let (mut ui, mut view) = showing(&w, &[]);
        let sel = ui.selected_index();
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 4, 0, nobody), panel::Hit::Keyboard);
        let other = (0..H)
            .find(|&y| matches!(panel::row_at(&ui, &view, W, H, y), Some(i) if i != sel))
            .expect("a row the cursor is not on");
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 0, other, nobody), panel::Hit::Keyboard);
        assert_eq!(ui.selected_index(), sel, "nothing under the pointer was read at all");
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
        let foot = |ui: &panel::Ui, view: &mut panel::View| panel::frame(ui, view, W, H)[H - 2].text();

        let (ui, mut view) = showing(&w, &[Key::Char('R')]);
        assert!(foot(&ui, &mut view).contains("review only"));

        let (ui, mut view) = showing(&w, &[Key::Char('R'), Key::Char('w')]);
        let f = foot(&ui, &mut view);
        assert!(f.contains("agents") && !f.contains("review only"), "{f}");

        let (ui, mut view) = showing(&w, &[Key::Char('w'), Key::Char('R')]);
        let f = foot(&ui, &mut view);
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

    /// The tree scrolls once the cursor has crossed it, and a click has to go
    /// through the same offset the frame drew with — otherwise clicking works
    /// at the top of a list and acts on the wrong task further down.
    #[test]
    fn a_click_follows_the_scroll() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        for _ in 0..20 {
            panel::apply_key(Key::Down, &mut ui, &mut view);
            panel::place(&ui, &mut view, W, H);
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
    fn assert_mapping_matches_frame(ui: &panel::Ui, view: &mut panel::View, at: &str) {
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
                        let drawn = panel::frame(&ui, &mut view, W, h);
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
    /// Selecting recentred the tree, so the row you clicked slid out from under
    /// the pointer — and the second click of select-then-activate landed on
    /// whatever moved into its place.
    ///
    /// The view keeps its own position now, so a click in the middle of the
    /// pane cannot move it at all. What this still guards is the edge: a
    /// keystroke landing two rows from the foot is owed rows beyond it and
    /// scrolls the tree to get them, and the same row reached by pointer is not.
    #[test]
    fn clicking_a_row_leaves_it_where_it_was() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        // Scroll into the middle of a list long enough to move.
        for _ in 0..14 {
            panel::apply_key(Key::Down, &mut ui, &mut view);
            panel::place(&ui, &mut view, W, H);
        }
        // Every clickable row on the pane, not a handful near the top: the row
        // that would move is the one at the edge, and which screen row that is
        // depends on how tall the dock happens to be today.
        for y in 0..H {
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
            let at = |ui: &mut panel::Ui, view: &mut panel::View| {
                panel::click(ui, view, W, H, 0, y, WORKING_HERE)
            };
            assert_eq!(at(&mut after_ui, &mut after_view), panel::Hit::Select);
            assert_eq!(
                panel::row_at(&after_ui, &after_view, W, H, y),
                Some(i),
                "clicking screen row {y} moved tree row {i} out from under the pointer"
            );
            // And the row it selected is the row that was clicked, so the
            // second click activates rather than selecting something else.
            assert_eq!(at(&mut after_ui, &mut after_view), panel::Hit::Activate);
        }
    }

    /// The view has a position and keeps it: the cursor crosses a still pane,
    /// and only pushes the tree when it runs out of pane to cross.
    ///
    /// The tree used to be drawn from the cursor alone, held in the middle, so
    /// every row of travel scrolled everything — you could not read two rows
    /// without the one you had just read moving. The cost was clearest on
    /// turning round: `k` after a run of `j` re-scrolled at once, when what you
    /// were going back to had been on the pane the whole time.
    #[test]
    fn the_cursor_crosses_the_pane_before_the_pane_moves() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        panel::place(&ui, &mut view, W, H);
        let top = |ui: &panel::Ui, view: &panel::View| panel::row_at(ui, view, W, H, 2);
        let mut down = |ui: &mut panel::Ui, view: &mut panel::View| {
            panel::apply_key(Key::Down, ui, view);
            panel::place(ui, view, W, H);
        };

        // Opening at the top, the first rows of travel move nothing: the
        // cursor has a pane in front of it to walk through.
        let start = top(&ui, &view);
        assert_eq!(start, Some(0), "the tree opens at its first row");
        for n in 0..4 {
            down(&mut ui, &mut view);
            assert_eq!(top(&ui, &view), start, "the tree moved on press {}", n + 1);
        }

        // Far enough down and the cursor reaches the foot, where there is
        // nothing left to cross and the tree has to give.
        for _ in 0..14 {
            down(&mut ui, &mut view);
        }
        let moved = top(&ui, &view);
        assert!(moved > start, "the tree should scroll once the cursor reaches the foot");

        // And turning round is free. The rows above the cursor are the ones
        // already on the pane, so going back to them is cursor travel and
        // nothing else.
        for n in 0..4 {
            panel::apply_key(Key::Up, &mut ui, &mut view);
            panel::place(&ui, &mut view, W, H);
            assert_eq!(top(&ui, &view), moved, "the tree moved going back up, press {}", n + 1);
        }
    }

    /// Ed: "overscrolling has dead travel — scrolling back up does nothing
    /// until the overshoot is walked off". This is why, and it goes with the
    /// view having a position of its own.
    ///
    /// The wheel used to be three of what `j` does, because there was no view
    /// position to move: the cursor was the scroll. At the end of the tree the
    /// clamp stops the view while the cursor goes on to the foot, so every
    /// notch past the end bought half a pane of cursor travel that a wheel back
    /// up had to spend before anything on screen moved.
    #[test]
    fn a_wheel_back_up_moves_the_view_at_once() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        panel::place(&ui, &mut view, W, H);
        let top = |ui: &panel::Ui, view: &panel::View| panel::row_at(ui, view, W, H, 2);

        // Down to the end, and well past it: the wheel against the last screen
        // is a burst of events that move nothing.
        for _ in 0..12 {
            panel::wheel(&mut ui, &mut view, W, H, false);
        }
        let bottom = top(&ui, &view);
        panel::wheel(&mut ui, &mut view, W, H, false);
        assert_eq!(top(&ui, &view), bottom, "the last screen is the last screen");

        // And the very next notch upward moves the view — no travel to undo.
        panel::wheel(&mut ui, &mut view, W, H, true);
        assert!(top(&ui, &view) < bottom, "a wheel up should move the view straight away");
    }

    /// Scrolling away from the selection leaves it selected. The wheel moves
    /// the view and nothing else — what is selected is something you decided,
    /// and a scroll to go and look at something else must not quietly change
    /// the row the next verb acts on.
    ///
    /// It used to drag the cursor to the near edge to keep it on the pane,
    /// which is the same silent substitution wearing a helpful face.
    #[test]
    fn scrolling_past_the_selection_leaves_it_selected() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        panel::place(&ui, &mut view, W, H);
        let visible = |ui: &panel::Ui, view: &panel::View| {
            (0..H).any(|y| panel::row_at(ui, view, W, H, y) == Some(ui.selected_index()))
        };

        // Somewhere with tree above and below it, so the wheel can leave it in
        // either direction.
        for _ in 0..6 {
            panel::apply_key(Key::Down, &mut ui, &mut view);
            panel::place(&ui, &mut view, W, H);
        }
        let chosen = ui.selected_index();
        let target = ui.selected_target();

        for n in 0..12 {
            panel::wheel(&mut ui, &mut view, W, H, false);
            assert_eq!(ui.selected_index(), chosen, "the wheel moved the cursor, notch {}", n + 1);
            assert_eq!(ui.selected_target(), target);
        }
        assert!(!visible(&ui, &view), "twelve notches should have left the selection behind");

        // And the next press of a cursor key brings the view back to it: the
        // keyboard is where acting happens, so the tree returns to where the
        // acting will be.
        panel::apply_key(Key::Down, &mut ui, &mut view);
        panel::place(&ui, &mut view, W, H);
        assert!(visible(&ui, &view), "a cursor key should bring the view back to the cursor");
        assert_eq!(ui.selected_index(), chosen + 1, "and it continues from where the cursor was");
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

        let drawn = panel::frame(&d.ui, &mut d.view, W, H);
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
            let drawn = panel::frame(&d.ui, &mut d.view, W, H);
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
            let drawn = panel::frame(&d.ui, &mut d.view, W, H);
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
        let mut view = panel::View::default();
        let ui = ui_of(&w, &view);
        assert_mapping_matches_frame(&ui, &mut view, "at rest");
    }

    #[test]
    fn the_mapping_matches_the_frame_after_scrolling() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        for n in 0..24 {
            panel::apply_key(Key::Down, &mut ui, &mut view);
            assert_mapping_matches_frame(&ui, &mut view, &format!("after {} down", n + 1));
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
                let said = t.text.clone().expect("a sentence, not just a clear");
                assert!(said.contains("wsp next -p trance"), "said: {said}");
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
                let said = t.text.clone().expect("a sentence, not just a clear");
                assert!(said.contains("wsp next -p verb"), "said: {said}");
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
                let said = t.text.clone().expect("a sentence, not just a clear");
                assert!(said.contains("t-020"), "said: {said}");
            }
            _ => panic!("the pick should run a claim"),
        }
    }

    /// And it arrives on an empty context. The pane a claim lands in is nearly
    /// always an agent that has just finished something else, and a work order
    /// read through the last task's reasoning is the thing this stops.
    ///
    /// Only for the kinds whose clear we know how to spell. herdr starts twenty
    /// of them and `/clear` is Claude Code's word: anywhere else the sentence
    /// goes in on its own, rather than behind a line the agent would read as
    /// the first half of its instructions.
    #[test]
    fn the_work_order_goes_in_behind_a_clear() {
        let clear_before_the_claim = |w: &Snapshot| -> Option<&'static str> {
            let mut view = panel::View::default();
            let mut ui = ui_of(w, &view);
            for i in 0..500 {
                ui.select_for_test(i);
                if ui.selected_target() == panel::Target::Task("t-020".into()) {
                    break;
                }
            }
            press(&mut ui, &mut view, 'c');
            on_pane(&mut ui, "w4:p2");
            match panel::apply_key(Key::Enter, &mut ui, &mut view) {
                panel::Effect::Run { then, .. } => then.expect("an idle agent is told").clear,
                _ => panic!("the pick should run a claim"),
            }
        };

        assert_eq!(clear_before_the_claim(&world()), Some("/clear"));

        let mut other = world();
        for p in other.panes.iter_mut().filter(|p| p.pane_id == "w4:p2") {
            p.agent = "codex".into();
        }
        assert_eq!(clear_before_the_claim(&other), None);
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
                let said = t.text.clone().expect("a sentence, not just a clear");
                assert!(said.contains("wsp next -p verb"), "said: {said}");
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

    // ---- handing it to whoever is spare ----------------------------------

    /// `C` is `c` with the hunt taken out, and it has to end in the same place:
    /// the same claim, the same override behind it, and the same sentence typed
    /// into the pane it chose. A second hand-over that differed from the first
    /// in any of the three would be a second way of doing one thing.
    #[test]
    fn handing_a_task_over_claims_it_onto_a_spare_agent_and_tells_it() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        // A task in `trance`, and w4:p2 is the spare agent standing there.
        on_task(&mut ui, "t-001");
        match press(&mut ui, &mut view, 'C') {
            panel::Effect::Run { argv, escalate, then } => {
                assert_eq!(argv, vec!["claim", "t-001", "--pane", "w4:p2"]);
                assert_eq!(
                    escalate.expect("a refused claim is worth asking about"),
                    vec!["claim", "t-001", "--pane", "w4:p2", "--force"]
                );
                let t = then.expect("an agent handed a task is told about it");
                assert_eq!(t.pane, "w4:p2");
                assert_eq!(t.clear, Some("/clear"), "on an empty context, like every other");
                let said = t.text.clone().expect("a sentence, not just a clear");
                assert!(said.contains("t-001"), "said: {said}");
            }
            _ => panic!("C should claim onto the spare agent"),
        }
    }

    /// Who it picks is not "the first one going": the spare agent already
    /// standing in the work's own project is the one that can start on it,
    /// and the other is in a different checkout entirely.
    ///
    /// w6:p1 sorts first of the two — the census is ordered by name and it has
    /// none, so it answers to its workspace — which is what makes this a test
    /// of the preference rather than of the order.
    #[test]
    fn the_spare_agent_in_the_right_project_is_the_one_it_picks() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        let pane_for = |ui: &mut panel::Ui, view: &mut panel::View, task: &str| -> String {
            on_task(ui, task);
            match press(ui, view, 'C') {
                panel::Effect::Run { argv, .. } => argv[3].clone(),
                _ => panic!("C should claim onto a spare agent"),
            }
        };

        // In `trance`, where one of the two spare agents is standing.
        assert_eq!(pane_for(&mut ui, &mut view, "t-001"), "w4:p2");
        // Filed nowhere, so nothing is a better answer than anything else and
        // the first spare agent takes it.
        assert_eq!(pane_for(&mut ui, &mut view, "t-020"), "w6:p1");
        // In `wsp`, where nobody is standing: still handed over, because "the
        // agent is in another tree" is a worse answer than "nobody has it".
        assert_eq!(pane_for(&mut ui, &mut view, "t-101"), "w6:p1");
    }

    /// Who is free is a fact about the machine, not about what is drawn. The
    /// review filter takes the agents off the screen entirely, and a `C` that
    /// read the rows would answer "nobody is spare" over a header strip still
    /// showing two of them.
    #[test]
    fn a_hand_over_finds_an_agent_the_view_is_not_showing() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        if let panel::Effect::Refetch = press(&mut ui, &mut view, 'R') {
            panel::refetch_into(&mut ui, &w, &mut view, Some("w0"));
        }
        assert!(
            !has(&ui, &panel::Target::Pane("w6:p1".into())),
            "the review filter is the case: no agent row is on screen at all",
        );
        on_task(&mut ui, "t-004");
        match press(&mut ui, &mut view, 'C') {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["claim", "t-004", "--pane", "w6:p1"]);
            }
            _ => panic!("C should claim onto the spare agent the filter hid"),
        }
    }

    /// The half the key is named for. Nobody spare is the ordinary answer on a
    /// busy afternoon, and the panel says so and does nothing — a claim onto a
    /// working agent is what `c` is for, deliberately, and interrupting one is
    /// not something a single keystroke should be able to do by accident.
    #[test]
    fn a_hand_over_with_nobody_to_hand_to_does_nothing() {
        let mut w = world();
        for p in w.panes.iter_mut().filter(|p| !p.agent.is_empty()) {
            p.agent_status = "working".into();
        }
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_task(&mut ui, "t-001");
        assert!(matches!(press(&mut ui, &mut view, 'C'), panel::Effect::None));

        // And with nobody running at all, which is the same answer for a
        // different reason — see the footer line, which says which.
        let quiet = quiet_world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&quiet, &view);
        on_task(&mut ui, "t-001");
        assert!(matches!(press(&mut ui, &mut view, 'C'), panel::Effect::None));
    }

    /// Only a task is handed over. An agent row is the other end of the same
    /// join and already has two keys — `c` to point it at work and `f` to send
    /// it looking — so a third meaning here would be guesswork about which of
    /// them was meant.
    #[test]
    fn only_a_task_is_handed_to_somebody() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_kind(&mut ui, panel::RowKind::Project);
        assert!(matches!(press(&mut ui, &mut view, 'C'), panel::Effect::None));
        on_pane(&mut ui, "w4:p2");
        assert!(matches!(press(&mut ui, &mut view, 'C'), panel::Effect::None));
    }

    // ---- taking the work back --------------------------------------------

    /// `u` is `c` run backwards, and it undoes both halves. The release is what
    /// takes the binding, the durable claim and the name the claim wrote; the
    /// clear behind it is what stops an agent that now holds nothing going on
    /// reasoning about the task it used to.
    ///
    /// Nothing typed after the clear. Every other `Tell` here exists to carry a
    /// sentence and empties the context so the sentence lands cleanly; this one
    /// is the empty context, and a line explaining that would be the only thing
    /// left in the window it just cleared.
    #[test]
    fn taking_work_off_an_agent_releases_it_and_empties_its_window() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        // w2:p1 is idle on t-003 — the "needs you" case, and the one you most
        // want this key for.
        on_pane(&mut ui, "w2:p1");
        match press(&mut ui, &mut view, 'u') {
            panel::Effect::Run { argv, escalate, then } => {
                assert_eq!(argv, vec!["release", "--pane", "w2:p1"]);
                assert!(escalate.is_none(), "release has nothing to refuse on");
                let t = then.expect("an idle agent handed its work back is cleared");
                assert_eq!(t.pane, "w2:p1");
                assert_eq!(t.clear, Some("/clear"));
                assert!(t.text.is_none(), "nothing is typed after the clear");
            }
            _ => panic!("u should run a release"),
        }
    }

    /// The same gesture from the other end of the join. "Take this agent off
    /// its task" and "get whoever is on this task off it" are one fact asked
    /// two ways, and the row under the cursor is the only difference — exactly
    /// as `c` takes a task or a pane and ends in the same claim.
    #[test]
    fn a_task_row_answers_the_same_key_with_the_agent_holding_it() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_task(&mut ui, "t-003");
        match press(&mut ui, &mut view, 'u') {
            panel::Effect::Run { argv, then, .. } => {
                assert_eq!(argv, vec!["release", "--pane", "w2:p1"]);
                assert!(then.is_some(), "the agent it names is idle, so it is cleared");
            }
            _ => panic!("u on a claimed task should release the agent holding it"),
        }
    }

    /// A busy agent is released and not cleared, the mirror of what a claim
    /// does to one: the store is safe to change at any moment and a pane in the
    /// middle of a turn is not. Taking work off an agent that will not stop is
    /// most of the reason for the key, so the release itself cannot wait.
    #[test]
    fn a_working_agent_hands_the_task_back_without_being_cleared() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w1:p1"); // working, holding t-001
        match press(&mut ui, &mut view, 'u') {
            panel::Effect::Run { argv, then, .. } => {
                assert_eq!(argv, vec!["release", "--pane", "w1:p1"]);
                assert!(then.is_none(), "a working agent is not typed into");
            }
            _ => panic!("u should run a release"),
        }
    }

    /// Only the kinds whose clear we know how to spell. herdr starts twenty of
    /// them and `/clear` is Claude Code's word — and unlike a work order, which
    /// still goes in on its own where the clear is unknown, there is nothing
    /// left to send here. So the release lands alone and no thread is started
    /// to type nothing into a pane.
    #[test]
    fn an_agent_whose_clear_we_cannot_spell_is_only_released() {
        let mut w = world();
        for p in w.panes.iter_mut().filter(|p| p.pane_id == "w2:p1") {
            p.agent = "codex".into();
        }
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w2:p1");
        match press(&mut ui, &mut view, 'u') {
            panel::Effect::Run { argv, then, .. } => {
                assert_eq!(argv, vec!["release", "--pane", "w2:p1"]);
                assert!(then.is_none(), "nothing to clear and nothing to say");
            }
            _ => panic!("u should run a release"),
        }
    }

    /// Nothing to take back. An agent holding no work and a task nobody is on
    /// are the same non-event, and running a release against either would be a
    /// commit, an event and a log line about a change that did not happen.
    #[test]
    fn there_is_nothing_to_take_back_from_an_agent_holding_nothing() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_pane(&mut ui, "w4:p2"); // idle, spare, bound to nothing
        assert!(matches!(press(&mut ui, &mut view, 'u'), panel::Effect::None));

        on_task(&mut ui, "t-020"); // in the inbox, nobody on it
        assert!(matches!(press(&mut ui, &mut view, 'u'), panel::Effect::None));

        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Project("verb".into()) {
                break;
            }
        }
        assert!(matches!(press(&mut ui, &mut view, 'u'), panel::Effect::None));
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

    /// One key for three values, and the order it steps through is the whole
    /// of the design: `high` first, because raising something is what a person
    /// reaching for `!` nearly always means, and `normal` last so holding the
    /// key returns you to where you started rather than stranding you.
    #[test]
    fn one_key_steps_a_task_through_its_priorities() {
        use crate::model::Priority;

        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_task(&mut ui, "t-101");
        match panel::apply_key(Key::Char('!'), &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => assert_eq!(argv, vec!["prio", "t-101", "high"]),
            _ => panic!("! on an ordinary task should raise it"),
        }

        // And on the one the fixture already raised it goes down, not nowhere:
        // a key that only ever means `high` cannot put a task back.
        on_task(&mut ui, "t-105");
        match panel::apply_key(Key::Char('!'), &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => assert_eq!(argv, vec!["prio", "t-105", "low"]),
            _ => panic!("! on a raised task should lower it"),
        }
        assert_eq!(Priority::High.cycled().cycled(), Priority::Normal, "three presses is a round trip");

        // Nowhere else. A project has no priority, and running `prio` against
        // its id would be a command about a task that does not exist.
        on_kind(&mut ui, panel::RowKind::Project);
        assert!(matches!(
            panel::apply_key(Key::Char('!'), &mut ui, &mut view),
            panel::Effect::None
        ));
    }

    /// What the key is for. Priority orders a project's own tasks and does
    /// nothing across projects — the tree is what keeps that local, since two
    /// tasks under different projects are never in one list to sort — and the
    /// row that broke id order carries the mark that says why it did.
    #[test]
    fn priority_orders_a_project_and_marks_the_rows_it_moved() {
        let w = world();
        let view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        let mut order: Vec<(String, String)> = Vec::new();
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if let panel::Target::Task(id) = ui.selected_target() {
                order.push((id, panel::render_row_for_test(&ui, i, W).text()));
            }
        }
        let at = |id: &str| {
            order.iter().position(|(x, _)| x == id).unwrap_or_else(|| panic!("no row for {id}"))
        };
        let row = |id: &str| order[at(id)].1.clone();

        // t-105 is raised and t-101..t-104 are not, so it leads a list that is
        // otherwise in id order.
        assert!(at("t-105") < at("t-101"), "a raised task leads its project");
        assert!(at("t-101") < at("t-102"), "and the rest keep their order");
        assert!(row("t-105").contains(panel::glyph::HIGH), "unexplained: {}", row("t-105"));
        assert!(!row("t-101").contains(panel::glyph::HIGH), "normal is drawn as nothing");

        // Lowered, t-002 sits under its project-mate — and `verb`, a different
        // project entirely, is untouched by either.
        assert!(at("t-001") < at("t-002"), "a lowered task sinks");
        assert!(row("t-002").contains(panel::glyph::LOW), "unexplained: {}", row("t-002"));

        // The mark comes out of the title's budget, not off the end of the
        // row: t-002 is the longest title in the fixture by a distance.
        for (id, text) in &order {
            assert!(text.chars().count() <= W, "{id} overran the pane: {text}");
        }
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
        let shown = panel::frame(&ui, &mut view, W, H)[H - 1].text();
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
        let shown = panel::frame(&ui, &mut view, W, H)[H - 1].text();
        assert!(!shown.contains(&tail), "ctrl-u should clear the line: {shown}");
    }

    /// Put the cursor on a project row by id, for the same reason `on_task`
    /// exists: counting presses rots the moment the fixture gains a row.
    fn on_project(ui: &mut panel::Ui, id: &str) {
        let want = panel::Target::Project(id.to_string());
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_target() == want {
                return;
            }
        }
        panic!("no row for project {id}");
    }

    /// What the picker is offering, in the order it draws it.
    fn tag_dock(view: &panel::View) -> Vec<String> {
        panel::tag_rows_for_test(view, W).iter().map(|l| l.trim_end().to_string()).collect()
    }

    /// Tagging is a picker, not a prompt, and the reason is the removal. A
    /// line taking `+dsp -ui` can add and remove in one go and still makes you
    /// *spell* every tag — including the one you are taking off, which is a
    /// name the panel is already holding and you are being asked to remember.
    /// The vocabulary is nineteen words across the real store, so it fits.
    #[test]
    fn tagging_picks_from_the_tags_the_store_already_uses() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_task(&mut ui, "t-001");
        panel::apply_key(Key::Char('t'), &mut ui, &mut view);

        // What the task carries first — its own, then what its project lends
        // it — and the vocabulary under that, commonest first. Projects count
        // twice over: `synth` is on no task at all and has to be offerable,
        // and it reaches this one from `trance` whether anybody offers it or
        // not.
        let dock = tag_dock(&view);
        let names: Vec<String> =
            dock.iter().map(|l| l.split_whitespace().nth(1).unwrap_or("").to_string()).collect();
        assert_eq!(names, ["dsp", "release", "synth", "herdr", "rust"], "{dock:?}");
        assert!(dock[0].trim_start().starts_with(panel::glyph::TAG_ON), "carried: {}", dock[0]);
        assert!(dock[3].trim_start().starts_with(panel::glyph::QUIET), "not carried: {}", dock[3]);
        // The one the project lends is drawn as *on*, because it is: `wsp
        // show` prints it and so does the detail pane. Showing only the task's
        // own tags made `synth` read as absent on a task that every other
        // surface says is tagged `synth`, which is why taking one off looked
        // broken rather than impossible.
        assert!(dock[2].trim_start().starts_with(panel::glyph::TAG_ON), "lent: {}", dock[2]);
        assert!(dock[2].contains("trance"), "and says where from: {}", dock[2]);

        // Nothing is written until `↵`, so the frame has to say what `↵` will
        // do. `␣` on a carried tag marks it for removal where it stands — the
        // rows do not reorder, because a list that shuffles under the cursor
        // is how you take off the tag next to the one you meant.
        panel::apply_key(Key::Char(' '), &mut ui, &mut view);
        let dock = tag_dock(&view);
        assert!(dock[0].trim_start().starts_with(panel::glyph::TAG_OFF), "going: {}", dock[0]);
        assert_eq!(
            dock.iter().map(|l| l.split_whitespace().nth(1).unwrap_or("")).collect::<Vec<_>>(),
            ["dsp", "release", "synth", "herdr", "rust"],
            "the order is fixed when it opens",
        );

        // `␣` on a lent tag does nothing at all and says why. `wsp tag <id>
        // -synth` would remove nothing here and report success, so the picker
        // refuses rather than passing that on.
        panel::apply_key(Key::Down, &mut ui, &mut view);
        panel::apply_key(Key::Down, &mut ui, &mut view);
        panel::apply_key(Key::Char(' '), &mut ui, &mut view);
        let dock = tag_dock(&view);
        assert!(dock[2].trim_start().starts_with(panel::glyph::TAG_ON), "still lent: {}", dock[2]);

        // And on one it does not carry, marks it for adding.
        panel::apply_key(Key::Down, &mut ui, &mut view);
        panel::apply_key(Key::Char(' '), &mut ui, &mut view);
        let dock = tag_dock(&view);
        assert!(dock[3].trim_start().starts_with(panel::glyph::TAG_NEW), "coming: {}", dock[3]);

        // One command for the lot, in the order the list drew them — and the
        // lent one is not in it, because there was never anything to send.
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["tag", "t-001", "--", "-dsp", "+herdr"]);
            }
            _ => panic!("↵ should apply the changes"),
        }
    }

    /// A tag can be the task's *and* the project's, which in the real store is
    /// the common case — a task under `wsp` carrying its own `rust`. Taking
    /// off the copy the task owns leaves the tag on the task, because the
    /// project puts it back, and a picker that did not say so would look like
    /// the removal had failed. Which is the complaint this came from.
    #[test]
    fn removing_a_tag_the_project_also_lends_says_it_will_still_be_there() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        // t-001 owns `dsp`, and `trance` lends it too.
        on_task(&mut ui, "t-001");
        panel::apply_key(Key::Char('t'), &mut ui, &mut view);
        assert!(tag_dock(&view)[0].contains("trance"), "{:?}", tag_dock(&view));

        panel::apply_key(Key::Char(' '), &mut ui, &mut view);
        let row = tag_dock(&view)[0].clone();
        assert!(row.trim_start().starts_with(panel::glyph::TAG_OFF), "going: {row}");
        assert!(row.contains("trance"), "…and coming straight back: {row}");

        // The command is still honest about what it does: it takes off the
        // copy the task owns, which is the only copy `wsp tag` can reach.
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => assert_eq!(argv, vec!["tag", "t-001", "--", "-dsp"]),
            _ => panic!("↵ should still remove the task's own copy"),
        }
    }

    /// The picker takes its rows off the tree, and the row it is about is one
    /// of the ones that can go. A wheel is entitled to carry the view clean
    /// off the selection and leave it selected — that is the whole point of
    /// the view having a position of its own — so `t` pressed after one would
    /// otherwise open a list of tags over a tree with the task nowhere in it.
    #[test]
    fn the_picker_brings_the_task_it_is_about_back_into_view() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        let shows = |ui: &panel::Ui, view: &mut panel::View| {
            panel::frame(ui, view, W, H).iter().any(|l| l.text().contains("Apply reverb fixes"))
        };

        on_task(&mut ui, "t-001");
        panel::place(&ui, &mut view, W, H);
        assert!(shows(&ui, &mut view), "the fixture should open with it on screen");

        // Scroll away from it. The selection stays where it is and the view
        // does not follow, which is what the wheel is for.
        for _ in 0..8 {
            panel::wheel(&mut ui, &mut view, W, H, false);
        }
        panel::place(&ui, &mut view, W, H);
        assert!(!shows(&ui, &mut view), "the wheel should have left it behind");
        assert_eq!(ui.selected_target(), panel::Target::Task("t-001".into()), "still selected");

        panel::apply_key(Key::Char('t'), &mut ui, &mut view);
        panel::place(&ui, &mut view, W, H);
        assert!(shows(&ui, &mut view), "opening the picker owes the task a look");
        // …and the tree is still a tree underneath it.
        assert!(!tag_dock(&view).is_empty());
    }

    /// The whole bargain of a mode that writes nothing until `↵`: a fumble
    /// costs nothing. `prio` had to grow the same guard for the same reason —
    /// a key you can press by accident must not spend a log line, an event and
    /// a commit recording that it was pressed.
    #[test]
    fn a_tag_toggled_back_is_not_a_change_and_esc_is_not_one_either() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_task(&mut ui, "t-001");
        panel::apply_key(Key::Char('t'), &mut ui, &mut view);
        panel::apply_key(Key::Char(' '), &mut ui, &mut view);
        panel::apply_key(Key::Char(' '), &mut ui, &mut view);
        assert!(
            matches!(panel::apply_key(Key::Enter, &mut ui, &mut view), panel::Effect::None),
            "on and off again is where it started",
        );

        // …and walking away from real changes discards them.
        panel::apply_key(Key::Char('t'), &mut ui, &mut view);
        panel::apply_key(Key::Char(' '), &mut ui, &mut view);
        assert!(matches!(
            panel::apply_key(Key::Esc, &mut ui, &mut view),
            panel::Effect::None
        ));
        // Nothing stuck: opening again offers the tags the task still has.
        panel::apply_key(Key::Char('t'), &mut ui, &mut view);
        let dock = tag_dock(&view);
        assert!(dock[0].trim_start().starts_with(panel::glyph::TAG_ON), "unchanged: {}", dock[0]);
    }

    /// Typing is still how a tag nobody has used yet gets its name — the
    /// filter and the new-tag field are one line, because they are one
    /// gesture: you type what you want and take whichever of the two you are
    /// offered.
    #[test]
    fn typing_narrows_the_list_and_names_a_tag_that_is_not_in_it() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_task(&mut ui, "t-001");
        panel::apply_key(Key::Char('t'), &mut ui, &mut view);
        for c in "rus".chars() {
            panel::apply_key(Key::Char(c), &mut ui, &mut view);
        }
        let dock = tag_dock(&view);
        assert_eq!(dock.len(), 2, "one match, and the offer to make it anyway: {dock:?}");
        assert!(dock[0].contains("rust"));
        assert!(dock[1].contains("rus") && dock[1].contains("new"), "{}", dock[1]);

        // `↵` on the row that would make one makes it and applies: typing a
        // name and pressing return is a single gesture and reading it as two
        // would be the picker being clever at you.
        panel::apply_key(Key::Down, &mut ui, &mut view);
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => assert_eq!(argv, vec!["tag", "t-001", "--", "+rus"]),
            _ => panic!("↵ on the new row should make the tag"),
        }

        // A filter that matches something exactly offers no duplicate of it.
        panel::apply_key(Key::Char('t'), &mut ui, &mut view);
        for c in "rust".chars() {
            panel::apply_key(Key::Char(c), &mut ui, &mut view);
        }
        assert_eq!(tag_dock(&view).len(), 1, "rust is already a tag");

        // Case-folded on the way in, or `DSP` and `dsp` become two tags that
        // read as one everywhere they are listed.
        panel::apply_key(Key::KillLine, &mut ui, &mut view);
        for c in "DSP".chars() {
            panel::apply_key(Key::Char(c), &mut ui, &mut view);
        }
        let dock = tag_dock(&view);
        assert_eq!(dock.len(), 1, "DSP finds dsp rather than offering a second one: {dock:?}");

        // Nowhere but a task. `wsp tag` is a task verb, and a project's tags
        // are a whole list set at once by `project set`.
        panel::apply_key(Key::Esc, &mut ui, &mut view);
        on_kind(&mut ui, panel::RowKind::Project);
        assert!(matches!(
            panel::apply_key(Key::Char('t'), &mut ui, &mut view),
            panel::Effect::None
        ));
    }

    /// A project's *name*, which is not the string the row is drawn with. The
    /// tree is slugs — short, unique, what `-p` takes — and every project in
    /// the real store has a longer name behind it. `e` changes that one,
    /// because it is the only rename a project has: nothing in wsp moves a
    /// slug, since every task, pin and mandate refers to it by that.
    #[test]
    fn a_project_is_renamed_by_the_name_it_is_not_drawn_with() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_project(&mut ui, "wsp");
        let row = panel::render_row_for_test(&ui, ui.selected_index(), W).text();
        assert!(row.contains("wsp"), "the row draws the slug: {row}");
        assert!(!row.contains("control plane"), "and never the name: {row}");

        // The prompt opens holding the name, the same as a task's title does,
        // and for the same reason: a rename is a correction.
        panel::apply_key(Key::Char('e'), &mut ui, &mut view);
        let shown = panel::frame(&ui, &mut view, W, H)[H - 1].text();
        assert!(shown.contains("wsp control plane"), "should open holding the name: {shown}");

        // Sent back untouched it is not a rename, so it does not spend a
        // commit saying so.
        assert!(matches!(
            panel::apply_key(Key::Enter, &mut ui, &mut view),
            panel::Effect::None
        ));

        // Changed, it is one key=value pair with the spaces intact: `project
        // set` splits on the first `=` and takes the rest whole.
        on_project(&mut ui, "wsp");
        panel::apply_key(Key::Char('e'), &mut ui, &mut view);
        panel::apply_key(Key::Char('!'), &mut ui, &mut view);
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["project", "set", "wsp", "name=wsp control plane!"]);
            }
            _ => panic!("a changed name should run a set"),
        }
    }

    /// Put the cursor on the inbox heading.
    fn on_inbox(ui: &mut panel::Ui) {
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Inbox {
                return;
            }
        }
        panic!("no inbox row");
    }

    /// `m` on a task is one question — where does this belong — and the tree
    /// answers it at whichever scale the cursor lands on. A project row means
    /// the top level of that project; a task row means inside that task.
    ///
    /// The pair has to be a pair. A key that could only push work down into a
    /// sub-tree would be one you learn not to press, because the way back out
    /// would be a shell and an id you had to go and read — so a project row
    /// carries the detach, and landing on one is how a sub-task stops being
    /// one.
    #[test]
    fn m_files_a_task_under_another_and_takes_it_back_out() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        // Down a level, across projects: `t-004` is in `verb` and `t-005` is in
        // `tooling`. No `-p` in the argv — the project follows the parent, and
        // naming the one the cursor was in would be the disagreement `mv`
        // refuses.
        on_task(&mut ui, "t-004");
        assert!(matches!(
            panel::apply_key(Key::Char('m'), &mut ui, &mut view),
            panel::Effect::None
        ));
        on_task(&mut ui, "t-005");
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, escalate, .. } => {
                assert_eq!(argv, vec!["mv", "t-004", "--parent", "t-005"]);
                // Nothing to override: a cycle is not a policy, it is a branch
                // that stops being reachable.
                assert_eq!(escalate, None);
            }
            _ => panic!("landing on a task should make it a sub-task"),
        }

        // …and back up. `t-052` is a sub-task of `t-005`; a project row means
        // the top of that project, so the detach travels with the move.
        on_task(&mut ui, "t-052");
        panel::apply_key(Key::Char('m'), &mut ui, &mut view);
        on_project(&mut ui, "verb");
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["mv", "t-052", "-p", "verb", "--parent", "none"]);
            }
            _ => panic!("landing on a project should file it at the top of one"),
        }

        // The inbox is a top level like any other, and unfiling a sub-task has
        // to detach it for the same reason.
        on_task(&mut ui, "t-052");
        panel::apply_key(Key::Char('m'), &mut ui, &mut view);
        on_inbox(&mut ui);
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["mv", "t-052", "-p", "inbox", "--parent", "none"]);
            }
            _ => panic!("the inbox should unfile it"),
        }
    }

    /// One key for both things that move. `m` on a task asks which project it
    /// belongs to; on a project it asks which project it belongs *under*, and
    /// the pick is the same tree either way.
    #[test]
    fn m_moves_a_project_the_way_it_moves_a_task() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_project(&mut ui, "verb");
        assert!(matches!(
            panel::apply_key(Key::Char('m'), &mut ui, &mut view),
            panel::Effect::None
        ));

        // The tree is the picker, so the destination is wherever the cursor
        // walks to.
        on_project(&mut ui, "tooling");
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, escalate, .. } => {
                assert_eq!(argv, vec!["project", "set", "verb", "parent=tooling"]);
                // No stronger form. A project inside itself is not a policy to
                // override — the branch simply stops being reachable from the
                // root and vanishes from every list with its files still there.
                assert_eq!(escalate, None);
            }
            _ => panic!("landing on a project should run a set"),
        }

        // Only a project answers. A task, the inbox and a pane are all rows
        // the cursor reaches during the hunt and none of them is a parent.
        on_project(&mut ui, "verb");
        panel::apply_key(Key::Char('m'), &mut ui, &mut view);
        on_task(&mut ui, "t-001");
        assert!(matches!(
            panel::apply_key(Key::Enter, &mut ui, &mut view),
            panel::Effect::None
        ));
    }

}
