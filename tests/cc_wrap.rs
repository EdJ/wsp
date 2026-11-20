//! `.cargo/cc-wrap`'s four routes (docs/state-core.md section 9, option 1), run with a fake `sccache`
//! and a fake `cc` on a PATH of their own. Each fake appends its name and arguments to a log and exits
//! with the code its env var gives.
//!
//! Both wrappers run sccache under `.cargo/sccache-bound` (wsp-083): a fake that hangs on the compile
//! and on `--show-stats` stands for a server that is alive and never answers, one that sleeps and
//! then succeeds for a compile that is merely slow. Each run gets its own `TMPDIR`, where the
//! bound writes down a wedged server.
//!
//! What runs when, under load: the bound forks a `--show-stats` probe whenever the call outlasts
//! `COMPOUND_SCCACHE_WAIT`, and a fake's `echo` lands in the log when the scheduler gets to it, so
//! the call's line and the probe's can come in either order (wsp-700). A test that is not about the
//! probe runs with a wait no scheduler delay reaches, so no probe forks. A hung fake is killed on
//! the bound's clock, maybe before its `echo` has run, so a hung test asks of the call's and the
//! probe's lines only that neither comes twice, and of the order only what the wrapper sets: the
//! fallback runs after the bound has given up. That the probe ran and got no answer is read off
//! stderr, which the bound writes itself.
//!
//! The wrapper also tries `/usr/local/bin/sccache` and `/opt/homebrew/bin/sccache`, so on a machine
//! that has a real one there the "sccache missing" route cannot be reproduced; that test skips then.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

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
        self.script(name, &body);
    }
    /// A fake `name` with `body` as its whole script.
    fn script(&self, name: &str, body: &str) {
        let p = self.dir.join(name);
        wsp_testkit::script(&p, body);
    }
    /// A fake `sccache` whose server is alive and never answers: it logs, then sleeps past any
    /// bound, on a compile and on `--show-stats` alike. `exec sleep` so a kill or an alarm ends it.
    fn hung_sccache(&self) {
        self.script("sccache", &format!("#!/bin/sh\necho \"sccache $*\" >> \"$FAKE_LOG\"\nexec sleep {HUNG_SLEEP}\n"));
    }
    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("log")).unwrap_or_default()
    }
    /// Run `.cargo/cc-wrap` with `args`; returns (exit code, log lines).
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, Vec<String>) {
        let (code, log, _) = self.run_wrap("cc-wrap", args, env);
        (code, log)
    }
    /// Run `.cargo/<wrap>` with `args`; returns (exit code, log lines, stderr). The bound's wait is
    /// 600 s unless `env` sets its own, so a call the scheduler is slow to start is never probed.
    fn run_wrap(&self, wrap: &str, args: &[&str], env: &[(&str, &str)]) -> (i32, Vec<String>, String) {
        let wrap = root().join(".cargo").join(wrap);
        assert!(wrap.exists(), "{} is missing", wrap.display());
        let mut c = Command::new("/bin/sh");
        // PATH: the fakes first, then only the directories `/bin/sh` itself needs.
        c.arg(&wrap).args(args).env("PATH", format!("{}:/usr/bin:/bin", self.dir.display())).env("FAKE_LOG", self.dir.join("log"));
        c.env_remove("COMPOUND_NO_SCCACHE").env("TMPDIR", &self.dir).env("COMPOUND_SCCACHE_WAIT", "600");
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        let code = out.status.code().unwrap();
        (code, self.log().lines().map(String::from).collect(), String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

impl Drop for Bin {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// How long a hung fake sleeps: a run that ends well inside it was ended by the bound.
const HUNG_SLEEP: u64 = 600;

/// A hung run's log: `fallback` once and last, and before it only `maybe` lines, each at most once
/// (a fake killed before its `echo` ran leaves none).
fn assert_hung_log(log: &[String], maybe: &[&str], fallback: &str) {
    let (last, rest) = log.split_last().unwrap_or_else(|| panic!("no fallback: {log:?}"));
    assert_eq!(last, fallback, "the fallback comes last: {log:?}");
    for l in rest {
        assert!(maybe.contains(&l.as_str()), "only the call and the probe come before it: {log:?}");
    }
    for m in maybe {
        assert!(rest.iter().filter(|l| l == m).count() <= 1, "{m:?} at most once: {log:?}");
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

/// Bounds for a hung fake: 3 s before the probe and 3 s for it; it costs about 7 s.
const HUNG: [(&str, &str); 2] = [("COMPOUND_SCCACHE_WAIT", "3"), ("COMPOUND_SCCACHE_PROBE", "3")];

#[test]
fn a_hung_sccache_on_cc_wraps_route_falls_back_to_cc_and_says_so_once() {
    let b = Bin::new("hung-cc");
    b.hung_sccache();
    b.fake("cc");
    let t = Instant::now();
    let (code, first, err) = b.run_wrap("cc-wrap", &["-c", "a.c"], &HUNG);
    assert!(t.elapsed() < Duration::from_secs(HUNG_SLEEP / 2), "bounded: {:?}", t.elapsed());
    assert_eq!(code, 0, "the fallback's exit code: {err}");
    assert_hung_log(&first, &["sccache cc -c a.c", "sccache --show-stats"], "cc -c a.c");
    assert_eq!(err.matches("did not answer").count(), 1, "said once: {err}");

    // The next compile in the same build does not wait on the wedged server again, nor say so again:
    // sccache is never started, so there is nothing to wait on.
    let (code, log, err) = b.run_wrap("cc-wrap", &["-c", "b.c"], &HUNG);
    assert_eq!(code, 0);
    assert_eq!(&log[first.len()..], ["cc -c b.c"], "sccache skipped: {log:?}");
    assert!(!err.contains("did not answer"), "{err}");
}

#[test]
fn a_hung_sccache_on_rustc_wraps_route_falls_back_to_rustc() {
    let b = Bin::new("hung-rustc");
    b.hung_sccache();
    b.fake("rustc");
    let rustc = b.dir.join("rustc");
    let t = Instant::now();
    let (code, log, err) = b.run_wrap("rustc-wrap", &[rustc.to_str().unwrap(), "-vV"], &HUNG);
    assert!(t.elapsed() < Duration::from_secs(HUNG_SLEEP / 2), "bounded: {:?}", t.elapsed());
    assert_eq!(code, 0, "{err}");
    let remap = root().join(".cargo/rustc-remap");
    let remap = remap.to_str().unwrap();
    let call = format!("sccache {remap} -vV");
    assert_hung_log(&log, &[call.as_str(), "sccache --show-stats"], "rustc -vV");
    assert_eq!(err.matches("did not answer").count(), 1, "{err}");
}

#[test]
fn a_slow_compile_with_an_answering_server_runs_once_and_is_not_cut_short() {
    let b = Bin::new("slow");
    // The compile outlasts the 1 s wait three times over; the server answers `--show-stats` at once,
    // and the probe has 30 s to do it in, so a loaded machine's slow start is not read as a wedge.
    b.script(
        "sccache",
        "#!/bin/sh\necho \"sccache $*\" >> \"$FAKE_LOG\"\n\
         [ \"$1\" = --show-stats ] && exit 0\nsleep 3\necho done >> \"$FAKE_LOG\"\nexit 0\n",
    );
    b.fake("cc");
    let t = Instant::now();
    let (code, log, err) =
        b.run_wrap("cc-wrap", &["-c", "sqlite3.c"], &[("COMPOUND_SCCACHE_WAIT", "1"), ("COMPOUND_SCCACHE_PROBE", "30")]);
    assert!(t.elapsed() >= Duration::from_secs(3), "waited for the compile: {:?}", t.elapsed());
    assert_eq!(code, 0, "{err}");
    assert_eq!(log.iter().filter(|l| l.as_str() == "sccache cc -c sqlite3.c").count(), 1, "once: {log:?}");
    // A probe forked as the compile ends can log after its "done", so "done" is counted, not placed.
    assert_eq!(log.iter().filter(|l| l.as_str() == "done").count(), 1, "ran to its end: {log:?}");
    assert!(log.iter().all(|l| !l.starts_with("cc ")), "never run again bare: {log:?}");
    assert!(log.iter().any(|l| l == "sccache --show-stats"), "the server was asked: {log:?}");
    assert!(!err.contains("did not answer"), "{err}");
}
