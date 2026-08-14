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
use crate::panel::{self, Key, Snapshot};

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

fn agent(pane: &str, ws: &str, state: &str, title: &str) -> herdr::Agent {
    herdr::Agent {
        pane_id: pane.to_string(),
        workspace_id: ws.to_string(),
        agent_status: state.to_string(),
        title: title.to_string(),
        ..Default::default()
    }
}

/// A small but complete world: nested projects, every task status, an agent
/// bound to a task, an idle agent on a `doing` task (the "needs you" case), an
/// unclaimed agent, a project past the task cap, and two inbox items.
fn world() -> Snapshot {
    let projects = vec![
        project("audio", None),
        project("vst", Some("audio")),
        project("trance", Some("vst")),
        project("verb", Some("vst")),
        project("meta", None),
        project("tooling", Some("meta")),
        project("wsp", Some("tooling")),
    ];

    let mut tasks = vec![
        task("t-001", "Apply reverb fixes from the tuning table", Some("trance"), "doing"),
        task("t-002", "Plan demo video strategy", Some("trance"), "doing"),
        // Doing, with an idle agent on it — the one combination that raises the
        // "needs you" arrow, because the work is live and the process is not.
        task("t-003", "Retune the early reflections", Some("verb"), "doing"),
        task("t-004", "Ship the release notes", Some("verb"), "review"),
        task("t-006", "Waiting on the tuning table decision", Some("verb"), "blocked"),
        task("t-005", "Design workspace management system", Some("tooling"), "doing"),
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
        workspace("w0", "wsp", true),
        workspace("w1", "Trance Video", false),
        workspace("w2", "Verb UI", false),
        workspace("w3", "Easter", false),
    ];

    let agents = vec![
        agent("w1:p1", "w1", "working", "Trance Video"),
        agent("w2:p1", "w2", "idle", "Verb UI"),
        agent("w3:p1", "w3", "working", "◐ Unclaimed explorer"),
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
        agents,
    }
}

/// The same world with nothing running — what the panel looks like before any
/// agent exists, which is most people's first sight of it.
fn quiet_world() -> Snapshot {
    let mut s = world();
    s.agents.clear();
    s.bindings.clear();
    s
}

// ---- scenes -------------------------------------------------------------

struct Scene {
    title: String,
    caption: String,
    /// The keys that produced this frame, as the reader would press them.
    gesture: String,
    html: String,
}

fn key_name(k: Key) -> String {
    match k {
        Key::Up => "↑".into(),
        Key::Down => "↓".into(),
        Key::Left => "←".into(),
        Key::Right => "→".into(),
        Key::Enter => "↵".into(),
        Key::Digit(d) => d.to_string(),
        Key::Char(c) => c.to_string(),
        Key::Quit => "q".into(),
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
            panel::refetch_into(&mut self.ui, self.snap, &self.view, Some("w0"));
        }
        self
    }

    fn keys(&mut self, ks: &[Key]) -> &mut Self {
        for k in ks {
            self.key(*k);
        }
        self
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
            html: panel::to_html(&panel::frame(&self.ui, W, H), W),
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
        "Three agents. One working a task (●), one idle on a task that is still doing — which raises the ← asking for you — and one unclaimed at the foot.",
    ));

    out.push(
        Driver::new(&w)
            .keys(&[Key::Down, Key::Down, Key::Down])
            .scene("Moving down", "j × 3. Section headings are skipped; the cursor only rests on rows that lead somewhere."),
    );

    out.push(
        Driver::new(&w)
            .keys(&[Key::Down, Key::Left])
            .scene("Folded", "← on a project hides its tasks and every project beneath it."),
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
            .key(Key::Char('A'))
            .scene("Showing done", "A brings finished work back, and with it any project holding nothing else."),
    );

    out.push(
        Driver::new(&w)
            .key(Key::Char('?'))
            .scene("Help", "? puts the key map in the message line for four seconds."),
    );

    out
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
    out.push_str("<div class=\"scenes\">\n");
    for s in scenes {
        out.push_str(&format!(
            "<section class=\"scene\">\
             <div class=\"rail\"><h2>{}</h2><span class=\"gesture\">{}</span><p>{}</p></div>\
             <div class=\"frame\">{}</div>\
             </section>\n",
            s.title, s.gesture, s.caption, s.html
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
