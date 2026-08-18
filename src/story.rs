//! `wsp panel storyboard` — the panel driven and drawn without a terminal.
//!
//! Four steps, and the shape is Ed's: **state, modification, state', render.**
//! A fixture is a state built in memory; an input is a modification; the reducer
//! is the only thing that produces the next state; and the frame is what that
//! state looks like. Nothing here talks to herdr or the store, which is the
//! point — the frames come out the same on a laptop with nothing running.
//!
//! Two kinds of scene, mirroring the pair that works well on the Daisy side:
//! *fixtures* pin a hand-built state and catch layout regressions, and *flows*
//! push scripted input through the real reducer so the frame is a consequence
//! of input rather than an assertion about it.
//!
//! # The alphabet of modifications
//!
//! Keys, clicks, the wheel, and the keyboard arriving or leaving — everything a
//! panel can be handed. All of it goes through [`panel::apply_input`], which is
//! the door the live event loop uses; see [`Driver::input`]. It was keys alone
//! for a long time, and the three that were missing were exactly the three that
//! could only be verified by a person at a trackpad. Two of the panel's live
//! failures were mouse failures, and neither had a test that could have caught
//! one.
//!
//! A script names the row it means — [`Driver::to_task`], [`Driver::click_on`]
//! — rather than counting presses or coordinates, so a fixture that gains a
//! task does not silently re-aim every scene written against it.
//!
//! # Why a filmstrip is not enough on its own
//!
//! Sixty-odd captioned frames catch a change in the drawing, and that is worth
//! having. What they cannot do is state intent: a frame fails with a diff
//! rather than a reason, and it has no way to say *after this click the cursor
//! is on that task and the effect is `inspect`*. Half of a transition is not
//! drawn at all — an effect is what the panel asked the loop to do, and `c` on
//! a task and `c` on an agent draw the same next frame while one of them runs
//! `wsp claim`.
//!
//! So a script writes [`Claim`]s beside the inputs that satisfy them, and they
//! are checked as the scenes are built. One pass yields both halves: what
//! became true, and what it looked like. The page draws the claims under the
//! frame they belong to, and `every_claim_the_storyboard_makes_holds` fails the
//! build when one does not.

use std::collections::BTreeMap;

use serde_json::json;

use crate::live::AgentRef;
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

/// The workspaces the fixture's panes stand in.
///
/// A list of places, and not a field on the snapshot: a pane row carries the
/// name of the place it stands in, so the join herdr's two lists used to need
/// has already happened by the time a view sees one. What the workspaces
/// themselves said — a label, and which was focused — is either on the row or
/// is not a view fact at all. See `crate::live`.
fn workspace(id: &str) -> String {
    match id {
        "w0" => "orchestrator",
        "w1" => "Trance Video",
        "w2" => "Verb UI",
        "w3" => "Easter",
        "w4" => "Trance Lite",
        "w5" => "panel work",
        // A label that matches no project, for a pane that stands nowhere.
        "w6" => "scratch",
        _ => "",
    }
    .to_string()
}

fn agent(pane: &str, ws: &str, state: &str, title: &str) -> AgentRef {
    AgentRef {
        pane: pane.to_string(),
        workspace: ws.to_string(),
        workspace_label: workspace(ws),
        agent: true,
        kind: "claude".into(),
        state: state.to_string(),
        title: title.to_string(),
        ..Default::default()
    }
}

/// A pane with nobody driving it. The panel could not see these at all before,
/// because a shell is not an agent.
fn shell(pane: &str, ws: &str, cwd: &str) -> AgentRef {
    AgentRef {
        pane: pane.to_string(),
        workspace: ws.to_string(),
        workspace_label: workspace(ws),
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
        // Beside it, the other kind of stopped: nobody owes this one anything.
        // In the fixture because the pair is only legible together — one row
        // red and loud, the next dim and at the foot of its project.
        task("t-007", "Port the tank to the new SIMD path", Some("verb"), "parked"),
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
        AgentRef {
            label: panel::PANEL_LABEL.into(),
            pane: "w0:p2".into(),
            workspace: "w0".into(),
            workspace_label: workspace("w0"),
            ..Default::default()
        },
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
        // Nothing raised. A flag is an interruption and the world is what the
        // panel looks like when nobody is interrupting — [`flagged_world`] is
        // the one that shows the section, so every other frame goes on saying
        // what the panel says on an ordinary afternoon.
        flags: BTreeMap::new(),
        // Nobody coordinating, which is the ordinary state and the one the
        // storyboard is for. A seat changes one judgement on one row — see
        // [`crate::cmd_govern`] — and a world that had one would be showing
        // the exception in every frame.
        governors: BTreeMap::new(),
        panes: agents,
    }
}

/// The same world with a governor in `wsp`'s slot, and an empty one on `verb`.
///
/// Both halves on purpose. A filled slot is what the position looks like when
/// it is working, and an empty one is the case every other surface in wsp
/// cannot show at all — the post outliving whoever was in it, which is the
/// thing that has to be visible for anybody to fill it.
///
/// The custodian is `w2:p1`, which in [`world`] is an idle agent holding a
/// `doing` task: the exact row that drew `← needs you` all night while it was
/// the busiest agent on the machine. Here it draws as a seat, under `wsp`,
/// rather than as a stalled worker under `verb`.
fn seated_world() -> Snapshot {
    let mut s = world();
    // herdr's own name for the pane, which `wsp govern` writes: a governor's
    // pane is named after the position, because the position is the work in it.
    // It carries no `▣` — every surface of wsp's draws that in its own first
    // column, and a label with one came out as `▣ ▣ governor · wsp` in the
    // census at the foot.
    if let Some(p) = s.panes.iter_mut().find(|p| p.pane == "w2:p1") {
        p.label = crate::cmd_govern::governor_of("wsp");
    }
    s.governors.insert(
        "wsp".to_string(),
        json!({ "workspace": "w2", "pane": "w2:p1", "host": crate::util::hostname() }),
    );
    // Vacated rather than absent: `wsp govern --clear` and a reaped workspace
    // both leave this, and it is what the tree has to keep drawing.
    s.governors.insert("verb".to_string(), json!({ "vacated": "2026-08-17T04:00:00Z" }));
    s
}

/// The same world with nothing running — what the panel looks like before any
/// agent exists, which is most people's first sight of it.
fn quiet_world() -> Snapshot {
    let mut s = world();
    s.panes.clear();
    s.bindings.clear();
    s
}

/// Five agents, one of them alone in its project and the rest together in
/// another — the arrangement Ed's short pane cut in the wrong place.
///
/// The shape is the point: the foot draws a heading, one agent, a second
/// heading, and the rest under it, so the row a short pane stops after is a
/// heading rather than an agent. A fixture whose agents are all in one project
/// has no such row and cannot show the bug at any height.
#[cfg(test)]
fn crowded_world() -> Snapshot {
    let mut s = world();
    // Nobody holding anything: what is under test is where the agents are, and
    // a claim would put half of them in the tree as well.
    s.bindings.clear();
    s.panes = vec![{
        let mut a = agent("w1:p1", "w1", "idle", "spare hands");
        a.cwd = "~/claude/trance".into();
        a
    }];
    // Spare sorts before working, so the one on its own is first in the census
    // and its heading is first in the foot.
    for i in 1..=4 {
        let mut a = agent(&format!("w2:p{i}"), "w2", "working", "on the panel");
        a.cwd = "~/claude/wsp".into();
        s.panes.push(a);
    }
    s
}

/// Two agents asking to be looked at.
///
/// One is the case the feature was asked for — an agent that has found work it
/// thinks it should take and cannot decide that for itself. The other is the
/// weaker and commoner one: a task that exists and wants eyes, with nothing
/// written on it beyond that, which the row has to draw as the title rather
/// than as an empty line.
fn flagged_world() -> Snapshot {
    let mut s = world();
    s.flags.insert(
        "t-105".to_string(),
        json!({
            "said": "this is next — can I take it?",
            "title": "the panel work, in order",
            "body": "Items 1-4 are done and this one is the first that is not. \
It needs the reducer, which nobody else is in, so it will not collide with \
anything running.\n\nI have context on it already — I wrote the four above it.",
            "ask": "claim",
            // A spare agent, and idle — which is what makes the sentence
            // reach it when the ask is answered. herdr calls the pane in w5
            // neither working nor idle, and a `y` there would land the claim
            // and go untold, the same way `c` does on a pane that has not
            // spoken since it started.
            "pane": "w4:p2",
            "at": "2026-08-16T09:41:00Z",
        }),
    );
    // Read, and still up: the card has been put away and the row is what is
    // left. Also the older of the two, so the fixture shows the card that comes
    // up *next* rather than the one somebody has already dealt with.
    s.flags.insert(
        "t-004".to_string(),
        json!({ "said": "", "pane": "w2:p1", "at": "2026-08-16T09:12:00Z", "seen": true }),
    );
    s
}

/// The same asks, every one of them read.
///
/// A card is up for as long as it takes to answer and the section is up until
/// somebody deals with the work, so this is what the panel looks like for the
/// vastly longer half of a flag's life — and it is the fixture anything about
/// the *section* has to use, because a card holds the keyboard and a frame with
/// one up is a frame where no other key did anything.
fn read_world() -> Snapshot {
    let mut s = flagged_world();
    for f in s.flags.values_mut() {
        if let Some(m) = f.as_object_mut() {
            m.insert("seen".into(), json!(true));
        }
    }
    s
}

// ---- scenes -------------------------------------------------------------

struct Scene {
    title: String,
    caption: String,
    /// The inputs that produced this frame, as the reader would make them.
    gesture: String,
    /// What a subcommand aimed at the cursor would act on.
    target: String,
    /// What the script said the inputs would make true, and whether it did.
    claims: Vec<Claim>,
    html: String,
}

/// What a script says a modification made true.
///
/// A filmstrip catches a change in the drawing and cannot state intent: it
/// fails with a diff rather than a reason, and it has no way to say "after this
/// key the cursor is on the next task and the effect is a refetch". Ed's
/// framing is that the assertion belongs on the **transition**, with the render
/// as the fourth step rather than the only one.
///
/// So a claim is written beside the input that is meant to satisfy it, checked
/// as the scene is built, and drawn under the frame. One script yields both
/// halves: what became true, and what it looked like. A claim that does not
/// hold fails a test by name — and says so on the page, which is where anyone
/// reading the filmstrip is already looking.
struct Claim {
    /// The sentence a reader sees under the frame.
    said: String,
    /// What actually happened, when that is not what was said.
    broke: Option<String>,
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
        panel::Target::Seat(p) => format!("the {p} seat"),
        panel::Target::Overflow(k) => format!("hidden tail of {k}"),
        panel::Target::Nothing => "nothing".into(),
    }
}

