//! `.cargo/cc-wrap`'s four routes (docs/state-core.md section 9, option 1), run with a fake `sccache`
//! and a fake `cc` on a PATH of their own. Each fake appends its name and arguments to a log and exits
//! with the code its env var gives.
//!
//! The wrapper also tries `/usr/local/bin/sccache` and `/opt/homebrew/bin/sccache`, so on a machine
//! that has a real one there the "sccache missing" route cannot be reproduced; that test skips then.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Bin {
    dir: PathBuf,
}

impl Bin {
    fn new(tag: &str) -> Bin {
        let dir = std::env::temp_dir().join(format!("wsp-cc-wrap-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Bin { dir }
    }
    /// A fake `name` that logs `name args...` and exits with `$<NAME>_EXIT` (default 0).
    fn fake(&self, name: &str) {
        let var = format!("FAKE_{}_EXIT", name.to_uppercase());
        let body = format!(
            "#!/bin/sh\necho \"{name} $*\" >> \"$FAKE_LOG\"\nexit \"${{{var}:-0}}\"\n"
        );
        let p = self.dir.join(name);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("log")).unwrap_or_default()
    }
    /// Run the wrapper with `args`; returns (exit code, log lines).
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, Vec<String>) {
        let wrap = root().join(".cargo/cc-wrap");
        assert!(wrap.exists(), "{} is missing", wrap.display());
        let mut c = Command::new("/bin/sh");
        // PATH: the fakes first, then only the directories `/bin/sh` itself needs.
        c.arg(&wrap).args(args).env("PATH", format!("{}:/usr/bin:/bin", self.dir.display())).env("FAKE_LOG", self.dir.join("log"));
        c.env_remove("COMPOUND_NO_SCCACHE");
        for (k, v) in env {
            c.env(k, v);
        }
        let code = c.status().unwrap().code().unwrap();
        (code, self.log().lines().map(String::from).collect())
    }
}

impl Drop for Bin {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn real_sccache_installed() -> bool {
    ["/usr/local/bin/sccache", "/opt/homebrew/bin/sccache"].iter().any(|p| Path::new(p).exists())
}

#[test]
fn sccache_found_runs_sccache_cc_with_the_args() {
    let b = Bin::new("found");
    b.fake("sccache");
    b.fake("cc");
    let (code, log) = b.run(&["-c", "sqlite3.c", "-o", "sqlite3.o"], &[]);
    assert_eq!(code, 0);
    assert_eq!(log, ["sccache cc -c sqlite3.c -o sqlite3.o"]);
}

#[test]
fn sccache_missing_runs_cc_with_the_args() {
    if real_sccache_installed() {
        eprintln!("skipped: a real sccache is installed where the wrapper looks");
        return;
    }
    let b = Bin::new("missing");
    b.fake("cc");
    let (code, log) = b.run(&["-c", "a.c"], &[]);
    assert_eq!(code, 0);
    assert_eq!(log, ["cc -c a.c"]);
}

#[test]
fn sccache_exiting_2_falls_back_to_cc_once() {
    let b = Bin::new("two");
    b.fake("sccache");
    b.fake("cc");
    let (code, log) = b.run(&["-c", "a.c"], &[("FAKE_SCCACHE_EXIT", "2")]);
    assert_eq!(code, 0, "the fallback's own exit code is the wrapper's");
    assert_eq!(log, ["sccache cc -c a.c", "cc -c a.c"], "sccache once, then cc once");
}

#[test]
fn no_sccache_env_runs_cc_and_never_sccache() {
    let b = Bin::new("off");
    b.fake("sccache");
    b.fake("cc");
    let (code, log) = b.run(&["-c", "a.c"], &[("COMPOUND_NO_SCCACHE", "1")]);
    assert_eq!(code, 0);
    assert_eq!(log, ["cc -c a.c"]);
}

#[test]
fn exit_codes_pass_through_on_every_route() {
    // sccache's own failure (not 2) is the compile's failure: no second try.
    let b = Bin::new("rc-sccache");
    b.fake("sccache");
    b.fake("cc");
    let (code, log) = b.run(&["a.c"], &[("FAKE_SCCACHE_EXIT", "1")]);
    assert_eq!((code, log), (1, vec!["sccache cc a.c".to_string()]));

    // the fallback's failure after sccache's 2.
    let (code, log) = b.run(&["a.c"], &[("FAKE_SCCACHE_EXIT", "2"), ("FAKE_CC_EXIT", "7")]);
    assert_eq!(code, 7);
    assert_eq!(log.len(), 3, "the earlier run's line, then sccache and cc: {log:?}");

    // bypassed: cc's code.
    let b = Bin::new("rc-cc");
    b.fake("cc");
    let (code, _) = b.run(&["a.c"], &[("COMPOUND_NO_SCCACHE", "1"), ("FAKE_CC_EXIT", "3")]);
    assert_eq!(code, 3);
}
