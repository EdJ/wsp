//! New records land in the caller's workspace (wsp-172): private unless
//! `--workspace shared:<name>` or `$WSP_WORKSPACE` says otherwise; shared needs
//! a login; a child takes its parent's workspace; no edit verb moves a record.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

struct Rig {
    dir: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("wsp-record-workspace-{name}-{}", std::process::id()));
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
            .env_remove("WSP_WORKSPACE")
            .env_remove("XDG_CONFIG_HOME");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn login(&self) {
        let conf = self.dir.join("home/.config/wsp");
        std::fs::create_dir_all(&conf).unwrap();
        let file = conf.join("hubs.json");
        std::fs::write(&file, r#"{"url": "tcp://127.0.0.1:1", "token": "t"}"#).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
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
fn add_with_no_flag_writes_no_workspace_line() {
    let rig = Rig::new("private");
    assert!(rig.run(&["add", "one", "-p", "pp"], &[]).status.success());
    assert!(!rig.file("tasks", "pp-001").contains("workspace:"));
    let o = rig.run(&["worklist", "new", "wl", "a list"], &[]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(!rig.file("worklists", "wl").contains("workspace:"));
    assert!(!rig.file("projects", "pp").contains("workspace:"));
}

#[test]
fn shared_while_logged_out_is_refused_naming_login_and_writes_nothing() {
    let rig = Rig::new("out");
    for (args, env) in [
        (vec!["add", "one", "-p", "pp", "--workspace", "shared:core"], vec![]),
        (vec!["add", "one", "-p", "pp"], vec![("WSP_WORKSPACE", "shared:core")]),
        (vec!["worklist", "new", "wl", "a list", "--workspace", "shared:core"], vec![]),
        (vec!["project", "add", "q", "--workspace", "shared:core"], vec![]),
    ] {
        let o = rig.run(&args, &env);
        assert!(!o.status.success(), "{args:?}");
        assert!(said(&o).contains("wsp login"), "{}", said(&o));
    }
    assert!(!rig.dir.join("store/tasks/pp-001.md").exists());
    assert!(!rig.dir.join("store/worklists/wl.md").exists());
    assert!(!rig.dir.join("store/projects/q.md").exists());
}

#[test]
fn logged_in_a_record_is_shared_and_its_children_follow_it() {
    let rig = Rig::new("in");
    rig.login();
    let o = rig.run(&["add", "parent", "-p", "pp", "--workspace", "shared:core"], &[]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(has_line(&rig.file("tasks", "pp-001"), "workspace: shared:core"));

    // The flag does not move a child: it is where its parent is.
    let o = rig.run(&["add", "child", "--parent", "pp-001", "--workspace", "private"], &[("WSP_WORKSPACE", "private")]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(has_line(&rig.file("tasks", "pp-002"), "workspace: shared:core"));
    let o = rig.run(&["add", "child", "--parent", "pp-001"], &[]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(has_line(&rig.file("tasks", "pp-003"), "workspace: shared:core"));

    // The environment is the default and the flag beats it.
    assert!(rig.run(&["worklist", "new", "wl", "a list"], &[("WSP_WORKSPACE", "shared:core")]).status.success());
    assert!(has_line(&rig.file("worklists", "wl"), "workspace: shared:core"));
    assert!(rig.run(&["add", "own", "-p", "pp", "--workspace", "private"], &[("WSP_WORKSPACE", "shared:core")]).status.success());
    assert!(!rig.file("tasks", "pp-004").contains("workspace:"));

    // A child of a private row stays private, whatever the flag says.
    let o = rig.run(&["add", "kid", "--parent", "pp-004", "--workspace", "shared:core"], &[]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(!rig.file("tasks", "pp-005").contains("workspace:"));

    let o = rig.run(&["project", "add", "sub", "--parent", "pp", "--workspace", "shared:core"], &[]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(!rig.file("projects", "sub").contains("workspace:"), "a child project is where its parent is");
}

#[test]
fn no_edit_verb_moves_a_record_by_flag() {
    let rig = Rig::new("edit");
    rig.login();
    assert!(rig.run(&["add", "one", "-p", "pp"], &[]).status.success());
    for verb in [["edit", "pp-001"], ["mv", "pp-001"], ["project", "edit"], ["worklist", "edit"]] {
        let mut line = verb.to_vec();
        if line[0] == "project" || line[0] == "worklist" {
            line.push("pp");
        }
        line.extend(["--workspace", "shared:core"]);
        let o = rig.run(&line, &[]);
        assert!(!o.status.success(), "{line:?}: {}", said(&o));
    }
    assert!(!rig.file("tasks", "pp-001").contains("workspace:"));
}