/// What the reducer asked the loop for, as one phrase.
///
/// The effect is the half of a transition no frame can show. `c` on a task and
/// `c` on an idle agent draw the same next frame and mean different things —
/// one of them ran `wsp claim` and typed a work order into a terminal — so a
/// scene that only compares pictures cannot tell them apart. Named rather than
/// compared as a value: `Run` carries argv, and a claim reading `run wsp claim
/// t-004 --pane w1:p1` is the sentence a reader wants, where a `Vec<String>`
/// in a diff is not.
fn effect_label(e: &panel::Effect) -> String {
    match e {
        panel::Effect::None => "nothing".into(),
        panel::Effect::Refetch => "a refetch".into(),
        panel::Effect::Focus(a) => format!("focus {}", a.pane),
        panel::Effect::Sync => "a sync".into(),
        panel::Effect::Quit => "quit".into(),
        panel::Effect::Spawn { argv, .. } => format!("spawn: wsp {}", argv.join(" ")),
        panel::Effect::Inspect(_) => "inspect".into(),
        panel::Effect::CloseView => "close the view".into(),
        panel::Effect::PopOut { label, .. } => format!("pop out {label}"),
        panel::Effect::Board { label, .. } => format!("open {label}"),
        panel::Effect::Full => "open the tab".into(),
        panel::Effect::Run { argv, .. } => format!("run wsp {}", argv.join(" ")),
        panel::Effect::Tell(_) => "tell an agent".into(),
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
        // Not the coordinates. Where a click landed is what the claim under
        // the frame says, in the store's own words; `click 2,14` on a gesture
        // chip is a number pair the reader has to hold the fixture to decode.
        Key::Click { .. } => "click".into(),
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
    /// The pane this one is being driven in. A field rather than the constants,
    /// because a fullscreen panel is the same panel in a bigger pane and there
    /// is no other difference to show.
    size: (usize, usize),
    /// Whether this pane is the one being worked in.
    ///
    /// Not part of the view — `crate::draw`'s "focus is not an input" holds
    /// here, and nothing below draws it. It is on the driver because it is the
    /// one piece of the panel's state a *click* reads and writes, so a script
    /// that could not set it could only ever drive the focused case, which is
    /// the half that was never the bug.
    keyboard: bool,
    /// What the last input asked the loop for. The half of a transition the
    /// frame cannot show; see [`effect_label`].
    effect: panel::Effect,
    /// What the script has said about the inputs since the last scene.
    claims: Vec<Claim>,
}

impl<'a> Driver<'a> {
    fn new(snap: &'a Snapshot) -> Driver<'a> {
        Driver::at(snap, W, H)
    }

    /// The panel `Z` opens: a tab of its own, at the width of the workspace.
    fn at(snap: &'a Snapshot, w: usize, h: usize) -> Driver<'a> {
        let mut view = panel::View::default();
        // Exactly what the live loop does before it builds its rows: which rows
        // there are depends on whether this is a page or a sidebar, and what
        // `q` means depends on whether this panel is the tab or the furniture.
        view.fit_to_pane(w);
        view.takes_the_tab(true);
        let ui = panel::collect(snap, &view);
        panel::place(&ui, &mut view, w, h);
        Driver {
            snap,
            view,
            ui,
            log: Vec::new(),
            size: (w, h),
            // A panel opens as the pane being worked in. Every script that says
            // nothing about focus is describing that panel, which is the one
            // almost every reader has in front of them.
            keyboard: true,
            effect: panel::Effect::None,
            claims: Vec::new(),
        }
    }

    /// One input, through the door the live loop uses.
    ///
    /// [`panel::apply_input`] and not `apply_key`: a click has to be turned
    /// into a row before it means anything, and that translation used to live
    /// in the event loop, where no fixture could reach it. A script driving
    /// `apply_key` and doing the translation itself would be testing its own
    /// copy of the loop, which is the one thing a harness must never be.
    fn input(&mut self, input: panel::Input) -> &mut Self {
        self.log.push(match &input {
            panel::Input::Key(k) => key_name(*k),
            panel::Input::Focus(true) => "\u{2328} here".into(),
            panel::Input::Focus(false) => "\u{2328} away".into(),
        });
        let (w, h) = self.size;
        self.effect =
            panel::apply_input(input, &mut self.ui, &mut self.view, w, h, &mut self.keyboard);
        // The reducer may ask for a refetch; offline that just means rebuilding
        // the rows from the same snapshot, exactly as the live loop does.
        if let panel::Effect::Refetch = self.effect {
            panel::refetch_into(&mut self.ui, self.snap, &mut self.view);
        }
        // And the loop draws, which is where the view keeps its place. A
        // driver that skipped this would leave the tree deciding its offset
        // from the cursor alone, every key, for ever — the arrangement this
        // scrolling replaced.
        panel::place(&self.ui, &mut self.view, self.size.0, self.size.1);
        self
    }

    fn key(&mut self, k: Key) -> &mut Self {
        self.input(panel::Input::Key(k))
    }

    /// Point at the line a target is drawn on.
    ///
    /// By target and not by coordinate, for the reason [`Driver::to_task`]
    /// exists rather than a count of `j` presses: `click 2, 14` describes the
    /// fixture it was written against, and would go on passing while it
    /// silently pointed at a different row, the day a project above it gained a
    /// task. The line is asked of [`panel::row_at`] — the arithmetic that drew
    /// the frame — so a scripted click lands where a real one would.
    fn click_on(&mut self, want: panel::Target) -> &mut Self {
        let (w, h) = self.size;
        let rows = self.ui.rows_for_target(&want);
        let row = *rows.first().unwrap_or_else(|| panic!("no row for {}", target_label(&want)));
        let y = (0..h)
            .find(|&y| panel::row_at(&self.ui, &self.view, w, h, y) == Some(row))
            .unwrap_or_else(|| panic!("{} is not on the pane", target_label(&want)));
        self.click_at(2, y)
    }

    /// Point at whatever the cursor is already on — the second half of select,
    /// then activate.
    fn click_again(&mut self) -> &mut Self {
        let want = self.ui.selected_target();
        self.click_on(want)
    }

    /// Point at a mark in the header strip. Not a row: see
    /// [`panel::strip_column`].
    fn click_mark(&mut self, pane: &str) -> &mut Self {
        let x = panel::strip_column(&self.ui, self.size.0, pane)
            .unwrap_or_else(|| panic!("the strip is not drawing {pane}"));
        self.click_at(x, 0)
    }

    /// And at the ellipsis that stands for the agents it could not draw. Only a
    /// test points at that one — see [`panel::strip_rest_column`].
    #[cfg(test)]
    fn click_rest(&mut self) -> &mut Self {
        let x = panel::strip_rest_column(&self.ui, self.size.0)
            .unwrap_or_else(|| panic!("the strip is not clipped at {} cells", self.size.0));
        self.click_at(x, 0)
    }

    /// A raw coordinate. The primitive the three above resolve to, and the one
    /// to reach for when the point of the scene *is* the coordinate — a click
    /// on furniture, on the blank tail, on a line nothing is drawn on.
    fn click_at(&mut self, x: usize, y: usize) -> &mut Self {
        self.key(Key::Click { x, y })
    }

    fn wheel(&mut self, up: bool) -> &mut Self {
        self.key(Key::Wheel { up })
    }

    /// The keyboard arrived in this pane, or left it.
    fn focus(&mut self, here: bool) -> &mut Self {
        self.input(panel::Input::Focus(here))
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

    /// And for a project's seat, which is a row of its own and not a pane: a
    /// scene about the position has to name which position.
    fn to_seat(&mut self, project: &str) -> &mut Self {
        let want = panel::Target::Seat(project.to_string());
        loop {
            if self.ui.selected_target() == want {
                return self;
            }
            let before = self.ui.selected_index();
            self.key(Key::Down);
            if self.ui.selected_index() == before {
                panic!("no seat row for {project}");
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

    /// An agent with nothing in its hands, which is not in the tree.
    ///
    /// It used to be, and these scenes were written when it was. An unclaimed
    /// agent stopped being a tree row when the census was pinned at the foot —
    /// the same pane twice in one glance is one too many to count — and this
    /// fixture holds more agents than the section draws, so the spare one sits
    /// behind its `⋯`. `G` to the last row, `→` to open the tail where it
    /// stands, and `⇱` back to the top to hunt down from.
    ///
    /// Three presses that say out loud where a spare agent now lives. The
    /// storyboard had been panicking on this row since the day it moved, and
    /// nothing ran it: see `every_scene_the_storyboard_ships_can_be_drawn`.
    fn to_spare(&mut self, pane: &str) -> &mut Self {
        self.keys(&[Key::Char('G'), Key::Right, Key::Home]).to_pane(pane)
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

    // ---- what the script says the inputs made true ----------------------
    //
    // Each of these is one sentence about the state the inputs arrived at, and
    // each is checked here rather than left for a diff to notice. Written
    // beside the input that is meant to satisfy it, so a script reads as
    // modification, claim, modification, claim — and the frame at the end is
    // the fourth step rather than the only one. See [`Claim`].

    /// Record a sentence, and what actually happened if it is not true.
    fn claim(&mut self, said: String, ok: bool, but: String) -> &mut Self {
        self.claims.push(Claim { said, broke: (!ok).then_some(but) });
        self
    }

    /// The cursor is on this, in the store's own words — which is to say: this
    /// is what the next verb acts on.
    fn now_on(&mut self, want: &str) -> &mut Self {
        let at = target_label(&self.ui.selected_target());
        self.claim(format!("the cursor is on {want}"), at == want, format!("it is on {at}"))
    }

    /// And this is what the panel asked the loop to do about it.
    fn did(&mut self, want: &str) -> &mut Self {
        let got = effect_label(&self.effect);
        self.claim(format!("the effect is {want}"), got == want, format!("it is {got}"))
    }

    /// What the panel is now in the middle of — the half of the state a frame
    /// reports worst. See [`panel::View::mode_name`].
    fn now_in(&mut self, want: &str) -> &mut Self {
        let got = self.view.mode_name();
        self.claim(
            format!("the keyboard is in {want}"),
            got == want,
            format!("it is in {got}"),
        )
    }

    /// The words the panel put in front of the reader. Read off the frame and
    /// not off the state, because a footer nobody can see is not a message.
    fn says(&mut self, want: &str) -> &mut Self {
        let (w, h) = self.size;
        let frame = panel::frame(&self.ui, &mut self.view, w, h);
        let said = frame.iter().any(|l| l.text().contains(want));
        self.claim(format!("the panel says \u{201c}{want}\u{201d}"), said, "it does not".into())
    }

    /// Whether this is the pane being worked in. The one fact a click both
    /// reads and writes, and the one that made the two live mouse failures
    /// invisible to every test that existed.
    fn has_the_keyboard(&mut self, want: bool) -> &mut Self {
        let got = self.keyboard;
        self.claim(
            match want {
                true => "the keyboard is in this pane".into(),
                false => "the keyboard is somewhere else".into(),
            },
            got == want,
            match got {
                true => "it is here".into(),
                false => "it is not".into(),
            },
        )
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
            claims: std::mem::take(&mut self.claims),
            html: panel::to_html(
                &panel::frame(&self.ui, &mut self.view, self.size.0, self.size.1),
                self.size.0,
            ),
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
        "Three agents. One working a task (●), one idle on a task that is still doing — which raises the ← asking for you, on the agent's own row in the tree as well as in the strip — and one unclaimed at the foot. An agent is drawn the same way wherever it appears: the tree used to give every stopped pane the same grey ○ and leave the difference to the section at the foot. The cursor opens on the inbox, because unfiled work is what you triage before reading anything that already has a home.",
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

    // The same fixture, in the pane `Z` leaves behind. Every scene above and
    // below this one is the same panel at thirty-four columns, which is the
    // point: there is no second surface here, only a bigger pane and a frame
    // drawn to it.
    out.push(
        Driver::at(&w, 120, 30).scene(
            "The whole tree",
            "Z opens the panel in a tab of its own, at the width of the workspace. It is a second panel and it costs nothing to be one: the folds, the filters and the cursor live in the store, so this and the sidebar are the same panel at two widths — it opens on the row you pressed Z from. Nothing is laid out differently, either. The tree is the same one row to a line, and every one of those rows is now as wide as the pane, so a title that was twenty-five characters and an ellipsis is a sentence. Two things do change: the six-task cap comes off, because six was what a project could spend of a column that had to hold thirty projects; and the footer says how to leave, because this is a tab somebody opened rather than furniture that is always there.",
        ),
    );

    out.push(
        Driver::new(&w)
            .down_to_next(panel::RowKind::Section)
            .scene("The other group", "Shells that resolve nowhere sit at the foot, after the work — a pane with nobody in it is a fact about a place, so it stays in the tree, where places are. Nothing can be added here; herdr owns panes."),
    );

    out.push(
        Driver::new(&w)
            .to_pane("w2:p1")
            .scene("The agents, always on", "Under a rule of its own at the foot: five rows of agents, picked in the order the strip is drawn in — what wants you, what is free, what is busy — and then stood under the project each one is in, the same runs `w` draws at length. Which of them is a question about who has stopped; where they sit is a question about the tree, and neither answer is allowed to be the other's. Pinned, so the tree above scrolls and this does not, because who has stopped is the question you ask between reading anything else and it must not be a keystroke away. Five rows and not five agents: the headings are rows like any other, and a section that grew with the number of projects its agents were in was one a short pane could not draw. What they buy is the column the project used to be repeated down — a pane's name has half the width here that it has in the tree, and it is the same name. The section's heading counts every agent, so the ones the rows could not take are never silently absent, and 1-9 start here rather than in the tree: a digit you can always see is worth more than one in row order."),
    );

    out.push(
        Driver::new(&w)
            .keys(&[Key::Char('G'), Key::Right])
            .scene("Opening the rest in place", "G to the last row, which is the section's own `⋯`, and `→` opens the tail where it stands — the same gesture as a project past the six-task cap. For anyone who would rather not leave the tree at all; `w` is the other door, and it gives the same agents three lines each instead of one."),
    );

    out.push(
        Driver::new(&w)
            .to_spare("w4:p2")
            .key(Key::Char('c'))
            .scene("Handing work to an idle agent", "`○` is an agent that has stopped holding nothing — a person's worth of attention going spare, and the row the section exists to keep on screen: it sorts above the busy ones for exactly that reason, since there is nothing to do about an agent that is working. `c` on it turns the tree into the picker: choose what it takes. The section draws five and this world runs seven, so `G →` opens its tail first — an unclaimed agent is only in the census, never in the tree, and drawing the same pane twice in one glance is one time too many to count."),
    );

    out.push(
        Driver::new(&w)
            .to_spare("w4:p2")
            .key(Key::Char('c'))
            .up_to(panel::RowKind::More)
            .key(Key::Enter)
            .scene("Reaching past the cap", "Every project stops at six tasks and puts the rest behind `⋯`, so a hunt for work to hand over runs into one. `↵` inside a pick takes the row it lands on — and on a row it cannot take but can *open*, it opens it: the tail here, a folded project the same way. The pick is still running; the two tasks that were out of reach are now rows like any other."),
    );

    out.push(
        Driver::new(&w)
            .to_spare("w4:p2")
            .key(Key::Char('f'))
            .scene("Letting it choose for itself", "The other half of the same idea. `c` hands over a task you picked; `f` hands over a *project* and lets the agent pick inside it — and asks first, because everything that aims an agent does. Say yes and the panel types `wsp next` into the pane and leaves. The project comes from the same chain the agent's own `wsp where` would use, so the panel can never send a pane somewhere it would disagree it is. Shells are refused and a working agent is left alone: a sentence typed into the wrong pane is a command, and a refusal puts up no question at all."),
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

    // The custodial slot, filled and empty, one after the other: the position
    // is meant to be the same row either way, and two scenes side by side is
    // the only way to show that.
    let g = seated_world();

    out.push(
        Driver::new(&g)
            .to_seat("wsp")
            .scene(
                "A governor, in its place",
                "A project can have a governor, and the agent in it is answerable for everything beneath \u{2014} it sequences what runs next, writes the direction an arriving agent needs, reviews finished work against the code, and holds the record. It is not claimed onto a task and should not be, so the row names the *position* and the project rather than the work: a governor holding no task is the most assigned thing on the panel, and every earlier version of this row went looking for a task and drew `unassigned`. It is drawn under the project it answers for, directly beneath it and above the work, rather than under whatever task it had picked up to have somewhere to stand \u{2014} which was the whole complaint this came from, a governor of `wsp` appearing in the tree as an agent working a task three branches away. One agent holds one governorship; the pane on the right is where `\u{21b5}` goes. The mark is `\u{25a3}` wherever a governor is drawn, and the same agent in the census at the foot draws it too: it is idle between the agents it started, which is most of the night, and none of that idleness is you being the blocker.",
            ),
    );

    out.push(
        Driver::new(&g)
            .to_seat("verb")
            .scene(
                "The seat outlives whoever was in it",
                "`verb`'s governor is empty \u{2014} stood down, or its workspace closed \u{2014} and the row is still here. That is the point of a slot: the position belongs to the project and an agent fills it, so a night ending does not take the post down with the agent, and there is somewhere to stand when you want to fill it again. `wsp spawn -p verb --govern` opens a workspace, seats an agent in it and hands it the custodial work order rather than a claim. A vacancy draws only where no governor above it is filled: it is an invitation, and one per level of a deep tree would bury the governors that exist. On a slot that has somebody in it, `T` says something to them \u{2014} addressed to the position, so it reaches whoever is in it when the sentence lands rather than the pane the agent started in.",
            ),
    );

    let f = flagged_world();
    let r = read_world();

    out.push(Driver::new(&r).scene(
        "A hand raised",
        "`wsp flag <id> \"why\"` is how an agent asks to be looked at, and this is where it arrives: a section pinned above the agents, in every panel, without anybody pressing anything. The sentence is what the row draws, because the sentence is the news — the task's own title is up in the tree with a `\u{25b2}` on it, and the pane that raised it is on the right. A flag with nothing written on it draws the title instead, which is the whole of \"look at this, it exists\". The footer counts them for the pane too short to give the section any rows.",
    ));

    out.push(Driver::new(&f).scene(
        "The card, which nobody pressed a key for",
        "A raised hand comes up over the tree on its own — this is the one thing on the panel that arrives rather than being asked for, and the only one drawn with a border: everything else here is furniture that is always there, and a rule is enough to separate that. It is inset by a column so the rows behind it show at both edges, which is the difference between a card lying on the tree and a pane that has replaced it. The heading is the agent's own where it wrote one, because a task's title is often the wrong sentence for the moment. It holds the keyboard while it is up, so `y` cannot land on the tree behind it, and it never covers the section at the foot — answering one ask must not hide the queue of the others.",
    ));

    out.push(
        Driver::new(&f)
            .key(Key::Esc)
            .scene(
                "Not now",
                "`esc` is *not now*: the card goes and the hand stays raised, so the section still says somebody is waiting and `↵` on that row brings the card back. It is written into the flag rather than remembered per panel — there are twenty-two panels, and a card put away on this one would otherwise come up again on the next one you switched to. `x` is the other answer, and it means dealt with: the flag comes down everywhere.",
            ),
    );

    out.push(
        Driver::new(&r)
            .down_to(panel::RowKind::Flag)
            .scene(
                "The flag is the task",
                "The cursor is on a raised hand and the target is the task it points at — which is the deeplink the agent could not otherwise draw. Every verb already aimed at a task works from here: `\u{21b5}` opens it, `c` claims it, `s` starts it, `E` writes it up. `x` lowers the flag, and only `x` does: reading the ask must not be what clears it, or it would be gone from every panel before you had decided whether to leave it up.",
            ),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('w'))
            .scene(
                "The agents, not the work",
                "`w` puts every running agent in place of the tree, in runs under the project each one belongs to and ordered by what it is waiting for rather than by what has to be done — the one question the tree cannot answer, because an agent with nothing to do has no work to be filed under. The marks are the header strip's, one per row: ← stopped on live work and waiting on you, ? stopped on a task parked with a question, in a colour of its own because an answer is a different thing to ask for than a nudge, ● running, ○ spare, · not saying. herdr reports only working or idle; which of the four an idle agent is comes from the task in its hands, which is the half the store knows. The heading is what the tree would have said by where it drew the row, and it is where the project used to be repeated down the right of every line; the groups come in the tree's own order and step in where one project lives inside another, because the panel has one spine and a second arrangement of the same projects is one more thing to hold in your head. Urgency is answered where it can be without moving a project — the strip is the whole census by state, and inside a run what wants you is at the top. Only the agents take the cursor — a heading neither folds nor selects, because every row you can reach here leads to a terminal, and that is what keeps ↵, `c` and 1-9 meaning what they mean.",
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
            .key(Key::Char('/'))
            .type_in("tuning")
            .scene(
                "Finding it by one word",
                "`/` narrows the tree to the tasks a phrase is in, and narrows it on every keystroke — the answer is the tree itself rather than something you get after pressing return. Here two tasks in two different projects, found without knowing either. That is the whole requirement: with two hundred and seventy-six tasks across thirty-one projects, the list stopped being something anybody could read down. Folds and the six-task cap are set aside while it is up, because a search whose answer is behind a fold is a search that says there is nothing — and nothing is *unfolded*, so the tree comes back exactly as you left it. Seats, shells and the counts on a project row go with them: those are questions about places and people, and this one is about work.",
            ),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('/'))
            .type_in("plate")
            .key(Key::Enter)
            .scene(
                "…including the word that is only in the prose",
                "Nothing in this title mentions a plate; the line that put the row here is in the task's overview. Most of what distinguishes one task from another in this store is written under the title rather than in it, so a search over titles alone would answer confidently and wrongly — which is worse than not searching. `↵` stops the typing and leaves the filter on, so every key goes on meaning what it means and the hits are rows you can `s`, `c` or `S`. The footer wears the phrase for as long as it is up, and `esc` puts the tree back.",
            ),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('i'))
            .scene("Showing ids", "i puts the id in front of each title — the thing you type at a shell, next to the thing you read. Off by default because most of an id is the project it is in, and the row is already sitting under that project's node. So the number alone is what shows: it is all `wsp start 003` needs, and all that separates the row from its siblings, since tasks are numbered inside their own project. The prefix comes back wherever the row is drawn away from that node — a flat list, a search, a dock — because there it is the half that says which task this is."),
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
            .now_in("a prompt")
            .says("Retune the plate decay")
            .scene("Adding to a project", "a on a project opens a field in the footer. The cursor's row decides the scope, so this becomes `wsp add … -p audio` without asking which project."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Project)
            .key(Key::Char('a'))
            .type_in("Retune the plate decay")
            .key(Key::Esc)
            .scene("Half a cancel", "`esc` on a line with something typed on it arms rather than cancelling, and the second press throws the typing away. The hands that reach this field come from vim, where `esc` is how you stop typing — so it lands here as a reflex, aimed at a mode the panel does not have, and it was costing whole titles that had to be typed again from memory. The two presses have to be next to each other: anything in between disarms. An empty line still closes on the first press, because there is nothing to lose, and `ctrl-c` cancels outright for anyone who wants one key."),
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
            .key(Key::Char('p'))
            .type_in("when the SIMD path lands")
            .scene("Parking, until something", "p is the other half of b. Blocked is addressed to a person — an answer is owed, and the row is red until it comes. Parked is addressed to nobody: it is a decision that the moment is wrong, so the prompt asks for the condition that would change that, and the row goes dim and sinks to the foot of its project."),
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
            .now_in("a confirmation")
            .did("nothing")
            .scene("Before removing", "X asks first, and the question carries the consequence — how many tasks would be displaced — because the answer changes with the row and you should not have to remember."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Task)
            .key(Key::Char('c'))
            .now_in("a pick")
            .did("nothing")
            .scene("Claiming", "c from a task picks the agent that takes it. From an agent row it runs the other way — pick the task it moves to, which is how one agent hands itself from one piece of work to the next."),
    );

    out.push(
        Driver::new(&w)
            .down_to(panel::RowKind::Agent)
            .key(Key::Char('c'))
            .scene("Migrating an agent", "The same key from the pane row. Landing on a task moves the agent to it: the task being left keeps its status — work underway with nobody on it is a real state — and gives up its claim, keeping the record of who had it. The cursor is on the pane row, and the pane row is what the tree carries to wherever it lands."),
    );

    out.push(
        Driver::new(&w)
            .to_pane("w2:p1")
            .key(Key::Char('u'))
            .scene("Before taking work back", "The five keys that generate or re-aim an agent — S C c f u — all ask, the way X does. Nothing here is a record you can retype: u releases the claim *and* empties the window, and an emptied context does not come back. The question names the agent rather than the pane id, and says whether it is still mid-task, which is the fact you want before answering and the one the tree does not show you."),
    );

    // ---- the mouse, and the pane the keyboard is in ----------------------
    //
    // Scripted for the first time here. The mouse had bitten twice in live use
    // — a herdr restart that broke it in every pane application, and a session
    // where selection worked but nothing fired — and neither had a test that
    // could have caught it, because the arithmetic was reachable and the
    // *sentence around it* was in the event loop. See `panel::apply_input`.

    out.push(
        Driver::new(&w)
            .click_on(panel::Target::Task("t-002".into()))
            .now_on("task t-002")
            .did("nothing")
            .scene(
                "Pointing at a row",
                "One click moves the cursor and does nothing else. A click that both selects and opens is how you end up somewhere you did not ask to be, on a row you had not read — and here ↵ can focus another pane, so that is a terminal you were not looking at. The row also stays under the pointer: a click pins the view where the frame left it, where a keystroke landing in the same place is owed rows beyond it.",
            ),
    );

    out.push(
        Driver::new(&w)
            .click_on(panel::Target::Task("t-002".into()))
            .click_again()
            .now_on("task t-002")
            .did("inspect")
            .scene(
                "Then activating",
                "The second click on the row already under the cursor means what ↵ means — it *becomes* that key rather than restating what opening a row does. So the two gestures cannot drift: whatever ↵ grows into on a task row, the second click grows into as well.",
            ),
    );

    out.push(
        Driver::new(&w)
            .focus(false)
            .click_on(panel::Target::Task("t-002".into()))
            .now_on("the inbox")
            .has_the_keyboard(true)
            .did("nothing")
            .scene(
                "A click into a pane nobody is working in",
                "The mouse reaches a pane the keyboard is not in — that is what makes the panel worth pointing at — and taking the keyboard is the whole of what this click did. The cursor has not moved: acting as well is the bounce Ed named, where pointing at the agent the cursor is already on means ↵, and focus arrives here and leaves again for that agent's terminal in the same gesture. The next click has the keyboard and means what it says.",
            ),
    );

    out.push(
        Driver::new(&w)
            .click_mark("w2:p1")
            .did("focus w2:p1")
            .has_the_keyboard(false)
            .now_on("the inbox")
            .scene(
                "Pointing at a mark in the strip",
                "The one clickable thing on the panel that is not a row. A mark *is* an agent, so there is nothing to select and one click goes there — the strip is a line of single columns, there is nothing to read on the way, and the ← you are reaching for is one you have already decided to answer. The keyboard goes with it, which is why the panel says so rather than waiting for herdr's census to agree.",
            ),
    );

    out.push(
        Driver::new(&w)
            .wheel(false)
            .wheel(false)
            .now_on("the inbox")
            .did("nothing")
            .scene(
                "Scrolling past the cursor",
                "The wheel moves the view and leaves the cursor where it is. What is selected is something you decided, not a consequence of where you are looking: dragging it along means a scroll to check something quietly moves the row the next verb acts on, and you have no way of knowing it did. Off the pane is a state the panel is allowed to be in — the next keystroke brings the view back to it. Unlike a click it is not gated on the keyboard being here, because a wheel cannot send you anywhere.",
            ),
    );

    out.extend(detail_scenes(&w));
    out.extend(board_scenes(&w));
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
        // A still frame of a pane nothing was driven through: there was no
        // transition, so there is nothing to claim about one.
        claims: Vec::new(),
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

/// The board, over the same fixture. A second surface on the same facts, so a
/// glyph or a colour that drifts in one drifts visibly against the other.
///
/// Wider than the panel and shorter, because that is what it is: the panel is a
/// sidebar and this opens in a tab of its own.
fn board_scenes(w: &Snapshot) -> Vec<Scene> {
    use crate::kanban::{self, Cursor, Scope};
    const BW: usize = 96;
    const BH: usize = 22;

    let ctx = kanban::Ctx {
        tasks: w.tasks.clone(),
        index: crate::resolve::Index::new(w.projects.clone()),
        bindings: w.bindings.clone(),
        // No `claimed_at`, for the reason the panel's fixture gives: a live
        // claim prints how long it has been held, and a fixture that says
        // "356d" is a fixture whose age is showing.
        claims: BTreeMap::new(),
        // Seated the way `Ctx::live` seats them, so a custodian in the fixture
        // draws on the board as it draws in the panel rather than as a spare
        // pair of hands.
        panes: kanban::seated(w.panes.clone(), &w.governors),
    };

    let shot = |title: &str, caption: &str, scope: Scope, cur: Cursor, done: bool, note: &str| {
        let board = kanban::collect(&ctx, &scope, done);
        let target = match board.card_at(&cur) {
            Some(c) => format!("task {}", c.id),
            None => "nothing".into(),
        };
        Scene {
            title: title.to_string(),
            caption: caption.to_string(),
            gesture: "K".into(),
            target,
            claims: Vec::new(),
            html: panel::to_html(&kanban::frame(&board, &cur, BW, BH, note), BW),
        }
    };

    // Where a key would land, pushed through the same reducer the live board
    // runs — so a scene can only show a state you could arrive at.
    let after = |scope: Scope, done: bool, keys: &[Key]| -> Cursor {
        let board = kanban::collect(&ctx, &scope, done);
        let mut cur = Cursor::default();
        for k in keys {
            kanban::apply_key(*k, &board, &mut cur);
        }
        cur
    };

    vec![
        shot(
            "The board",
            "K opens the work by state rather than by tree: one column per lane, cards in each. The same join the panel makes — what herdr says about a pane, against what the store holds — drawn as a mark on the card an agent is on. The column the cursor is in is named in the live ink, because a board with three empty columns otherwise gives no clue where a verb would land.",
            Scope::Everything,
            Cursor::default(),
            false,
            "",
        ),
        shot(
            "Scoped to a project",
            "A board of one project and everything filed beneath it — sub-projects included, since a parent whose work all lives in its children would otherwise open as four empty columns. Scoped, the cards stop naming their project: it is the heading.",
            Scope::Project("vst".into()),
            after(Scope::Project("vst".into()), false, &[Key::Char('l'), Key::Char('j')]),
            false,
            "",
        ),
        shot(
            "Finished work, brought back",
            "A shows the done column. Off by default because a board is what is in flight, and a year of completions would be the widest column on it — but the question \"what did we finish\" is asked often enough to be one key away.",
            Scope::Project("tooling".into()),
            after(Scope::Project("tooling".into()), true, &[Key::Char('l'), Key::Char('l'), Key::Char('l')]),
            true,
            "",
        ),
        shot(
            "A verb with nothing under it",
            "The cursor in an empty column. A key aimed at nothing has to say so in the footer — silence reads as a board that has stopped answering, which is the one thing a live surface must never look like.",
            Scope::Inbox,
            after(Scope::Inbox, false, &[Key::Char('l')]),
            false,
            "nothing here to send to review",
        ),
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
pre.wsp .q { color:#B08CD9; }
pre.wsp .sel { background:#E4EAE7; color:#0D1110; }
pre.wsp .sel .d, pre.wsp .sel .m, pre.wsp .sel .a, pre.wsp .sel .w,
pre.wsp .sel .q, pre.wsp .sel .p { color:#0D1110; }

/* A claim is a sentence the script made true, and it sits under the frame it
   is about rather than beside the caption: the caption is prose about why, and
   these are the facts a test is holding the code to. */
.claims { margin:.8rem 0 0; padding:0; list-style:none; display:flex;
  flex-direction:column; gap:.25rem; }
.claims li {
  font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  color:var(--sub);
}
.claims li::before { content:"\2713"; color:var(--accent); margin-right:.5rem; }
.claims li.broke { color:#C2453B; font-weight:600; }
.claims li.broke::before { content:"\2717"; color:#C2453B; }
.claims li.broke b { font-weight:400; }

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
.chip .q { color:#B08CD9; }
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
         <p class=\"eyebrow\">wsp · panel · detail · board</p>\
         <h1>Every state these surfaces can reach</h1>\
         <p>Each frame below is the real renderer over a fixed fixture — no herdr, \
         no store, no terminal. The flows are produced by pushing the keys shown \
         through the same reducer the live surface runs, so a scene can only show a \
         state you could actually arrive at. All three draw from one fixture, so a \
         glyph or a colour that drifts in one drifts visibly against the others.</p>\
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
        let mut claims = String::new();
        if !s.claims.is_empty() {
            claims.push_str("<ul class=\"claims\">");
            for c in &s.claims {
                match &c.broke {
                    None => claims.push_str(&format!("<li>{}</li>", esc(&c.said))),
                    Some(what) => claims.push_str(&format!(
                        "<li class=\"broke\">{} \u{2014} <b>{}</b></li>",
                        esc(&c.said),
                        esc(what),
                    )),
                }
            }
            claims.push_str("</ul>");
        }
        out.push_str(&format!(
            "<section class=\"scene\">\
             <div class=\"rail\"><h2>{}</h2><span class=\"gesture\">{}</span><p>{}</p>\
             <p class=\"tgt\">cursor is on <b>{}</b></p></div>\
             <div class=\"frame\">{}{}</div>\
             </section>\n",
            esc(&s.title),
            esc(&s.gesture),
            esc(&s.caption),
            esc(&s.target),
            s.html,
            claims,
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

/// Every claim the scripts made that did not hold, as `scene: said — but`.
///
/// The filmstrip and the assertions come out of one pass, so this is free: the
/// scenes have already been built by the time anyone asks. It is what the test
/// below checks and what the command prints, which means a broken transition
/// fails the build *and* is legible to whoever was only looking at the page.
fn broken(scenes: &[Scene]) -> Vec<String> {
    scenes
        .iter()
        .flat_map(|s| {
            s.claims.iter().filter_map(|c| {
                c.broke.as_ref().map(|b| format!("{}: {} \u{2014} {b}", s.title, c.said))
            })
        })
        .collect()
}

pub fn run(args: &crate::Args) -> i32 {
    let scenes = scenes();
    let html = page(&scenes);
    // Said on the way past rather than instead of writing the page: a page
    // showing where a claim broke is more use than no page, and the frame
    // beside the broken claim is usually the answer.
    let broke = broken(&scenes);
    for line in &broke {
        eprintln!("wsp: {line}");
    }

    match args.get("out") {
        Some(path) => {
            let p = crate::util::expand(&path);
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match crate::store::write_atomic(&p, &html) {
                Ok(()) => {
                    println!("wrote {} ({} scenes)", p.display(), scenes.len());
                    (!broke.is_empty()) as i32
                }
                Err(e) => {
                    eprintln!("wsp: {}: {e}", p.display());
                    1
                }
            }
        }
        None => {
            print!("{html}");
            (!broke.is_empty()) as i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filmstrip is only as good as its being built, and it was not.
    ///
    /// `wsp panel storyboard` panicked — `no row for pane w4:p2` — from the day
    /// an unclaimed agent stopped being a tree row, which is months of the page
    /// being unbuildable with nothing saying so. Nothing ran it: `board_scenes`
    /// had a test of its own and the panel's scenes had none, so the scripts
    /// that drive the whole thing were exercised only by a person typing the
    /// command, and nobody had a reason to.
    ///
    /// This is that reason. Every hunt in a script — `to_task`, `to_pane`,
    /// `to_spare` — panics when the row it names is not there, which is the
    /// design: a scene names the row it means so the caption underneath cannot
    /// go on describing a row that has moved. That only helps if something
    /// presses the button.
    #[test]
    fn every_scene_the_storyboard_ships_can_be_drawn() {
        let scenes = scenes();
        assert!(scenes.len() > 50, "the storyboard is {} scenes", scenes.len());
        for s in &scenes {
            assert!(!s.html.is_empty(), "{} drew nothing", s.title);
        }
    }

    /// And the assertions the scripts make hold.
    ///
    /// This is the half the filmstrip could never do. A frame catches a change
    /// in the drawing and fails with a diff; it cannot say "after this click
    /// the cursor is on that task and the effect is `inspect`", so a transition
    /// that broke while the picture stayed the same — which is every mouse
    /// failure this panel has had in live use — went past it. The claims are
    /// checked as the scenes are built, so the page and the assertions come out
    /// of one pass and cannot describe different runs.
    #[test]
    fn every_claim_the_storyboard_makes_holds() {
        let scenes = scenes();
        let claimed: usize = scenes.iter().map(|s| s.claims.len()).sum();
        assert!(claimed > 0, "no scene claims anything, so nothing is being checked");
        let broke = broken(&scenes);
        assert!(broke.is_empty(), "{}", broke.join("\n"));
    }

    /// A click acts on the row under the pointer, or on nothing. Every case
    /// here is one where being off by a single line would act on the wrong
    /// task — silently, and looking exactly like a misplaced click.
    fn ui_of(snap: &Snapshot, view: &panel::View) -> panel::Ui {
        panel::collect(snap, view)
    }

    /// What the row above `i` stands for, as the cursor moving up would find
    /// it: past the lines that belong to the row above *them* — where a
    /// project's work lives, what an agent is waiting for — none of which the
    /// cursor can stand on. Asking for `i - 1` instead makes every test about
    /// what is under what fail the day a row gains a second line.
    fn above(ui: &panel::Ui, i: usize) -> panel::Target {
        let mut probe = ui.clone();
        let mut at = i;
        while at > 0 {
            at -= 1;
            probe.select_for_test(at);
            if probe.selected_kind() != panel::RowKind::Detail {
                break;
            }
        }
        probe.selected_target()
    }

    /// A slot is drawn under the project it belongs to, wherever its occupant
    /// happens to be standing.
    ///
    /// The two are routinely different and that is the point: this custodian's
    /// pane resolves to `verb`, because `verb` is where the work it was last
    /// reading lives, and the slot is `wsp`'s. A tree that placed the row by
    /// the occupant would put the governor of `wsp` under `verb` — which is
    /// the arrangement Ed was looking at when he said he could not see a
    /// governor at all.
    #[test]
    fn a_slot_draws_under_its_own_project_and_not_under_its_occupant() {
        let mut w = world();
        w.governors.insert(
            "wsp".to_string(),
            json!({ "workspace": "w2", "pane": "w2:p1", "host": crate::util::hostname() }),
        );
        let view = panel::View::default();
        let ui = ui_of(&w, &view);

        let rows = ui.rows_for_target(&panel::Target::Seat("wsp".into()));
        assert_eq!(rows.len(), 1, "the slot is drawn once");
        assert_eq!(above(&ui, rows[0]), panel::Target::Project("wsp".into()));
        // And the occupant's own placement is `verb`, which is where it would
        // have been drawn by the tree's ordinary rules.
        assert!(
            ui.census_for_test().iter().any(|(_, a)| a.pane == "w2:p1"),
            "the occupant is still an agent on the machine"
        );
    }

    /// The place, which is what the whole task turns on: a governor of `wsp`
    /// draws under `wsp`.
    ///
    /// And draws there *instead of* under the task it borrowed. The seat in
    /// this fixture is claimed onto `t-003`, a `verb` task — the arrangement
    /// the live seat was in when Ed said *"I don't see you as governor, I still
    /// see you as robustness/078"* — so the assertion has both halves: the row
    /// is under the project it answers for, and it is not under the work it
    /// picked up to have somewhere to stand.
    #[test]
    fn a_governor_draws_under_the_project_it_governs_and_nowhere_else() {
        let w = seated_world();
        let view = panel::View::default();
        let ui = ui_of(&w, &view);

        let rows: Vec<usize> = ui.rows_for_target(&panel::Target::Seat("wsp".into()));
        assert_eq!(rows.len(), 1, "one position, one row");
        // Directly under its project: the row above it is `wsp` itself.
        assert_eq!(above(&ui, rows[0]), panel::Target::Project("wsp".into()));

        // The occupant is not drawn a second time in the tree as a worker on
        // the task it holds. It is still in the census at the foot, which is a
        // list of who is running rather than of where work sits.
        let tree = ui.rows_for_test() - ui.dock_for_test();
        let as_worker: Vec<usize> = ui
            .rows_for_target(&panel::Target::Pane("w2:p1".into()))
            .into_iter()
            .filter(|i| *i < tree)
            .collect();
        assert!(as_worker.is_empty(), "the seat is drawn as a worker too: {as_worker:?}");
        assert!(
            ui.census_for_test().iter().any(|(_, a)| a.pane == "w2:p1"),
            "and it fell out of the census, which is every agent running"
        );
    }

    /// The slot outlives its occupant, and that is a thing you can see.
    ///
    /// `verb`'s seat has nobody in it — stood down, or its workspace closed —
    /// and the row is still there, still selectable, still the place a person
    /// goes to fill it. Every other surface in wsp answers "no governor" and
    /// "never had one" with the same silence.
    #[test]
    fn an_empty_seat_is_still_drawn_and_can_still_be_stood_on() {
        let w = seated_world();
        let view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        let rows = ui.rows_for_target(&panel::Target::Seat("verb".into()));
        assert_eq!(rows.len(), 1, "the position went with the agent");
        ui.select_for_test(rows[0]);
        assert_eq!(ui.selected_kind(), panel::RowKind::Seat, "and the cursor can reach it");
    }

    /// What the row says is what the agent **is**, not what it holds.
    ///
    /// Both of the wrong answers came from asking the occupant for a task and
    /// finding none: `▣ unassigned` while the custodian's borrowed claim was
    /// released, then `▣ seat · empty`. A governor holding no task is the most
    /// assigned thing on the panel, so the row names the position — and names
    /// the project, which is the one fact a slot has whether or not anybody is
    /// sitting in it.
    #[test]
    fn a_seat_row_names_the_position_and_never_the_claim() {
        let mut w = seated_world();
        // The custodian with nothing in its hands: no claim, and a pane herdr
        // has nothing better to call than the word `claim` left behind.
        w.bindings.remove("w2:p1");
        if let Some(p) = w.panes.iter_mut().find(|p| p.pane == "w2:p1") {
            p.label = "unassigned".into();
        }
        let view = panel::View::default();
        let ui = ui_of(&w, &view);
        let rows = ui.rows_for_target(&panel::Target::Seat("wsp".into()));
        let text = panel::full_text_for_test(&ui, rows[0]);
        assert!(text.contains("governor"), "the row does not say what it is: {text}");
        assert!(text.contains("wsp"), "…or which project it is answerable for: {text}");
        assert!(!text.contains("unassigned"), "it went looking for a task again: {text}");

        // And the empty one says the same thing about the same project.
        let empty = ui.rows_for_target(&panel::Target::Seat("verb".into()));
        let text = panel::full_text_for_test(&ui, empty[0]);
        assert!(text.contains("governor") && text.contains("verb"), "{text}");
    }

    /// The same fact one surface further down: a custodian's row in the census
    /// says the post it is speaking from.
    ///
    /// Ed, 2026-08-17. Every worker in the dock reads `data/024 · what it said`
    /// and the governor read as the sentence alone — because `wsp say` scopes a
    /// sentence by the **task** the pane holds, and the one agent whose identity
    /// is a position rather than a piece of work holds none. The seat row in the
    /// tree above was already right; this is the row at the foot, which is read
    /// on its own and had nothing on it to say whose voice it was.
    #[test]
    fn a_governors_row_in_the_census_says_the_post_it_speaks_from() {
        let mut w = seated_world();
        // What the pane wears from its first `wsp say` onwards, which is what
        // `cmd_agent::say` writes for a pane holding no task: the sentence, and
        // nothing in front of it.
        if let Some(p) = w.panes.iter_mut().find(|p| p.pane == "w2:p1") {
            p.label = "restarted after herdr died; 02 is next".into();
        }
        // The whole census, so the assertion is about what the row says rather
        // than about which five of them the foot had room for.
        let (mut ui, _view) = showing(&w, &[Key::Char('w')]);
        let at = ui.rows_for_target(&panel::Target::Pane("w2:p1".into()));
        assert_eq!(at.len(), 1, "the custodian is in the census once");
        ui.select_for_test(at[0]);
        let row = panel::render_row_for_test(&ui, at[0], W).text();
        assert!(row.contains("governor · wsp"), "a bare sentence again: {row}");
        assert!(row.contains("restarted"), "and what it said is still the news: {row}");

        // A worker beside it is untouched: it already carried its scope, and
        // this must not put a second one in front of it.
        let worker = ui.rows_for_target(&panel::Target::Pane("w1:p1".into()));
        let row = panel::render_row_for_test(&ui, worker[0], W).text();
        assert!(!row.contains("governor"), "an ordinary agent wearing a post: {row}");
    }

    /// Vacancies draw once, at the top. Filled slots draw wherever they sit.
    ///
    /// Ed's rule, and the two states earn their space differently: a filled slot
    /// is a fact about who coordinates what, and a vacancy is an invitation. One
    /// invitation per level of a deep tree is clutter that hides the filled rows
    /// — so a vacancy under a slot that somebody is actually in does not draw at
    /// all, and nesting stays supported without being advertised at every node.
    #[test]
    fn a_vacancy_under_an_occupied_slot_does_not_draw() {
        let mut w = seated_world();
        // `trance` sits under `vst`, which sits under `audio`: a vacancy three
        // levels down with nothing above it draws, because there is nobody
        // above it to be the invitation instead.
        w.governors.insert("trance".to_string(), json!({ "vacated": "2026-08-17T04:00:00Z" }));
        let view = panel::View::default();
        let ui = ui_of(&w, &view);
        assert_eq!(ui.rows_for_target(&panel::Target::Seat("trance".into())).len(), 1);

        // Now put somebody in `vst` above it. The filled row draws and the
        // vacancy beneath it stops.
        w.governors.insert(
            "vst".to_string(),
            json!({ "workspace": "w1", "pane": "w1:p1", "host": crate::util::hostname() }),
        );
        let ui = ui_of(&w, &view);
        assert_eq!(ui.rows_for_target(&panel::Target::Seat("vst".into())).len(), 1, "the governor");
        assert!(
            ui.rows_for_target(&panel::Target::Seat("trance".into())).is_empty(),
            "the vacancy under it is clutter"
        );
        // A *filled* slot under a filled slot still draws: a nested governor is
        // a real arrangement, and this rule is about invitations only.
        w.governors.insert(
            "trance".to_string(),
            json!({ "workspace": "w3", "pane": "w3:p1", "host": crate::util::hostname() }),
        );
        let ui = ui_of(&w, &view);
        assert_eq!(ui.rows_for_target(&panel::Target::Seat("trance".into())).len(), 1);
    }

    /// The row that was wrong all night, on the surface a person actually looks
    /// at.
    ///
    /// t-260817-013 taught `wip` and the panel's sort that an idle custodian is
    /// not a person being the blocker, and left the *mark* alone — so the tree
    /// and the dock went on drawing `←` on the busiest agent on the machine.
    /// The seat is idle between the agents it starts, by construction, and that
    /// is its resting state rather than a stall.
    #[test]
    fn an_idle_custodian_never_draws_as_an_agent_that_wants_you() {
        let w = seated_world();
        let view = panel::View::default();
        let ui = ui_of(&w, &view);
        let state = ui
            .census_for_test()
            .into_iter()
            .find(|(_, a)| a.pane == "w2:p1")
            .map(|(s, _)| s)
            .expect("the custodian is running");
        assert_eq!(state, panel::AgentState::Seated, "idle in a slot is coordinating");

        // And the same pane without the slot is exactly what it always was, so
        // the exception costs nobody else anything.
        let ordinary = ui_of(&world(), &view)
            .census_for_test()
            .into_iter()
            .find(|(_, a)| a.pane == "w2:p1")
            .map(|(s, _)| s)
            .expect("the same pane, ungoverned");
        assert_eq!(ordinary, panel::AgentState::Asking);
    }

    /// The half a record could never be: you can say something to the governor
    /// from the row that is the governor.
    ///
    /// Addressed to the project rather than to the pane, which is the whole
    /// distinction — the sentence has to reach whoever is in the slot when it
    /// lands, not the pane the agent happened to start in. And it runs the CLI,
    /// like every other key here, so the shell and the panel cannot come to
    /// disagree about what talking to a seat means.
    #[test]
    fn t_says_something_to_whoever_is_in_the_seat() {
        let w = seated_world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        let rows = ui.rows_for_target(&panel::Target::Seat("wsp".into()));
        ui.select_for_test(rows[0]);

        assert!(matches!(panel::apply_key(Key::Char('T'), &mut ui, &mut view), panel::Effect::None));
        for c in "hold 060".chars() {
            panel::apply_key(Key::Char(c), &mut ui, &mut view);
        }
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["govern", "wsp", "--tell", "hold 060"]);
            }
            _ => panic!("T should say it through the CLI"),
        }
    }

    /// And the verb that would take the position apart is refused there.
    ///
    /// `f` sends an idle agent to find its own work, which for a custodian
    /// means picking up a task — the exact move that turned a governor into an
    /// agent working `robustness/078` and started this whole task off.
    #[test]
    fn a_seat_is_not_an_agent_to_be_sent_looking_for_work() {
        let w = seated_world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        let rows = ui.rows_for_target(&panel::Target::Seat("wsp".into()));
        ui.select_for_test(rows[0]);
        assert!(matches!(panel::apply_key(Key::Char('f'), &mut ui, &mut view), panel::Effect::None));
        // `↵` still goes to its terminal: the position is not addressable
        // *instead* of the agent in it.
        assert!(matches!(panel::apply_key(Key::Enter, &mut ui, &mut view), panel::Effect::Focus(_)));
    }

    /// The label wins because it is the only one of the three anybody keeps
    /// current. An agent's terminal title is its opening prompt frozen, so a
    /// pane three tasks on still answered with the first thing it was asked —
    /// confidently, and wrongly, which is the failure a blank would not be.
    #[test]
    fn a_pane_is_named_by_the_name_somebody_is_still_maintaining() {
        use crate::live::pane_name;

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

    // ---- the sentence the loop used to spell -----------------------------
    //
    // `click` and `wheel` were always reachable, and were always tested. What
    // was not is what the event loop did with the answer: a `Hit` became a
    // keystroke, or a focus call, or nothing, in twenty lines that only a
    // person at a trackpad could run. Both live mouse failures were in that
    // gap — selection worked and nothing fired — so these are about the
    // translation and not about the arithmetic under it.

    /// A click on the row the cursor is already on *is* `↵`, whatever `↵` has
    /// grown into.
    ///
    /// Compared against the key rather than against a named effect, which is
    /// the whole point: the two must not be able to drift. A test asserting
    /// `Effect::Inspect` would go on passing on the day `↵` on a task started
    /// doing something else and the click did not follow it.
    #[test]
    fn a_second_click_on_a_row_is_whatever_return_does_to_it() {
        let w = world();
        for kind in [panel::RowKind::Task, panel::RowKind::Project, panel::RowKind::Agent] {
            let mut bykey = Driver::new(&w);
            bykey.down_to(kind);
            let want = bykey.ui.selected_target();
            bykey.key(Key::Enter);

            let mut bymouse = Driver::new(&w);
            bymouse.down_to(kind);
            bymouse.click_again();
            assert_eq!(
                effect_label(&bymouse.effect),
                effect_label(&bykey.effect),
                "a second click on {} is not what ↵ is",
                target_label(&want),
            );
        }
    }

    /// Ed: "clicks should only perform their action if the pane was already
    /// focused, to avoid bouncing."
    ///
    /// The click arrives, the panel takes the keyboard, and that is the whole
    /// of what happened — the cursor has not moved and no effect was asked
    /// for. The *next* click means what it says. Driven here as two clicks in
    /// a row, which is the gesture a person makes and the one the loop's
    /// `had_keyboard` line existed for.
    #[test]
    fn a_click_into_an_unwatched_pane_takes_the_keyboard_and_nothing_else() {
        let w = world();
        let mut d = Driver::new(&w);
        let opened_on = d.ui.selected_target();
        d.focus(false);

        d.click_on(panel::Target::Task("t-002".into()));
        assert_eq!(d.ui.selected_target(), opened_on, "the click moved the cursor as well");
        assert_eq!(d.effect, panel::Effect::None, "and asked for something");
        assert!(d.keyboard, "the pane it landed in is now the one being worked in");

        d.click_on(panel::Target::Task("t-002".into()));
        assert_eq!(
            d.ui.selected_target(),
            panel::Target::Task("t-002".into()),
            "the second click still did not act",
        );
    }

    /// And the wheel is not gated on it: a wheel cannot send you anywhere, so
    /// there is nothing to bounce, and a pane you have to click into before you
    /// can scroll it is worse at the one thing the panel is for — being read
    /// from across the screen.
    #[test]
    fn the_wheel_reaches_a_pane_the_keyboard_is_not_in() {
        let w = world();
        let mut d = Driver::new(&w);
        d.focus(false);
        // What is at the top of the tree, which is what a scroll moves.
        let top = |d: &Driver| panel::row_at(&d.ui, &d.view, W, H, 2);
        let at = top(&d);
        d.wheel(false);
        assert_ne!(top(&d), at, "the wheel was swallowed the way a click is");
    }

    /// The `⋯` at the end of a clipped strip stands for the agents it could not
    /// draw, which is exactly what the agents view is — so it presses `w`.
    ///
    /// Against the key again, for the same reason as the second click: one
    /// mark, one keystroke, and no third statement of what "the rest of the
    /// agents" means.
    #[test]
    fn the_rest_of_a_clipped_strip_is_the_key_that_opens_the_agents() {
        let w = world();
        // Narrow enough that seven marks cannot fit beside the name and total.
        let mut bymouse = Driver::at(&w, 10, H);
        bymouse.click_rest();

        let mut bykey = Driver::at(&w, 10, H);
        bykey.key(Key::Char('w'));
        assert_eq!(
            bymouse.ui.selected_target(),
            bykey.ui.selected_target(),
            "the ⋯ and w land somewhere different",
        );
        assert_eq!(effect_label(&bymouse.effect), effect_label(&bykey.effect));
    }

    /// A mark in the strip is an agent, and the keyboard goes with it.
    ///
    /// Said by the panel rather than waited for: the census will agree within
    /// the tick, and a click landing inside that window would be the bounce
    /// with a stopwatch on it.
    #[test]
    fn pointing_at_a_mark_sends_the_keyboard_to_that_agent() {
        let w = world();
        let mut d = Driver::new(&w);
        d.click_mark("w2:p1");
        match &d.effect {
            panel::Effect::Focus(a) => assert_eq!(a.pane, "w2:p1"),
            other => panic!("a mark should go to a terminal, got {other:?}"),
        }
        assert!(!d.keyboard, "the panel still thinks it is the pane being worked in");
    }

    /// Focus is not drawn, and this is what that means in practice.
    ///
    /// `crate::draw`'s rule is that no renderer reads it — so the frame before
    /// and after the keyboard leaves is the same frame, and the only difference
    /// is what the next click does. A test that asserted a visible change would
    /// be asserting the rule is broken.
    #[test]
    fn the_keyboard_arriving_or_leaving_changes_nothing_that_is_drawn() {
        let w = world();
        let mut d = Driver::new(&w);
        let before = panel::to_html(&panel::frame(&d.ui, &mut d.view, W, H), W);
        d.focus(false);
        let after = panel::to_html(&panel::frame(&d.ui, &mut d.view, W, H), W);
        assert_eq!(before, after, "focus reached something that draws");
        assert_eq!(d.effect, panel::Effect::None, "and asked the loop for something");
    }

    /// Drive the panel the way a person would and hand back what it is
    /// showing: the reducer, then the rebuild the live loop does for it.
    fn showing(snap: &Snapshot, keys: &[Key]) -> (panel::Ui, panel::View) {
        let mut view = panel::View::default();
        let mut ui = ui_of(snap, &view);
        panel::place(&ui, &mut view, W, H);
        for k in keys {
            if let panel::Effect::Refetch = panel::apply_key(*k, &mut ui, &mut view) {
                panel::refetch_into(&mut ui, snap, &mut view);
            }
            // The draw between one key and the next, which is where the view
            // keeps its place. Skipping it leaves a panel whose tree derives
            // its offset from the cursor every frame — the thing this replaced.
            panel::place(&ui, &mut view, W, H);
        }
        (ui, view)
    }

    /// Every row of the agents view the cursor can reach leads to a terminal,
    /// which is what makes `↵`, `c` and the 1-9 hotkeys go on working inside
    /// it. The project headings are drawn and are not reachable, for exactly
    /// that reason: a heading you could land on is the one row where none of
    /// the three verbs mean anything.
    #[test]
    fn the_agents_view_is_every_agent_and_nothing_else() {
        let w = world();
        let (mut ui, _view) = showing(&w, &[Key::Char('w')]);

        let running =
            w.panes.iter().filter(|p| p.agent && p.label != panel::PANEL_LABEL).count();
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
        // Everything else in the view is one of those agents' own lines, or a
        // heading saying which project the run under it belongs to.
        assert!(kinds.iter().all(|k| matches!(
            k,
            panel::RowKind::Agent | panel::RowKind::Detail | panel::RowKind::Group
        )));

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

    /// Ed: "group agents by project on the agents panel, for easier
    /// visibility".
    ///
    /// Seven agents is a list you read; twenty is one you scan for the two that
    /// are yours. The project was already on every row, on the right, where a
    /// repeated word is least read — as a heading it is said once and the run
    /// under it answers "who is on this" without anybody counting.
    ///
    /// The groups come in the tree's order, through the same walk the tree
    /// makes. Ed: "we're not respecting the tree here, so my current version is
    /// render->strata-prototype->wsp even though render is _below_ wsp in the
    /// tree." Ordering them by who most wants you was a second arrangement of
    /// the same projects to hold in your head, and it read as a list that had
    /// lost the tree: `render` is inside `wsp` on every other surface.
    ///
    /// Urgency is answered where it can be without moving a project — the strip
    /// is the whole census by state, and inside each run the census's own order
    /// stands, so what wants you is at the top of the group it belongs to. The
    /// panes that resolve nowhere are last of all: `no project` is the least
    /// that can be said about a pane.
    #[test]
    fn the_agents_view_stands_each_agent_under_its_project() {
        let w = world();
        let (mut ui, _view) = showing(&w, &[Key::Char('w')]);

        let mut heading = String::new();
        let mut under: Vec<(String, String)> = Vec::new();
        let mut order: Vec<String> = Vec::new();
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            match ui.selected_kind() {
                panel::RowKind::Group => {
                    heading = panel::full_text_for_test(&ui, i);
                    order.push(heading.clone());
                }
                panel::RowKind::Agent => match ui.selected_target() {
                    panel::Target::Pane(p) => under.push((heading.clone(), p)),
                    t => panic!("an agent row that is not a pane: {t:?}"),
                },
                _ => {}
            }
        }

        // `trance` and `verb` are both inside `vst`, which has no agents and so
        // no heading; `wsp` is under `meta`, which has none either. The walk
        // goes through them to reach the three that do.
        assert_eq!(order, ["trance", "verb", "wsp", "no project"], "the groups, in the tree's order");
        assert_eq!(
            under,
            [
                // Spare before working, there as everywhere: there is nothing
                // to do about an agent that is busy. One is placed by the
                // checkout it stands in and one by the task it holds — the two
                // ways a pane comes to belong somewhere.
                ("trance".into(), "w4:p2".into()),
                ("trance".into(), "w1:p1".into()),
                // Asking, then blocked: the census's own order, kept inside
                // each run so an agent sits where the strip and the dock put it.
                ("verb".into(), "w2:p1".into()),
                ("verb".into(), "w3:p2".into()),
                ("wsp".into(), "w5:p2".into()),
                // Neither a task nor a checkout: a pane herdr reports from one
                // directory above every checkout, which is the commonest pane
                // there is.
                ("no project".into(), "w6:p1".into()),
                ("no project".into(), "w3:p1".into()),
            ],
            "every agent under the heading that places it",
        );

        // The heading says it, so the row does not say it again — the width
        // that word cost is the pane's name, which is what the row is for.
        let names: Vec<String> =
            (0..ui.rows_for_test()).map(|i| panel::render_row_for_test(&ui, i, W).text()).collect();
        let row = names.iter().find(|l| l.contains("Trance Video")).expect("w1:p1's row");
        assert!(!row.contains("trance"), "the project twice over: {row}");
    }

    /// And a project inside another is drawn inside it: the case Ed reported,
    /// where `render` lives under `wsp` and the view had them the other way up.
    /// The heading steps right and the agents under it do not — a run is read
    /// down one column whatever its heading is nested in, and thirty-four
    /// characters go on being thirty-four.
    #[test]
    fn a_project_inside_another_is_drawn_inside_it() {
        let mut w = world();
        // A child of `wsp` with a checkout inside `wsp`'s own, which is how
        // every real one of these is laid out.
        let mut child = project("panel", Some("wsp"));
        child.roots = vec!["~/claude/wsp/panel".into()];
        w.projects.push(child);
        // The pane that was standing nowhere, moved into it. Longest root wins,
        // so it lands in `panel` rather than the `wsp` it is also inside.
        let pane = w.panes.iter_mut().find(|p| p.pane == "w3:p1").expect("w3:p1");
        pane.cwd = "~/claude/wsp/panel".into();

        let (mut ui, _view) = showing(&w, &[Key::Char('w')]);
        let mut headings = Vec::new();
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_kind() == panel::RowKind::Group {
                headings.push(panel::render_row_for_test(&ui, i, W).text());
            }
        }
        let at = |name: &str| -> usize {
            let row = headings.iter().find(|h| h.contains(name)).unwrap_or_else(|| {
                panic!("no heading for {name} in {headings:?}")
            });
            row.chars().take_while(|c| *c == ' ').count()
        };
        assert!(
            headings.iter().position(|h| h.contains("wsp")).unwrap()
                < headings.iter().position(|h| h.contains("panel")).unwrap(),
            "the parent first: {headings:?}",
        );
        assert_eq!(at("panel"), at("wsp") + 1, "one step in, under the project it is inside");
        // And `trance`, whose parents have no agents and so no headings, starts
        // where `wsp` does: there is nothing on screen for it to be inside of.
        assert_eq!(at("trance"), at("wsp"), "no heading above it to be indented under");
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
        // Idle, holding a task blocked with a question on it — waiting on an
        // answer that is at least written down.
        assert!(row("waiting on the tuning").contains(panel::glyph::QUESTION));
        // Idle, holding nothing at all: a person's worth of attention going spare.
        assert!(row("spare hands").contains(panel::glyph::IDLE));
        // No state from herdr yet, and nothing here pretends otherwise.
        assert!(row("just started").contains(panel::glyph::QUIET));
        assert!(row("Trance Video").contains(panel::glyph::WORKING));

        // What wants you is at the top of the run it is in, because that is
        // what a list of agents is read for. The groups follow the tree and the
        // agents inside one follow their state, so the heading is never
        // between you and the row that is asking.
        let verb = lines.iter().position(|l| l.trim_start().starts_with("verb")).expect("verb");
        assert!(lines[verb + 1].contains(panel::glyph::NEEDS_YOU), "{}", lines[verb + 1]);
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
                .filter(|c| "←?●○·".contains(*c))
                .collect()
        };
        // One mark per agent, ordered as the list is: wants you, blocked,
        // spare, working, quiet.
        let want = "←?○○●●·";

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
        // The first agent — under its project's heading, which is why this is
        // asked for rather than counted to.
        let agent = ui.selected_index();
        assert_eq!(ui.selected_kind(), panel::RowKind::Agent);
        let detail = screen_row(&ui, &view, agent + 1);

        // The cursor is elsewhere, so the click brings it here.
        panel::apply_key(Key::Down, &mut ui, &mut view);
        assert_ne!(ui.selected_index(), agent);
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 2, detail, WORKING_HERE), panel::Hit::Select);
        assert_eq!(ui.selected_index(), agent, "the agent it belongs to");
        assert_eq!(ui.selected_kind(), panel::RowKind::Agent);

        // And the second click on the same words is `↵` on that agent, exactly
        // as a second click on its own row is: what the pointer is on has not
        // changed, so what the click means must not either.
        assert_eq!(
            panel::click(&mut ui, &mut view, W, H, 2, detail, WORKING_HERE),
            panel::Hit::Activate,
        );
    }

    /// And the heading over a run belongs to the agents *under* it. Walking up
    /// the way an agent's own lines do would land on the last agent of the
    /// group above — the one click on the panel that goes backwards, and the
    /// row furthest from the words the pointer was on.
    #[test]
    fn clicking_a_project_heading_selects_the_first_agent_under_it() {
        let w = world();
        let (mut ui, mut view) = showing(&w, &[Key::Char('w')]);
        // The second heading, so there is a group above it to be wrongly
        // pulled back into.
        let heading = (0..ui.rows_for_test())
            .filter(|i| {
                ui.select_for_test(*i);
                ui.selected_kind() == panel::RowKind::Group
            })
            .nth(1)
            .expect("the fixture has agents in more than one project");
        let y = screen_row(&ui, &view, heading);
        assert_eq!(panel::click(&mut ui, &mut view, W, H, 2, y, WORKING_HERE), panel::Hit::Select);
        assert_eq!(ui.selected_index(), heading + 1, "the first agent of the run it heads");
        assert_eq!(ui.selected_kind(), panel::RowKind::Agent);
    }

    /// Where a row is drawn, asked of the frame rather than counted from the
    /// top — every row above it is free to change height or arrive.
    fn screen_row(ui: &panel::Ui, view: &panel::View, row: usize) -> usize {
        (0..H)
            .find(|&y| panel::row_at(ui, view, W, H, y) == Some(row))
            .unwrap_or_else(|| panic!("row {row} is not on the pane"))
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
                panel::Hit::Focus(a) => assert_eq!(a.pane, agent.pane, "mark {i}"),
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

    /// Ed: "looks like that's only applied to the popover agents mode (w), not
    /// the inline agents panel — let's apply to both."
    ///
    /// One census, one arrangement of it. The section at the foot stands its
    /// agents under the same headings in the same tree order, and the project
    /// comes off the right of the rows there too — which is where that column
    /// cost most, since a pane's name has about half the width in the foot that
    /// it has in the tree.
    ///
    /// What the cap means is unchanged, and that is the half worth pinning
    /// down: the agents on screen are still the ones the census puts first, so
    /// an agent asking for you is never displaced by one that is merely busy in
    /// a project the tree happens to walk sooner. The cap picks; the tree only
    /// arranges what it picked.
    #[test]
    fn the_section_at_the_foot_is_grouped_the_same_way() {
        let w = world();
        let (mut ui, _view) = showing(&w, &[]);
        let census = ui.census_for_test();
        let docked = ui.dock_for_test();

        let mut headings = Vec::new();
        let mut panes = Vec::new();
        for i in ui.rows_for_test() - docked..ui.rows_for_test() {
            ui.select_for_test(i);
            match ui.selected_kind() {
                panel::RowKind::Group => headings.push(panel::full_text_for_test(&ui, i)),
                panel::RowKind::Agent => match ui.selected_target() {
                    panel::Target::Pane(p) => panes.push(p),
                    t => panic!("an agent row that is not a pane: {t:?}"),
                },
                _ => {}
            }
        }

        assert_eq!(headings, ["verb", "no project"], "the tree's order, in the foot");
        let first: Vec<String> =
            census.iter().take(panes.len()).map(|(_, a)| a.pane.clone()).collect();
        let mut sorted = panes.clone();
        sorted.sort();
        let mut want = first.clone();
        want.sort();
        assert_eq!(sorted, want, "the ones the census puts first, whatever the tree does");

        // That they are *arranged* rather than listed takes more of them than
        // the cap can afford here — three agents in two projects come out in
        // the order the census already had them. With the tail opened the whole
        // census is in the foot, and there the two orders visibly differ.
        {
            let mut view = panel::View::default();
            let mut ui = ui_of(&w, &view);
            show_all_agents(&w, &mut ui, &mut view);
            let mut opened = Vec::new();
            for i in ui.rows_for_test() - ui.dock_for_test()..ui.rows_for_test() {
                ui.select_for_test(i);
                if let panel::Target::Pane(p) = ui.selected_target() {
                    opened.push(p);
                }
            }
            let by_census: Vec<String> = census.iter().map(|(_, a)| a.pane.clone()).collect();
            assert_eq!(opened.len(), by_census.len(), "all of them, once asked");
            assert_ne!(opened, by_census, "and stood in runs rather than left in that order");
        }

        // The heading says the project, so the rows do not — the width goes to
        // the name, which in this pane is what was being cut.
        let names: Vec<String> =
            (0..ui.rows_for_test()).map(|i| panel::render_row_for_test(&ui, i, W).text()).collect();
        let row = names
            .iter()
            .filter(|l| l.contains("waiting on the tuning"))
            .next_back()
            .expect("the docked row for w3:p2");
        assert!(row.contains("waiting on the tuning table"), "cut short for a project: {row}");
    }

    /// The section keeps a few on screen and says how many there are, so the
    /// ones it cannot fit are never silently absent.
    ///
    /// Five *rows*, headings included, and that is the fix: it was five agents,
    /// and the headings were rows nobody had budgeted for — five agents in five
    /// projects cost eleven of them. A section that grows with the number of
    /// projects its agents happen to be in is a section a short pane cannot
    /// draw, and what it loses there is the agents rather than the headings —
    /// see [`a_short_pane_never_ends_the_foot_on_a_heading`]. So the headings
    /// come out of the five, the same way the tail does, and the `⋯` counts
    /// what they displaced exactly as it counts what the cap did.
    #[test]
    fn the_section_is_five_rows_and_counts_every_agent() {
        let w = world();
        let (mut ui, mut view) = showing(&w, &[]);
        let census = ui.census_for_test().len();
        assert!(census > 5, "the fixture has to outrun the cap to test it");

        let docked = ui.dock_for_test();
        let mut agents = 0;
        let mut headings = 0;
        for i in ui.rows_for_test() - docked..ui.rows_for_test() {
            ui.select_for_test(i);
            match ui.selected_kind() {
                panel::RowKind::Agent => agents += 1,
                panel::RowKind::Group => headings += 1,
                _ => {}
            }
        }
        assert!(headings > 1, "the fixture has to spread them across projects to test this");
        assert_eq!(headings + agents, 5, "five rows of census, headings and all");
        assert!(agents < census, "so a `⋯` says how many did not fit");
        // A heading, five rows and the `⋯`: what the section cost when there
        // were no headings in it, which is what a short pane was built around.
        assert_eq!(docked, 7, "the section is no taller than it was before the headings");
        // The heading counts every one of them, whatever it could draw.
        let head = panel::render_row_for_test(&ui, ui.rows_for_test() - docked, W);
        assert!(head.text().contains("agents") && head.text().contains(&census.to_string()));

        // `→` on the `⋯` opens the tail in place, exactly as a project's does.
        panel::apply_key(Key::Char('G'), &mut ui, &mut view);
        if let panel::Effect::Refetch = panel::apply_key(Key::Right, &mut ui, &mut view) {
            panel::refetch_into(&mut ui, &w, &mut view);
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

    /// Ed, at a pane sixteen rows tall: the foot drew `agents 5`, `fork 1`, one
    /// agent, and then the `wsp 4` heading with nothing under it.
    ///
    /// The geometry grants the dock what the pane can spare and the frame draws
    /// the *first* of what it was granted, so the cut lands where the rows
    /// happen to put it — and a group heading is the one row it must not land
    /// after. A heading on its own says a project has agents and then shows
    /// none of them, which is a worse use of the row than giving it back to the
    /// tree. Budgeting the section by rows made this rarer and could not make
    /// it impossible: a pane can always be one row shorter than the section.
    #[test]
    fn a_short_pane_never_ends_the_foot_on_a_heading() {
        const SHORT: usize = 16;
        let w = crowded_world();
        let (ui, view) = showing(&w, &[]);

        let drawn: Vec<usize> =
            (0..SHORT).filter_map(|y| panel::row_at(&ui, &view, W, SHORT, y)).collect();
        let from = ui.rows_for_test() - ui.dock_for_test();
        let foot: Vec<usize> = drawn.iter().copied().filter(|&i| i >= from).collect();
        assert!(
            foot.len() < ui.dock_for_test(),
            "the pane has to be too short for the whole section to test this",
        );

        let mut probe = ui.clone();
        let kind = |probe: &mut panel::Ui, i: usize| {
            probe.select_for_test(i);
            probe.selected_kind()
        };
        let last = *foot.last().expect("the dock is drawn at all");
        assert_ne!(
            kind(&mut probe, last),
            panel::RowKind::Group,
            "the section ended on a heading with nothing under it",
        );
        assert!(
            foot.iter().any(|&i| kind(&mut probe, i) == panel::RowKind::Agent),
            "and the rows it kept are agents rather than headings alone",
        );
        // The row the guard gave up is the tree's now, and not a blank: a dock
        // that shrank without the tree growing would be the same clip with a
        // hole in it. Two rows of title, three of footer and one rule over the
        // dock; every other line of a pane this short is a row you can click.
        assert_eq!(drawn.len(), SHORT - 6, "the row goes back to the tree, not to nobody");
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

    /// The whole of what `/` is for, stated as the test Ed set: find a task
    /// from one word of it, from the panel, without knowing which project it
    /// is in. Here one word reaches two tasks in two different projects, and
    /// takes everything else off the tree.
    #[test]
    fn one_word_finds_the_work_wherever_it_is_filed() {
        let w = world();
        let (ui, _) = showing(&w, &[Key::Char('/')]);
        let all = ui.rows_for_test();

        let (ui, _) = showing(&w, &keyed("/tuning"));
        let titles: Vec<String> =
            (0..ui.rows_for_test()).map(|i| panel::render_row_for_test(&ui, i, W).text()).collect();
        let tree: Vec<&String> = titles.iter().take(ui.rows_for_test() - ui.dock_for_test()).collect();

        assert!(ui.rows_for_test() < all, "the tree did not narrow at all");
        assert!(
            tree.iter().any(|l| l.contains("Apply reverb fixes")),
            "the hit in trance is missing: {tree:#?}"
        );
        assert!(
            tree.iter().any(|l| l.contains("Waiting on the tuning tab")),
            "the hit in verb is missing: {tree:#?}"
        );
        // And nothing that is not a hit or a branch on the way to one. `wsp`
        // is the fixture's biggest project and holds none of them.
        assert!(!tree.iter().any(|l| l.contains("Panel work item")), "{tree:#?}");
        assert!(!tree.iter().any(|l| l.contains("Buy new monitor")), "{tree:#?}");
    }

    /// A search over titles alone would answer confidently and wrongly. Most of
    /// what separates one task from another here is written under the title:
    /// this fixture's `verb` task says nothing about a plate, and its overview
    /// is where the comparison against one is written down.
    #[test]
    fn a_word_only_the_prose_holds_still_puts_the_row_on_the_tree() {
        let w = world();
        let (ui, _) = showing(&w, &keyed("/plate"));
        let tree: Vec<String> = (0..ui.rows_for_test() - ui.dock_for_test())
            .map(|i| panel::render_row_for_test(&ui, i, W).text())
            .collect();
        assert!(tree.iter().any(|l| l.contains("Retune the early")), "{tree:#?}");
        assert!(!tree.iter().any(|l| l.contains("Ship the release")), "{tree:#?}");
    }

    /// A search whose answer is behind a fold is a search that says there is
    /// nothing. So folds are set aside while one is up — and *set aside*, not
    /// undone: the tree comes back exactly as it was left, because a fold is a
    /// decision somebody made about a branch and a search is a question about
    /// four rows.
    #[test]
    fn a_search_reaches_into_a_folded_branch_and_leaves_the_fold_alone() {
        let w = world();
        let mut d = Driver::new(&w);
        // Folded the way a person folds it, on the row they are standing on.
        d.to_project("audio").key(Key::Char('h'));
        let drawn = |d: &Driver| -> Vec<String> {
            (0..d.ui.rows_for_test())
                .map(|i| panel::render_row_for_test(&d.ui, i, W).text())
                .collect()
        };
        let folded = drawn(&d);
        assert!(!folded.iter().any(|l| l.contains("Apply reverb fixes")), "{folded:#?}");

        d.key(Key::Char('/')).type_in("tuning");
        let found = drawn(&d);
        assert!(found.iter().any(|l| l.contains("Apply reverb fixes")), "{found:#?}");

        // …and `esc` gives the folded tree back, rather than the one the search
        // was drawing: the fold was never spent to answer the search.
        d.key(Key::Esc);
        assert_eq!(drawn(&d), folded, "the tree did not come back as it was left");
    }

    /// Which projects the tree is currently drawing, folded or not — the row
    /// existing is the question, since a project inside a folded one has no row
    /// at all.
    fn drawn_projects(d: &mut Driver) -> Vec<String> {
        let at = d.ui.selected_index();
        let mut out = Vec::new();
        for i in 0..d.ui.rows_for_test() {
            d.ui.select_for_test(i);
            if let panel::Target::Project(id) = d.ui.selected_target() {
                out.push(id);
            }
        }
        d.ui.select_for_test(at);
        out
    }

    /// The panes the pinned section at the foot is drawing — the census, not
    /// the copies of it the tree keeps under the work each one holds.
    fn docked_panes(d: &mut Driver) -> Vec<String> {
        let at = d.ui.selected_index();
        let tree = d.ui.rows_for_test() - d.ui.dock_for_test();
        let mut out = Vec::new();
        for i in tree..d.ui.rows_for_test() {
            d.ui.select_for_test(i);
            if let panel::Target::Pane(p) = d.ui.selected_target() {
                out.push(p);
            }
        }
        d.ui.select_for_test(at);
        out
    }

    /// `h` folds one row, and the store is thirty-one projects: getting back to
    /// a tree you can read a branch at a time is the walk `H` is for. What it
    /// leaves is the top of the tree and nothing hanging off it.
    #[test]
    fn folding_the_whole_tree_leaves_the_top_of_it_and_nothing_under_it() {
        let w = world();
        let mut d = Driver::new(&w);
        d.key(Key::Char('H'));

        assert!(drawn_tasks(&mut d).is_empty(), "{:?}", drawn_tasks(&mut d));
        let projects = drawn_projects(&mut d);
        assert!(projects.contains(&"audio".to_string()), "{projects:?}");
        assert!(projects.contains(&"meta".to_string()), "{projects:?}");
        assert!(
            !projects.contains(&"vst".to_string()),
            "a project inside a folded one is still drawn: {projects:?}"
        );
    }

    /// The dock under the tree is not part of the tree, and `H` is a key about
    /// the tree.
    ///
    /// Ed, 2026-08-18: "I can't see any agents in the wsp inline agents panel."
    /// The stored view had `(agents)` folded in the same set as the twenty-odd
    /// projects he had just put away with one press, and a folded section is a
    /// heading with a count and a `▸` — so the panel went on saying five agents
    /// and drew none of them, with nothing on screen to say which key had done
    /// it or which would undo it. The census at the foot is pinned precisely so
    /// that who is running survives whatever the tree is doing; folding it as a
    /// side effect of tidying something else is the one thing that must not
    /// happen to it. `h` on the section is still how a person puts it away.
    #[test]
    fn folding_the_whole_tree_leaves_the_census_pinned_under_it() {
        let w = world();
        let mut d = Driver::new(&w);
        let before = docked_panes(&mut d);
        assert!(!before.is_empty(), "the fixture has nobody at the foot to lose");

        d.key(Key::Char('H'));
        assert_eq!(docked_panes(&mut d), before, "`H` took the agents dock with the tree");
    }

    /// And `L` reaches folds it cannot see. It clears the set rather than
    /// walking the rows, which is what makes it the one of the two that is
    /// exact: a branch shut by hand before `H` — and so hidden inside it after
    /// — comes back with everything else.
    #[test]
    fn unfolding_the_whole_tree_reaches_the_folds_that_are_out_of_sight() {
        let w = world();
        let mut d = Driver::new(&w);
        d.to_project("vst").key(Key::Char('h'));
        assert!(!drawn_tasks(&mut d).contains(&"t-001".to_string()), "the fold already shows it");

        d.keys(&[Key::Char('H'), Key::Char('L')]);
        assert!(
            drawn_tasks(&mut d).contains(&"t-001".to_string()),
            "a fold made before `H` outlived `L`",
        );
    }

    /// `<` reads the branch off wherever the cursor is standing, rather than
    /// asking for a walk up to the heading first. Here the cursor is on a task
    /// in `tooling`, and what folds is `tooling`'s — the project inside it, with
    /// all of its work.
    ///
    /// The task the cursor is on stays: folding what is *in* a branch is not
    /// folding the branch, which is `h` and one key along.
    #[test]
    fn a_branch_is_the_one_the_cursor_is_standing_in() {
        let w = world();
        let mut d = Driver::new(&w);
        d.to_task("t-005").key(Key::Char('<'));

        let tasks = drawn_tasks(&mut d);
        assert!(tasks.contains(&"t-005".to_string()), "the row it was pressed on went: {tasks:?}");
        assert!(
            !tasks.iter().any(|id| id.starts_with("t-1")),
            "the project inside it kept its work on screen: {tasks:?}",
        );
        assert!(
            drawn_projects(&mut d).contains(&"wsp".to_string()),
            "and that project's own row went with it",
        );
    }

    /// One press each way, at any depth. This is what decides that `<` shuts
    /// the head's children and stops: the rows below them are off the screen
    /// either way, and leaving their folds alone is what leaves `>` something
    /// to open. Fold `audio` and the whole of `vst`, `trance` and `verb` goes
    /// with it — and comes back on the next keystroke rather than the third.
    #[test]
    fn a_branch_folded_by_one_press_comes_back_in_one() {
        let w = world();
        let mut d = Driver::new(&w);
        d.to_project("audio").key(Key::Char('<'));

        let projects = drawn_projects(&mut d);
        assert!(projects.contains(&"vst".to_string()), "the head folded itself too: {projects:?}");
        assert!(!projects.contains(&"trance".to_string()), "{projects:?}");
        assert!(!drawn_tasks(&mut d).contains(&"t-001".to_string()), "two levels down is drawn");

        d.key(Key::Char('>'));
        assert!(drawn_projects(&mut d).contains(&"trance".to_string()), "one press back was not enough");
        assert!(drawn_tasks(&mut d).contains(&"t-001".to_string()), "one press back was not enough");
    }

    /// The fold keys stay live inside a pick, like the ones they are the plural
    /// of: a pick is a hunt for one project among thirty, and `H` is the tree
    /// out of the way. What it must not do is answer the pick — `<`, `>`, `H`
    /// and `L` change what is on screen and nothing in the store, and `↵` still
    /// takes the row it lands on afterwards.
    #[test]
    fn folding_the_tree_is_still_folding_it_while_a_pick_is_up() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        let rows = ui.rows_for_target(&panel::Target::Task("t-001".into()));
        ui.select_for_test(rows[0]);

        assert!(matches!(panel::apply_key(Key::Char('m'), &mut ui, &mut view), panel::Effect::None));
        assert!(matches!(
            panel::apply_key(Key::Char('H'), &mut ui, &mut view),
            panel::Effect::Refetch
        ));
        panel::refetch_into(&mut ui, &w, &mut view);

        let rows = ui.rows_for_target(&panel::Target::Project("meta".into()));
        ui.select_for_test(rows[0]);
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["mv", "t-001", "-p", "meta", "--parent", "none"]);
            }
            _ => panic!("the pick stopped taking the row it lands on"),
        }
    }

    /// A filter that hides most of the store has to say it is on. `A` and `R`
    /// each wear a word in the footer for that reason and this is the one that
    /// hides the most — a tree narrowed to two rows with nothing to say why is
    /// a panel that reads as an empty backlog.
    #[test]
    fn the_search_says_so_in_the_footer_until_it_is_cleared() {
        let w = world();
        let foot = |ui: &panel::Ui, view: &mut panel::View| panel::frame(ui, view, W, H)[H - 2].text();

        let (ui, mut view) = showing(&w, &keyed("/tuning"));
        assert!(foot(&ui, &mut view).contains("/tuning"), "{}", foot(&ui, &mut view));

        // `esc` in the browse chain clears it — after the key map and before
        // the detail pane, because it is the thing most in front of you.
        let (ui, mut view) = showing(&w, &[&keyed("/tuning")[..], &[Key::Enter, Key::Esc]].concat());
        assert!(!foot(&ui, &mut view).contains("/tuning"), "{}", foot(&ui, &mut view));
    }

    /// A search that matches nothing draws an empty tree, which looks exactly
    /// like a panel that has broken — and it happens on the way to every hit,
    /// because a word is typed one letter at a time.
    #[test]
    fn a_search_that_matches_nothing_says_which_it_is() {
        let w = world();
        let (ui, mut view) = showing(&w, &keyed("/kalimba"));
        let frame = panel::frame(&ui, &mut view, W, H);
        let text: Vec<String> = frame.iter().map(|l| l.text()).collect();
        assert!(text.iter().any(|l| l.contains("nothing matches")), "{text:#?}");
        assert!(text.iter().any(|l| l.contains("esc clears")), "{text:#?}");
    }

    /// `/` then the phrase, as a keyboard would send it.
    fn keyed(s: &str) -> Vec<Key> {
        s.chars().map(Key::Char).collect()
    }

    /// The agents view and a search are the same kind of pair as the agents
    /// view and `R`: one switch from either side. A search left on under a
    /// list of panes it has not touched is a footer describing a tree that is
    /// not on screen.
    #[test]
    fn the_agents_view_and_a_search_each_put_the_other_away() {
        let w = world();
        let foot = |ui: &panel::Ui, view: &mut panel::View| panel::frame(ui, view, W, H)[H - 2].text();

        let (ui, mut view) = showing(&w, &[&keyed("/tuning")[..], &[Key::Enter, Key::Char('w')]].concat());
        let f = foot(&ui, &mut view);
        assert!(f.contains("agents") && !f.contains("/tuning"), "{f}");

        // And the other way: `/` pressed in the agents view means the tree.
        let (ui, mut view) =
            showing(&w, &[&[Key::Char('w')][..], &keyed("/tuning"), &[Key::Enter]].concat());
        let f = foot(&ui, &mut view);
        assert!(f.contains("/tuning") && !f.contains("agents"), "{f}");
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
        panel::refetch_into(&mut ui, &w, &mut view);
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

    /// `Z` opens the whole tree in a tab, and in that tab it closes it again.
    ///
    /// The asymmetry underneath is the point. The sidebar is installed
    /// furniture: quitting it costs a reinstall and buys nothing, so `q` refuses
    /// and says which key does mean it. The tab is the opposite — somebody
    /// opened it a minute ago, `Z` opens it again for nothing, and a fullscreen
    /// whose exit you have to hunt for is one you close by closing the tab.
    #[test]
    fn the_tab_z_opens_is_the_tab_z_closes() {
        let w = world();
        let quits = |full: bool, k: Key| -> bool {
            let mut view = panel::View::default();
            view.takes_the_tab(full);
            let mut ui = ui_of(&w, &view);
            matches!(panel::apply_key(k, &mut ui, &mut view), panel::Effect::Quit)
        };

        // In the sidebar, `Z` asks for the tab and nothing closes anything.
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        assert!(matches!(
            panel::apply_key(Key::Char('Z'), &mut ui, &mut view),
            panel::Effect::Full
        ));
        assert!(!quits(false, Key::Char('q')), "a stray q must not take the sidebar down");
        assert!(!quits(false, Key::Char('Z')));

        // In the tab, all three ways of saying "put this away" put it away.
        for k in [Key::Char('Z'), Key::Char('q'), Key::Esc] {
            assert!(quits(true, k), "{k:?} should close the fullscreen tab");
        }
        // …but not over something that is itself open. The key map goes first,
        // as it does in the sidebar — closing the whole tab to put away a list
        // of keys would be the panel taking the loudest possible reading of
        // `esc`. The detail pane sits between the two and is guarded the same
        // way, one line above this in `browse_key`.
        let mut view = panel::View::default();
        view.takes_the_tab(true);
        view.set_help_for_test(true);
        let mut ui = ui_of(&w, &view);
        assert!(matches!(
            panel::apply_key(Key::Esc, &mut ui, &mut view),
            panel::Effect::None
        ));
        assert!(quits(true, Key::Esc), "and once it is closed, esc means the tab again");
    }

    /// A page shows the branch whole; a sidebar shows six of it and says so.
    ///
    /// The cap is an economy: six tasks is what one project may spend of a
    /// column that has to hold thirty projects, and the `⋯` is how the tail
    /// stays reachable. A pane wide enough to be read rather than glanced at has
    /// no such shortage, so the whole branch is there — which is what `Z` is
    /// pressed for, and it follows the width rather than the key, so dragging a
    /// split wider gets the same tree.
    #[test]
    fn a_page_shows_the_branch_whole_and_a_sidebar_shows_six() {
        let world = world();
        let tasks_of = |w: usize| -> (Vec<String>, usize) {
            let mut view = panel::View::default();
            view.fit_to_pane(w);
            let mut ui = ui_of(&world, &view);
            let mut ids = Vec::new();
            for i in 0..ui.rows_for_test() {
                ui.select_for_test(i);
                if let panel::Target::Task(id) = ui.selected_target() {
                    ids.push(id);
                }
            }
            let more =
                kinds(&mut ui).into_iter().filter(|k| *k == panel::RowKind::More).count();
            (ids, more)
        };

        // The fixture's `wsp` project holds eight, which is what the overflow
        // row exists for.
        let (sidebar, sidebar_more) = tasks_of(W);
        let (page, page_more) = tasks_of(120);
        let panel_work = |ids: &[String]| ids.iter().filter(|id| id.starts_with("t-1")).count();

        assert_eq!(panel_work(&sidebar), 6, "a sidebar spends six rows on one project");
        assert_eq!(panel_work(&page), 8, "a page shows the branch whole");
        assert!(page.len() > sidebar.len(), "and the tree is longer for it");

        // One `⋯` fewer, not none: the other belongs to the agents dock, which
        // keeps five rows however wide the pane is — it is a pinned list of who
        // has stopped, and a page is not a reason to grow it.
        assert_eq!(
            sidebar_more,
            page_more + 1,
            "the ⋯ the page loses is the project's tail, and the one it keeps is the dock's",
        );
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

    /// Press a key that asks first, and say yes.
    ///
    /// Every key that generates or re-aims an agent goes behind a y/n, so the
    /// tests that are about *what the key does* would otherwise all be about
    /// the question instead. This asserts the question happened — a key that
    /// stopped asking fails here — and then answers it, so what comes back is
    /// the deed, exactly as the un-guarded press used to return it.
    fn confirmed(ui: &mut panel::Ui, view: &mut panel::View, c: char) -> panel::Effect {
        match panel::apply_key(Key::Char(c), ui, view) {
            panel::Effect::None => {}
            _ => panic!("{c} should ask before it acts, not act"),
        }
        assert!(matches!(view.mode, panel::Mode::Confirm { .. }), "{c} put up no question");
        press(ui, view, 'y')
    }

    /// The same, for the `↵` that finishes a pick onto an agent.
    fn confirmed_enter(ui: &mut panel::Ui, view: &mut panel::View) -> panel::Effect {
        match panel::apply_key(Key::Enter, ui, view) {
            panel::Effect::None => {}
            _ => panic!("a pick onto an agent should ask before it acts"),
        }
        assert!(matches!(view.mode, panel::Mode::Confirm { .. }), "the pick put up no question");
        press(ui, view, 'y')
    }

    /// Open the agents section past the five rows it keeps on screen, the way
    /// `→` on its `⋯` does. A test that wants an agent the foot could not fit
    /// has to ask for it exactly as a person would — and the last overflow row
    /// in the tree is the section's, since it is the last thing in the list.
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
            panel::refetch_into(ui, snap, view);
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
        show_all_agents(&w, &mut ui, &mut view);
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
        match confirmed_enter(&mut ui, &mut view) {
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
        show_all_agents(&w, &mut ui, &mut view);
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
        match confirmed_enter(&mut ui, &mut view) {
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
    /// the whole difference between a place to work and a colleague — and with
    /// it, whether the screen goes there. The panel
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
            // `--focus` because `O` is a person asking to be put somewhere:
            // `spawn` itself no longer moves the screen, and a key that opened a
            // workspace out of sight would read as having done nothing.
            panel::Effect::Spawn { argv, .. } => assert_eq!(argv, ["spawn", "t-003", "--focus"]),
            _ => panic!("O on a task opens a workspace for it"),
        }
        match confirmed(&mut ui, &mut view, 'S') {
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
        match confirmed(&mut ui, &mut view, 'S') {
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
        show_all_agents(&w, &mut ui, &mut view);
        on_pane(&mut ui, "w4:p2");
        match confirmed(&mut ui, &mut view, 'f') {
            panel::Effect::Tell(t) => {
                assert_eq!(t.pane, "w4:p2");
                let said = t.text.clone().expect("a sentence, not just a clear");
                assert!(said.contains("wsp next -p trance"), "said: {said}");
            }
            _ => panic!("f on an idle unassigned agent should tell it something"),
        }
    }

    /// The commonest agent on the machine at four in the afternoon: one that
    /// has finished, put its task to `review`, and is standing there idle
    /// holding work only a person can close. `f` refused on it for a year
    /// because the row carried the task's id and not its status, and the `v` it
    /// asked for first would have changed nothing — `claim` leaves the last
    /// task where it is.
    #[test]
    fn an_agent_that_has_handed_its_work_back_is_still_sent_looking() {
        let mut w = world();
        // t-004 is at `review`, and the pane holding it is idle. Nothing else
        // about the pane changes.
        w.bindings.insert("w4:p2".into(), json!({ "task_id": "t-004" }));
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w4:p2");
        match confirmed(&mut ui, &mut view, 'f') {
            panel::Effect::Tell(t) => {
                assert_eq!(t.pane, "w4:p2");
                let said = t.text.clone().expect("a sentence, not just a clear");
                assert!(said.contains("wsp next -p verb"), "said: {said}");
            }
            _ => panic!("an agent whose work is at review is free to be sent looking"),
        }
    }

    /// And the half of the old refusal that was right. An agent in the middle
    /// of a task keeps it: work taken off one halfway through is work done
    /// twice, so `f` still says no and still names the key that hands it back.
    #[test]
    fn an_agent_in_the_middle_of_a_task_is_left_alone() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        // Idle, so it is only what it holds — t-003, at `doing` — that refuses.
        on_pane(&mut ui, "w2:p1");
        assert!(matches!(press(&mut ui, &mut view, 'f'), panel::Effect::None));
        // A refusal, not a question: `f` answers `None` both when it declines
        // and when its y/n is up, so the mode is what tells the two apart.
        assert!(matches!(view.mode, panel::Mode::Browse), "a refusal leaves no question up");
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
        show_all_agents(&w, &mut ui, &mut view);
        on_pane(&mut ui, "w4:p2");
        match confirmed(&mut ui, &mut view, 'f') {
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

        // Refused, not asked. `f` returns `Effect::None` both when it declines
        // and when it has put its y/n up, so the mode is what tells the two
        // apart — and a refusal that left a question standing would be a key
        // that says no and then offers to do it anyway.
        let refused = |ui: &mut panel::Ui, view: &mut panel::View| {
            let e = press(ui, view, 'f');
            matches!(e, panel::Effect::None) && matches!(view.mode, panel::Mode::Browse)
        };

        on_pane(&mut ui, "w5:p1"); // a shell standing in ~/claude/wsp
        assert!(refused(&mut ui, &mut view));

        show_all_agents(&w, &mut ui, &mut view);
        on_pane(&mut ui, "w3:p1"); // an agent working, holding no task
        assert!(refused(&mut ui, &mut view));

        on_pane(&mut ui, "w1:p1"); // an agent already holding t-001
        assert!(refused(&mut ui, &mut view));
    }

    /// The other door to the same place. A claim is a fact in the store and
    /// nothing at all in the pane it names, so handing a task to an idle agent
    /// has to carry the sentence that tells it.
    #[test]
    fn claiming_onto_an_idle_agent_tells_it_what_it_now_holds() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        show_all_agents(&w, &mut ui, &mut view);

        // `c` on a task turns the tree into the picker; the pick lands on a pane.
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Task("t-020".into()) {
                break;
            }
        }
        press(&mut ui, &mut view, 'c');
        on_pane(&mut ui, "w4:p2");
        match confirmed_enter(&mut ui, &mut view) {
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
            show_all_agents(w, &mut ui, &mut view);
            for i in 0..500 {
                ui.select_for_test(i);
                if ui.selected_target() == panel::Target::Task("t-020".into()) {
                    break;
                }
            }
            press(&mut ui, &mut view, 'c');
            on_pane(&mut ui, "w4:p2");
            match confirmed_enter(&mut ui, &mut view) {
                panel::Effect::Run { then, .. } => then.expect("an idle agent is told").clear,
                _ => panic!("the pick should run a claim"),
            }
        };

        assert_eq!(clear_before_the_claim(&world()), Some("/clear"));

        let mut other = world();
        for p in other.panes.iter_mut().filter(|p| p.pane == "w4:p2") {
            p.kind = "codex".into();
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
        match confirmed_enter(&mut ui, &mut view) {
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
        show_all_agents(&w, &mut ui, &mut view);
        for i in 0..500 {
            ui.select_for_test(i);
            if ui.selected_target() == panel::Target::Task("t-020".into()) {
                break;
            }
        }
        press(&mut ui, &mut view, 'c');
        on_pane(&mut ui, "w4:p2");
        match confirmed_enter(&mut ui, &mut view) {
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
        match confirmed_enter(&mut ui, &mut view) {
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
        match confirmed_enter(&mut ui, &mut view) {
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
        match confirmed(&mut ui, &mut view, 'C') {
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
            match confirmed(ui, view, 'C') {
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
            panel::refetch_into(&mut ui, &w, &mut view);
        }
        assert!(
            !has(&ui, &panel::Target::Pane("w6:p1".into())),
            "the review filter is the case: no agent row is on screen at all",
        );
        on_task(&mut ui, "t-004");
        match confirmed(&mut ui, &mut view, 'C') {
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
        for p in w.panes.iter_mut().filter(|p| p.agent) {
            p.state = "working".into();
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
        show_all_agents(&w, &mut ui, &mut view);
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
        match confirmed(&mut ui, &mut view, 'u') {
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
        match confirmed(&mut ui, &mut view, 'u') {
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
        match confirmed(&mut ui, &mut view, 'u') {
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
        for p in w.panes.iter_mut().filter(|p| p.pane == "w2:p1") {
            p.kind = "codex".into();
        }
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_pane(&mut ui, "w2:p1");
        match confirmed(&mut ui, &mut view, 'u') {
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
        show_all_agents(&w, &mut ui, &mut view);

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

    // ---- the question in front of all five ---------------------------------

    /// The five keys that make an agent or move one all ask before they act.
    ///
    /// They are the only keys in the panel whose damage cannot be typed back:
    /// `S` spends a context window, `C`, `c` and `f` aim one at work, and `u`
    /// empties one. Everything else here writes a field that another key
    /// rewrites. And each sits one shift or one finger from something harmless
    /// — `s`, `c`, `d`, `i` — so this is a list of what a slip costs, written
    /// out by name rather than left to whichever test happens to press each
    /// key.
    #[test]
    fn every_key_that_makes_or_moves_an_agent_asks_first() {
        let w = world();

        // Each key with a row it would actually work on: the question only
        // comes up where the key does something, and a refusal is not a
        // question — see `nothing_is_typed_into_a_shell_or_a_busy_agent`.
        let asks = |aim: &dyn Fn(&mut panel::Ui), k: char| {
            let mut view = panel::View::default();
            let mut ui = ui_of(&w, &view);
            show_all_agents(&w, &mut ui, &mut view);
            aim(&mut ui);
            assert!(
                matches!(press(&mut ui, &mut view, k), panel::Effect::None),
                "{k} acted instead of asking",
            );
            match &view.mode {
                panel::Mode::Confirm { question, .. } => question.clone(),
                other => panic!("{k} left the panel in {other:?} rather than asking"),
            }
        };

        // `S`, on a task: a model and a context window.
        assert!(asks(&|ui| on_task(ui, "t-003"), 'S').contains("t-003"));
        // `C`, on a task in a project with somebody spare: the panel picked the
        // pane, so the question is the only place that name is shown.
        assert!(asks(&|ui| on_task(ui, "t-001"), 'C').contains("t-001"));
        // `u`, on an agent holding work.
        assert!(asks(&|ui| on_pane(ui, "w2:p1"), 'u').contains("t-003"));
        // `f`, on an idle agent standing in a project it can be sent into.
        assert!(asks(&|ui| on_pane(ui, "w4:p2"), 'f').contains("trance"));

        // `c` is the odd one: the key opens a pick and the deed is the `↵` that
        // ends it, so the question belongs on the second act rather than the
        // first. Asking twice would make the pick itself the thing you confirm.
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        show_all_agents(&w, &mut ui, &mut view);
        on_task(&mut ui, "t-020");
        press(&mut ui, &mut view, 'c');
        assert!(matches!(view.mode, panel::Mode::Pick { .. }), "c opens the pick without asking");
        on_pane(&mut ui, "w4:p2");
        assert!(matches!(panel::apply_key(Key::Enter, &mut ui, &mut view), panel::Effect::None));
        match &view.mode {
            panel::Mode::Confirm { question, .. } => assert!(question.contains("t-020")),
            other => panic!("the pick landed on a pane and did not ask: {other:?}"),
        }
    }

    /// And the cheap neighbours do not, because a y/n on every keystroke is a
    /// y/n nobody reads. `O` costs a directory; `m` writes a field that `m`
    /// puts back.
    #[test]
    fn the_keys_that_cost_nothing_irreversible_do_not_ask() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_task(&mut ui, "t-003");
        assert!(matches!(press(&mut ui, &mut view, 'O'), panel::Effect::Spawn { .. }));
        assert!(matches!(view.mode, panel::Mode::Browse));

        press(&mut ui, &mut view, 'm');
        on_kind(&mut ui, panel::RowKind::Project);
        assert!(matches!(
            panel::apply_key(Key::Enter, &mut ui, &mut view),
            panel::Effect::Run { .. }
        ));
        assert!(matches!(view.mode, panel::Mode::Browse), "a move is not worth a question");
    }

    /// `n` is the whole point of the question: the deed is held on the mode and
    /// nothing outside it ever saw it, so refusing drops it. The bug this
    /// guards against is a confirm that closes and runs anyway — which is what
    /// an argv passed to the effect *before* the question would have done.
    #[test]
    fn saying_no_drops_the_deed_rather_than_deferring_it() {
        let w = world();
        for k in ['S', 'C', 'u', 'f'] {
            let mut view = panel::View::default();
            let mut ui = ui_of(&w, &view);
            show_all_agents(&w, &mut ui, &mut view);
            match k {
                'S' | 'C' => on_task(&mut ui, "t-001"),
                'u' => on_pane(&mut ui, "w2:p1"),
                _ => on_pane(&mut ui, "w4:p2"),
            }
            press(&mut ui, &mut view, k);
            assert!(matches!(view.mode, panel::Mode::Confirm { .. }), "{k} did not ask");
            assert!(
                matches!(press(&mut ui, &mut view, 'n'), panel::Effect::None),
                "{k} did it anyway after n",
            );
            assert!(matches!(view.mode, panel::Mode::Browse), "{k} left the question up");
            // And `esc` is the same answer, because a question you walk away
            // from is one you have not said yes to.
            press(&mut ui, &mut view, k);
            assert!(
                matches!(panel::apply_key(Key::Esc, &mut ui, &mut view), panel::Effect::None),
                "{k} did it anyway after esc",
            );
            assert!(matches!(view.mode, panel::Mode::Browse));
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

    /// The hands that reach this prompt come from vim, where `esc` is how you
    /// stop typing. It arrived here as a reflex, aimed at a mode the panel does
    /// not have, and threw away titles and notes that then had to be typed
    /// again from memory. So a line with something on it takes two presses.
    #[test]
    fn a_typed_line_is_not_thrown_away_by_one_escape() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_project(&mut ui, "audio");
        panel::apply_key(Key::Char('a'), &mut ui, &mut view);
        for c in "Retune the plate".chars() {
            panel::apply_key(Key::Char(c), &mut ui, &mut view);
        }

        panel::apply_key(Key::Esc, &mut ui, &mut view);
        assert!(matches!(view.mode, panel::Mode::Prompt { .. }), "one esc threw the line away");
        // And the panel says it heard the key. A first press that changed
        // nothing on screen reads as a panel that has stopped listening —
        // which is the reflex pressing itself harder.
        let shown = panel::frame(&ui, &mut view, W, H)[H - 1].text();
        assert!(shown.contains("Retune the plate"), "the line went missing: {shown}");
        assert!(shown.contains("esc again"), "nothing said the cancel was armed: {shown}");

        panel::apply_key(Key::Esc, &mut ui, &mut view);
        assert!(matches!(view.mode, panel::Mode::Browse), "the second esc should cancel");
    }

    /// The two presses have to be next to each other, or the rule is "esc, then
    /// some time later esc" — which is the reflex again with a delay in it.
    #[test]
    fn a_key_between_the_two_escapes_disarms_the_cancel() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);
        on_project(&mut ui, "audio");
        panel::apply_key(Key::Char('a'), &mut ui, &mut view);
        panel::apply_key(Key::Char('x'), &mut ui, &mut view);

        for k in [Key::Char('y'), Key::Backspace, Key::Up] {
            panel::apply_key(Key::Esc, &mut ui, &mut view);
            panel::apply_key(k, &mut ui, &mut view);
            panel::apply_key(Key::Esc, &mut ui, &mut view);
            assert!(
                matches!(view.mode, panel::Mode::Prompt { .. }),
                "{k:?} between the presses should have disarmed the cancel",
            );
            panel::apply_key(Key::Char('x'), &mut ui, &mut view);
        }
    }

    /// Two presses cost nothing when there is nothing to lose, and cost too
    /// much when the way out has to be quick. So an empty line goes on
    /// closing on the first `esc` — "I opened this by mistake" is the other
    /// reason the key gets pressed here — and `ctrl-c`, which nobody types by
    /// reflex, stays the one-key way out of a line that has been typed.
    #[test]
    fn an_empty_line_and_ctrl_c_still_close_on_one_press() {
        let w = world();
        let mut view = panel::View::default();
        let mut ui = ui_of(&w, &view);

        on_project(&mut ui, "audio");
        panel::apply_key(Key::Char('a'), &mut ui, &mut view);
        panel::apply_key(Key::Esc, &mut ui, &mut view);
        assert!(matches!(view.mode, panel::Mode::Browse), "nothing typed, nothing to protect");

        panel::apply_key(Key::Char('a'), &mut ui, &mut view);
        panel::apply_key(Key::Char('x'), &mut ui, &mut view);
        panel::apply_key(Key::Interrupt, &mut ui, &mut view);
        assert!(matches!(view.mode, panel::Mode::Browse), "ctrl-c should cancel outright");
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


    /// Put the cursor on the first row of a kind, wherever it is.
    fn on_row_kind(ui: &mut panel::Ui, want: panel::RowKind) -> usize {
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_kind() == want {
                return i;
            }
        }
        panic!("no row of kind {want:?}");
    }

    /// A raised hand is on screen without anybody having asked for it, and it
    /// stays on screen whatever view is up.
    ///
    /// That is the whole feature. `R` and `w` are filters over *work*, and a
    /// flag is somebody asking for you rather than work waiting — so a section
    /// that went quiet under either would be one you learn to distrust, and the
    /// view you happened to leave the panel in would decide whether an agent
    /// could reach you at all.
    #[test]
    fn a_raised_hand_is_pinned_in_every_view() {
        // Read, so the keys under test reach the tree: a card holds the
        // keyboard, and `R` pressed into one would prove nothing about `R`.
        let f = read_world();
        for keys in [vec![], vec![Key::Char('R')], vec![Key::Char('w')], vec![Key::Char('A')]] {
            let (mut ui, _view) = showing(&f, &keys);
            let docked = ui.dock_for_test();
            let mut flags = 0;
            for i in ui.rows_for_test() - docked..ui.rows_for_test() {
                ui.select_for_test(i);
                if ui.selected_kind() == panel::RowKind::Flag {
                    flags += 1;
                }
            }
            assert_eq!(flags, f.flags.len(), "raised hands went missing under {keys:?}");
        }
    }

    /// The row is the deeplink: it stands for the task it points at, so every
    /// verb already aimed at a task works from it without knowing flags exist.
    ///
    /// And the task's own row in the tree carries the mark, because once you
    /// have read the ask the next question is what the work sits beside — which
    /// is a question only the tree answers.
    #[test]
    fn the_flag_row_is_the_task_and_the_task_row_says_so() {
        let f = read_world();
        let (mut ui, _view) = showing(&f, &[]);

        let at = on_row_kind(&mut ui, panel::RowKind::Flag);
        assert_eq!(
            ui.selected_target(),
            panel::Target::Task("t-105".into()),
            "the newest hand is the first row, and it means its task",
        );
        // Drawn twice on purpose — once here, once up in the tree — which is
        // exactly the shape a pane row already has, and is why the cursor keeps
        // which side of the dock it was on.
        assert_eq!(
            ui.rows_for_target(&panel::Target::Task("t-105".into())).len(),
            2,
            "the tree row and the raised hand are two ways to one task",
        );
        assert!(
            panel::render_row_for_test(&ui, at, W).text().contains("this is next"),
            "the sentence is what the row draws, cut to the pane like any other",
        );

        // The flag with nothing written on it falls back to the title, because
        // "look at this, it exists" is a complete thing for an agent to say.
        ui.select_for_test(at + 1);
        assert!(
            panel::render_row_for_test(&ui, at + 1, W).text().contains("release notes"),
            "a flag with no sentence names the task instead",
        );

        // And the work is findable in place.
        on_task(&mut ui, "t-105");
        assert!(
            panel::render_row_for_test(&ui, ui.selected_index(), W)
                .text()
                .contains(panel::glyph::FLAG),
            "the task's own row carries the mark",
        );
    }

    /// `x` lowers it, from either row, and runs the same CLI the agent raised
    /// it with. Nothing else takes it down — reading the ask must not be what
    /// clears it, or it is gone from every panel before you have decided
    /// anything about it, including the decision to leave it up.
    #[test]
    fn x_lowers_a_raised_hand_and_only_a_raised_hand() {
        let f = read_world();
        let (mut ui, mut view) = showing(&f, &[]);

        on_row_kind(&mut ui, panel::RowKind::Flag);
        match panel::apply_key(Key::Char('x'), &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["flag", "--clear", "t-105"]);
            }
            _ => panic!("x on a raised hand should lower it"),
        }

        // From the task's own row too: you lower it from wherever you were
        // when you read it, not by walking back down to the section.
        on_task(&mut ui, "t-105");
        match panel::apply_key(Key::Char('x'), &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["flag", "--clear", "t-105"]);
            }
            _ => panic!("x on a flagged task should lower it"),
        }

        // Opening it does not. `↵` on a raised hand brings its card back —
        // reading the ask is not answering it, and nothing about the flag
        // changes for having been read a second time.
        on_row_kind(&mut ui, panel::RowKind::Flag);
        assert!(matches!(
            panel::apply_key(Key::Enter, &mut ui, &mut view),
            panel::Effect::None,
        ));
        assert!(matches!(view.mode, panel::Mode::Card(_)), "↵ on a flag opens its card");

        // And a task nobody has flagged is not something `x` can act on: it
        // would otherwise be a key that quietly does nothing on most rows.
        // The card goes away first — `esc` marks it read and leaves it raised,
        // which is the state the section is for.
        assert!(matches!(
            panel::apply_key(Key::Esc, &mut ui, &mut view),
            panel::Effect::Run { .. },
        ));
        on_task(&mut ui, "t-001");
        assert!(matches!(
            panel::apply_key(Key::Char('x'), &mut ui, &mut view),
            panel::Effect::None,
        ));
    }

    /// Three, and a `⋯` for the rest, so a burst of asks cannot push the census
    /// it is pinned above off the pane — and none of them is silently absent,
    /// because the heading counts them all.
    #[test]
    fn the_flag_section_keeps_three_and_counts_them_all() {
        let mut f = read_world();
        for (i, id) in ["t-001", "t-003", "t-020"].iter().enumerate() {
            f.flags.insert(
                (*id).to_string(),
                json!({
                    "said": format!("look at this {i}"),
                    "pane": "w1:p1",
                    "at": "2026-08-16T08:00:00Z",
                    "seen": true,
                }),
            );
        }
        let (mut ui, mut view) = showing(&f, &[]);
        let count = |ui: &mut panel::Ui| {
            let docked = ui.dock_for_test();
            let mut n = 0;
            for i in ui.rows_for_test() - docked..ui.rows_for_test() {
                ui.select_for_test(i);
                if ui.selected_kind() == panel::RowKind::Flag {
                    n += 1;
                }
            }
            n
        };
        assert_eq!(count(&mut ui), 3, "three drawn, whatever is raised");

        let head = panel::render_row_for_test(&ui, ui.rows_for_test() - ui.dock_for_test(), W);
        assert!(
            head.text().contains("flagged") && head.text().contains(&f.flags.len().to_string()),
            "the heading counts every one of them: {}",
            head.text(),
        );

        // `→` on the `⋯` opens the tail in place, exactly as the agents' does.
        on_row_kind(&mut ui, panel::RowKind::More);
        for i in 0..ui.rows_for_test() {
            ui.select_for_test(i);
            if ui.selected_kind() == panel::RowKind::More
                && ui.selected_target() == panel::Target::Overflow("(flagged)".into())
            {
                break;
            }
        }
        if let panel::Effect::Refetch = panel::apply_key(Key::Right, &mut ui, &mut view) {
            panel::refetch_into(&mut ui, &f, &mut view);
        }
        assert_eq!(count(&mut ui), f.flags.len(), "all of them, once asked");
    }

    /// A card comes up because somebody else raised a hand, and it holds the
    /// keyboard while it is up.
    ///
    /// The holding is the point. Every other mode here is entered by a key, so
    /// the person who opened it knows it is open; this one lands in front of
    /// whatever you were reading. A `d` typed a quarter-second before it
    /// arrived must not close a task, and `y` must not reach the tree behind
    /// it.
    #[test]
    fn a_card_arrives_on_its_own_and_holds_the_keyboard() {
        let f = flagged_world();
        let (mut ui, mut view) = showing(&f, &[]);
        let card = match &view.mode {
            panel::Mode::Card(c) => c.clone(),
            _ => panic!("an unread ask should come up on its own"),
        };
        assert_eq!(
            card.task(),
            "t-105",
            "the oldest unread ask, not the newest — a queue is answered in order",
        );

        // Keys the tree would act on do nothing at all while it is up.
        let sel = ui.selected_index();
        for k in [Key::Char('j'), Key::Char('d'), Key::Down, Key::Char('R')] {
            assert!(matches!(panel::apply_key(k, &mut ui, &mut view), panel::Effect::None));
            assert!(matches!(view.mode, panel::Mode::Card(_)), "{k:?} put the card away");
        }
        assert_eq!(ui.selected_index(), sel, "the cursor moved under a card");

        // And a click reads as nothing rather than as the row it is covering.
        assert_eq!(
            panel::click(&mut ui, &mut view, W, H, 4, 6, WORKING_HERE),
            panel::Hit::Nothing,
            "a click went through the card to whatever was underneath",
        );
    }

    /// A card stops growing where a paragraph stops being readable, and is
    /// centred in whatever is left over.
    ///
    /// Everything else on the panel is a row and wants every column it can have.
    /// A card is the one thing here that is prose, and prose set to a hundred
    /// and fifty columns is prose you lose your place in — so the pane getting
    /// bigger moves the card rather than stretching it.
    #[test]
    fn a_card_is_a_paragraph_rather_than_a_banner() {
        let f = flagged_world();
        let edges = |w: usize, h: usize| -> (usize, usize) {
            let (ui, mut view) = showing(&f, &[]);
            let drawn = panel::frame(&ui, &mut view, w, h);
            let top = drawn
                .iter()
                .find(|l| l.text().contains('┌'))
                .unwrap_or_else(|| panic!("no card at {w}x{h}"))
                .text();
            (
                top.chars().position(|c| c == '┌').unwrap(),
                top.chars().position(|c| c == '┐').unwrap(),
            )
        };

        // In a sidebar it is what it has always been: the pane, inset a column
        // either side so the tree shows at both edges.
        let (left, right) = edges(W, H);
        assert_eq!((left, right), (1, W - 2));

        // Zoomed, it is the same card, in the middle.
        let (left, right) = edges(153, 50);
        assert_eq!(right - left, 73, "the box stopped growing");
        assert!(
            left > 35 && 153 - right < left + 4,
            "the card is centred: {left} on the left, {} on the right",
            153 - right,
        );
    }

    /// It waits for the panel to be idle. Somebody else's question landing on a
    /// half-typed title would take the keys meant for the title and answer the
    /// question with them.
    #[test]
    fn a_card_waits_rather_than_interrupting() {
        let f = flagged_world();
        // `a` on the first row opens a prompt. The card is already up at that
        // point, so the flag has to be put away first — which is the sequence
        // this is about: dismissed, then typing, then no second card.
        let (_ui, view) = showing(&f, &[Key::Esc, Key::Char('a'), Key::Char('h')]);
        assert!(matches!(view.mode, panel::Mode::Prompt { .. }), "the prompt was taken over");
    }

    /// Three answers, and the difference between them is what happens to the
    /// hand: `esc` leaves it raised, `x` takes it down, and answering the
    /// question does whatever the question asked for.
    #[test]
    fn the_card_has_three_answers_and_they_do_different_things() {
        let f = flagged_world();

        let (mut ui, mut view) = showing(&f, &[]);
        match panel::apply_key(Key::Esc, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["flag", "t-105", "--seen"], "esc leaves it raised");
            }
            _ => panic!("esc should mark it read"),
        }
        assert!(matches!(view.mode, panel::Mode::Browse), "and puts the card away");

        let (mut ui, mut view) = showing(&f, &[]);
        match panel::apply_key(Key::Char('x'), &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => {
                assert_eq!(argv, vec!["flag", "--clear", "t-105"], "x takes it down");
            }
            _ => panic!("x should lower it"),
        }

        // `↵` is not an answer to a card that asked for something. A return
        // pressed out of habit must never hand a task over.
        let (mut ui, mut view) = showing(&f, &[]);
        assert!(matches!(
            panel::apply_key(Key::Enter, &mut ui, &mut view),
            panel::Effect::None,
        ));
        assert!(matches!(view.mode, panel::Mode::Card(_)), "↵ answered a question it was asked");

        // …and it is the answer to a card that only wanted to be looked at.
        let mut only_looking = flagged_world();
        only_looking.flags.remove("t-004");
        if let Some(f) = only_looking.flags.get_mut("t-105").and_then(|f| f.as_object_mut()) {
            f.insert("ask".into(), json!(""));
        }
        let (mut ui, mut view) = showing(&only_looking, &[]);
        match panel::apply_key(Key::Enter, &mut ui, &mut view) {
            panel::Effect::Run { argv, .. } => assert_eq!(argv, vec!["flag", "t-105", "--seen"]),
            _ => panic!("↵ should put away a card with nothing to answer"),
        }
    }

    /// `y` runs the claim the agent asked for, into the pane that asked, and
    /// tells it so. The same argv `c` builds from the other direction — the
    /// agent picked the task and you picked the agent — because a claim is
    /// thirty lines of guards that must have one implementation.
    ///
    /// Nothing lowers the flag here and nothing needs to: `claim` lowers the
    /// flags on the task it claims, so a failure between two commands cannot
    /// leave the ask up over work already handed over.
    #[test]
    fn y_hands_the_task_to_the_agent_that_asked() {
        let f = flagged_world();
        let (mut ui, mut view) = showing(&f, &[]);
        match panel::apply_key(Key::Char('y'), &mut ui, &mut view) {
            panel::Effect::Run { argv, then, .. } => {
                assert_eq!(argv, vec!["claim", "t-105", "--pane", "w4:p2"]);
                assert!(then.is_some(), "the agent that asked is told it can have it");
            }
            _ => panic!("y should hand it over"),
        }

        // And `n` says so out loud. An agent that asked and heard nothing is an
        // agent still waiting on an answer that never came.
        let (mut ui, mut view) = showing(&f, &[]);
        match panel::apply_key(Key::Char('n'), &mut ui, &mut view) {
            panel::Effect::Run { argv, then, .. } => {
                assert_eq!(argv, vec!["flag", "--clear", "t-105"]);
                assert!(then.is_some(), "a refusal is worth typing back");
            }
            _ => panic!("n should refuse it"),
        }
    }

    /// The card is asked once. The flag is marked read by a command, and the
    /// rows in hand were built before it ran — so a panel that popped whatever
    /// was unread on every frame would put the same card back up on the next
    /// one, for ever, with `esc` doing nothing you could see.
    #[test]
    fn a_card_that_was_put_away_stays_away() {
        let f = flagged_world();
        let (ui, mut view) = showing(&f, &[Key::Esc]);
        assert!(matches!(view.mode, panel::Mode::Browse));
        // The snapshot has not changed — the CLI that would mark it read never
        // ran here — so this is the exact state the live panel is in between
        // the key and the rebuild.
        panel::place(&ui, &mut view, W, H);
        assert!(matches!(view.mode, panel::Mode::Browse), "the card came back on the next frame");
    }

    /// It never covers the section at the foot. Answering one ask by hiding the
    /// queue of the others is the one thing a popup here must not do — and the
    /// dock is also where the agents are, which is what you look at to decide
    /// whether to hand anything over at all.
    #[test]
    fn a_card_leaves_the_foot_of_the_panel_alone() {
        let f = flagged_world();
        let (ui, mut view) = showing(&f, &[]);
        assert!(matches!(view.mode, panel::Mode::Card(_)));
        let with = panel::frame(&ui, &mut view, W, H);

        let (ui, mut view) = showing(&read_world(), &[]);
        let without = panel::frame(&ui, &mut view, W, H);

        let tail = |f: &[panel::Line]| -> Vec<String> {
            f.iter().rev().take(12).map(|l| l.text()).collect()
        };
        // The footer's own line differs — it says what is raised while a card
        // is up — so the comparison starts above it.
        assert_eq!(
            tail(&with)[1..],
            tail(&without)[1..],
            "the card covered the dock",
        );
    }

    /// Every panel draws the same card, and exactly one of them is answered.
    ///
    /// This is the half that was missing. A card comes up in all twenty-two
    /// panels at once — they read one file and they all drew it — and `esc`,
    /// `x` or `y` happens in whichever one you are looking at. Nothing told the
    /// others, so they went on holding a question that was already settled; and
    /// since a card holds the keyboard, switching workspaces meant arriving at
    /// a modal ask about work already in somebody's hands.
    ///
    /// The card is derived from the record now, not remembered, so the panels
    /// agree the same way the folds and the cursor do one file down.
    #[test]
    fn a_card_answered_on_one_panel_goes_from_the_others() {
        let f = flagged_world();
        // Two panels, both showing it — which is what actually happens.
        let (here, mut mine) = showing(&f, &[]);
        let (there, mut theirs) = showing(&f, &[]);
        assert!(matches!(mine.mode, panel::Mode::Card(_)));
        assert!(matches!(theirs.mode, panel::Mode::Card(_)));

        // Answered here: `esc` marks it read, which is a write to the file both
        // panels are reading.
        let mut ui = here;
        assert!(matches!(
            panel::apply_key(Key::Esc, &mut ui, &mut mine),
            panel::Effect::Run { .. },
        ));
        let mut answered = flagged_world();
        if let Some(r) = answered.flags.get_mut("t-105").and_then(|r| r.as_object_mut()) {
            r.insert("seen".into(), json!(true));
        }

        // The other panel rebuilds on its own tick and puts it away, with
        // nobody having touched that pane.
        let mut ui = there;
        panel::refetch_into(&mut ui, &answered, &mut theirs);
        panel::place(&ui, &mut theirs, W, H);
        assert!(
            matches!(theirs.mode, panel::Mode::Browse),
            "a panel nobody answered kept the card",
        );
    }

    /// The same when the hand comes down altogether — `x` here, or `wsp flag
    /// --clear` at any shell on the machine. A card is a question, and a
    /// question that has been withdrawn must not still be answerable: `y` on a
    /// stale one would claim a task for an agent that stopped asking.
    #[test]
    fn a_lowered_flag_takes_its_card_with_it() {
        let f = flagged_world();
        let (mut ui, mut view) = showing(&f, &[]);
        assert!(matches!(view.mode, panel::Mode::Card(_)));

        let mut lowered = flagged_world();
        lowered.flags.remove("t-105");
        panel::refetch_into(&mut ui, &lowered, &mut view);
        panel::place(&ui, &mut view, W, H);
        assert!(matches!(view.mode, panel::Mode::Browse), "the card outlived the flag");
    }

    /// An agent that raises its hand again with more to say replaces the card
    /// where it stands.
    ///
    /// In place, rather than closed and re-opened: closing it would count as
    /// this panel having asked, and the once-only guard would then refuse to
    /// put the new words up at all — so the panel would be showing the old
    /// sentence with no way to reach the new one.
    #[test]
    fn a_second_thought_replaces_the_card_rather_than_closing_it() {
        let f = flagged_world();
        let (mut ui, mut view) = showing(&f, &[]);

        let mut again = flagged_world();
        if let Some(r) = again.flags.get_mut("t-105").and_then(|r| r.as_object_mut()) {
            r.insert("said".into(), json!("actually the reducer is free now"));
        }
        panel::refetch_into(&mut ui, &again, &mut view);
        panel::place(&ui, &mut view, W, H);
        match &view.mode {
            panel::Mode::Card(c) => assert_eq!(c.said(), "actually the reducer is free now"),
            _ => panic!("the card closed instead of taking the new words"),
        }
    }

    /// Answering one and finding the next underneath it is the point of a
    /// queue. The footer says how many there are, so it is never a surprise.
    #[test]
    fn the_next_ask_comes_up_behind_the_one_answered() {
        // A second unread ask, newer than the first — so it is the one that
        // comes up *after*, not instead.
        let queued = || {
            let mut s = flagged_world();
            s.flags.insert(
                "t-006".to_string(),
                json!({
                    "said": "and this one is still parked",
                    "pane": "w2:p1",
                    "at": "2026-08-16T09:55:00Z",
                }),
            );
            s
        };
        let f = queued();
        let (mut ui, mut view) = showing(&f, &[]);
        match &view.mode {
            panel::Mode::Card(c) => assert_eq!(c.task(), "t-105", "the oldest unread is first"),
            _ => panic!("nothing came up"),
        }

        let mut answered = queued();
        if let Some(r) = answered.flags.get_mut("t-105").and_then(|r| r.as_object_mut()) {
            r.insert("seen".into(), json!(true));
        }
        panel::apply_key(Key::Esc, &mut ui, &mut view);
        panel::refetch_into(&mut ui, &answered, &mut view);
        panel::place(&ui, &mut view, W, H);
        match &view.mode {
            panel::Mode::Card(c) => assert_eq!(c.task(), "t-006", "the queue stopped after one"),
            _ => panic!("the next ask never came up"),
        }
    }

    /// The board's scenes are the proof that its seam is real: kanban::Ctx has
    /// existed since the board landed and nothing outside a live herdr had ever
    /// built one. A frame that comes out empty, or at the wrong size, or with
    /// the cursor pointing at a card that is not there, is a seam that only
    /// looks like one.
    #[test]
    fn the_board_draws_from_a_fixture_with_nothing_running() {
        let w = world();
        let scenes = board_scenes(&w);
        assert!(!scenes.is_empty());

        // The frames are html; what a reader sees is the text between the
        // tags, and a column heading split across two spans is still one word
        // on the screen.
        let text = |html: &str| {
            let mut out = String::new();
            let mut inside = false;
            for c in html.chars() {
                match c {
                    '<' => inside = true,
                    '>' => inside = false,
                    _ if !inside => out.push(c),
                    _ => {}
                }
            }
            out
        };

        for s in &scenes {
            assert!(text(&s.html).contains("todo"), "{}: no columns\n{}", s.title, s.html);
            // Every scene names what a verb would act on, and "nothing" is only
            // right for the one scene that is about an empty column.
            let empty = s.target == "nothing";
            assert_eq!(
                empty,
                s.title.contains("nothing under it"),
                "{}: cursor is on {}",
                s.title,
                s.target
            );
        }

        // The done column is off unless a scene asks for it, and one does —
        // otherwise the key that brings it back is drawn nowhere.
        let done: Vec<&Scene> = scenes.iter().filter(|s| text(&s.html).contains("done 1")).collect();
        assert_eq!(done.len(), 1, "exactly one scene shows finished work");
    }

}

