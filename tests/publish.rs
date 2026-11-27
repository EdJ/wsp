//! `wsp publish <row|list> --to shared:<name>` (wsp-185): a private row or list
//! goes into a shared workspace under ids the hub hands out. Refused while
//! logged out (naming `wsp login`) and for a record already shared; the happy
//! path runs against a real hub run as an external program
//! (`wsp_spoke::testhub`: `WSP_HUB_BIN` or `WSP_HUB_IMAGE`), the caller a member
//! of `shared:core`.

use std::path::PathBuf;
use std::process::{Command, Output};

use wsp_spoke::testhub::{Setup, TestHub};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

struct Rig {
    dir: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("wsp-publish-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let rig = Rig { dir };
        assert!(rig.run(&["init"], &[]).status.success());
        assert!(rig.run(&["project", "add", "pp"], &[]).status.success());
        rig
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(WSP);
        c.args(args)
            .env("HOME", self.dir.join("home"))
            .env("WSP_HOME", self.dir.join("store"))
            .env("WSP_STATE", self.dir.join("state"))
            .env_remove("WSP_HUB_TOKEN")
            .env_remove("WSP_HUB_URL")
            .env_remove("WSP_HUB_CA")
            .env_remove("WSP_WORKSPACE")
            .env_remove("XDG_CONFIG_HOME");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    /// Logged in to `hub` as `ann`, a member of `shared:core`.
    fn login_to(&self, hub: &TestHub) {
        let token = hub.seed_members(&["core"], &["ann"])["ann"].clone();
        hub.login(&self.dir.join("home"), "ann", &token);
    }

    fn file(&self, dir: &str, id: &str) -> String {
        std::fs::read_to_string(self.dir.join("store").join(dir).join(format!("{id}.md"))).unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn said(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|l| l == line)
}

#[test]
fn publishing_while_logged_out_is_refused_naming_login_and_changes_nothing() {
    let rig = Rig::new("out");
    assert!(rig.run(&["add", "one", "-p", "pp"], &[]).status.success());
    let before = rig.file("tasks", "pp-001");
    let o = rig.run(&["publish", "pp-001", "--to", "shared:core"], &[]);
    assert!(!o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("wsp login"), "{}", said(&o));
    assert_eq!(rig.file("tasks", "pp-001"), before);
}

#[test]
fn publishing_a_shared_row_is_refused() {
    let rig = Rig::new("shared");
    let Some(hub) = TestHub::start(Setup::default()) else { return };
    rig.login_to(&hub);
    let o = rig.run(&["add", "one", "-p", "pp", "--workspace", "shared:core"], &[]);
    assert!(o.status.success(), "{}", said(&o));
    let o = rig.run(&["publish", "pp-001", "--to", "shared:other"], &[]);
    assert!(!o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("already in shared:core"), "{}", said(&o));
    assert!(has_line(&rig.file("tasks", "pp-001"), "workspace: shared:core"));
}

#[test]
fn a_row_and_its_subtask_take_hub_ids_and_the_old_ones_still_resolve() {
    let rig = Rig::new("happy");
    let Some(hub) = TestHub::start(Setup::default()) else { return };
    rig.login_to(&hub);
    assert!(rig.run(&["add", "the row", "-p", "pp"], &[]).status.success());
    assert!(rig.run(&["add", "the sub", "--parent", "pp-001"], &[]).status.success());
    assert!(rig.run(&["add", "other", "-p", "pp"], &[]).status.success());

    let o = rig.run(&["publish", "pp-001", "--to", "shared:core"], &[]);
    assert!(o.status.success(), "{}", said(&o));

    // Above the highest local number (3), parent before child.
    let row = rig.file("tasks", "pp-004");
    let sub = rig.file("tasks", "pp-005");
    assert!(has_line(&row, "workspace: shared:core"), "{row}");
    assert!(has_line(&sub, "workspace: shared:core"), "{sub}");
    assert!(has_line(&sub, "parent: pp-004"), "{sub}");
    assert!(!rig.dir.join("store/tasks/pp-001.md").exists());
    // The row left alone stays private.
    assert!(!rig.file("tasks", "pp-003").contains("workspace:"));

    let shown = rig.run(&["show", "pp-001"], &[]);
    assert!(shown.status.success(), "{}", said(&shown));
    assert!(said(&shown).contains("pp-004"), "{}", said(&shown));

    // Nothing is pushed by publish: the hub holds no object.
    assert_eq!(hub.data_files().iter().filter(|(p, _)| p.starts_with("objects")).count(), 0);

    // And a second publish of the same row is refused as shared.
    let again = rig.run(&["publish", "pp-004", "--to", "shared:core"], &[]);
    assert!(!again.status.success());
}

#[test]
fn a_list_goes_with_its_members() {
    let rig = Rig::new("list");
    let Some(hub) = TestHub::start(Setup::default()) else { return };
    rig.login_to(&hub);
    assert!(rig.run(&["add", "member", "-p", "pp"], &[]).status.success());
    let o = rig.run(&["worklist", "new", "wl", "a list"], &[]);
    assert!(o.status.success(), "{}", said(&o));
    let o = rig.run(&["worklist", "add", "wl", "pp-001"], &[]);
    assert!(o.status.success(), "{}", said(&o));

    let o = rig.run(&["publish", "wl", "--to", "shared:core"], &[]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(has_line(&rig.file("worklists", "wl"), "workspace: shared:core"));
    assert!(has_line(&rig.file("tasks", "pp-002"), "workspace: shared:core"));
    assert!(rig.file("worklists", "wl").contains("pp-002"), "the list names the member by its new id");
}
