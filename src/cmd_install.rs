//! `wsp install` — put a built binary at `~/.local/bin/wsp`, one at a time.
//!
//! Every panel, every detail pane, every daemon and every agent re-execs into
//! that one file. Being shared is the whole point of it, so nothing isolates
//! it: a build tree does not make it yours (robustness-012 says so outright) and
//! `wsp sandbox` works around it by running the built binary by path
//! (robustness-013). What was left over is the copy itself. Nothing serialised
//! it, so two agents installing different HEADs a minute apart is a real race
//! — and the loser does not find out. What ends up live is whichever `install
//! -m 755` ran second, and the agent whose work it reverted goes on believing
//! its change is in, which is how five hours went on 2026-08-16.
//!
//! Three things, in the order they matter:
//!
//! - **The copy is exclusive.** A lock beside the destination, taken across the
//!   copy and nothing else. `Store::locked` gives up after two seconds and
//!   carries on regardless, on the grounds that a lost update is recoverable
//!   where a `wsp claim` that never returns is not. Here the judgement flips on
//!   the second half only: the wait is just as short, but a lock we could not
//!   take is a refusal, because an install that goes ahead anyway is the exact
//!   thing this command exists to prevent. It says who holds it and why —
//!   "held by w1:p6 for 3s — installing fdefcab" is a sentence you can act on,
//!   and `busy` is not.
//!
//! - **The loser finds out.** Every install through here is written down beside
//!   the binary: who, when, from where, and at what commit the artefact says it
//!   carries — the last clause is not a flourish, and the section below is why.
//!   So the next install can say what it is replacing, and can refuse when the
//!   live binary was installed *after* the one in your hand was built — which
//!   is precisely the race, seen from the losing side, and the one moment when
//!   going ahead quietly reverts somebody.
//!
//! - **Nothing execs a half-written file.** The bytes go to a temporary in the
//!   destination's own directory and arrive by `rename`, which is atomic and
//!   which — unlike writing in place — leaves a running process holding the
//!   old inode rather than a file changing shape underneath it. Then the copy
//!   is read back and compared, because `committing.md` step 6 asks for exactly
//!   that check by hand and a check done by hand is a check that gets skipped.
//!
//! # What a binary carries is asked of the binary
//!
//! `worklist-042`. Every commit this command prints, records or compares comes
//! out of the artefact's own stamp — the one `build.rs` compiles in and
//! `--version` prints, read back by [`ask`] — on both sides of the copy. It
//! used to come out of the git tree the source was sitting in, which is a
//! different question with the same shape: a build tree has moved on from its
//! last release build in the ordinary case, and that is the same case somebody
//! runs `-n` in to decide whether to install. So the stand-in was wrong exactly
//! when it was read, and wrong in the worst available form — a short hash under
//! the word `carries`, printed by a tool, which nobody checks by hand.
//!
//! One reading fed seven outputs, and the printed line was the least of them:
//! the default `--why` the next agent reads off the lock, the commit written
//! into the record and quoted back by every later `-n`, the `installed … →`
//! line, three `--json` bodies, the `keeps the old one` warning — whose other
//! side is a running `wsp watch`'s [`crate::build_stamp`], so a tree and an
//! artefact were being compared against each other — and [`overtaken`], whose
//! one exception is *the two commits are equal*. Fed a tree's HEAD that
//! exception could fire on a coincidence, and let through the silent revert
//! this whole command exists to refuse.
//!
//! What the tree is still asked for is the one thing no stamp can carry: which
//! files the uncommitted work was in. `+dirty` is a bit; a file list is not,
//! and the build tree has it — for as long as the tree is still standing where
//! the build left it, which [`Built::left_behind`] now checks rather than
//! assumes.
//!
//! # What it will not do
//!
//! Build. `wsp verify --release` builds this agent's change in its own tree at
//! HEAD, which is the only build whose result means anything while somebody
//! else is in the checkout; this installs what that produced, and defaults to
//! finding it there. Two commands because they fail differently and are worth
//! reading separately — a build that does not compile and an install that would
//! overwrite somebody are not the same event.
//!
//! It also never decides *when*. Installing is deliberate and timed around the
//! other agents being idle, which is a judgement no lock replaces. The lock is
//! for the case where two people made that judgement at once.
//!
//! # Why the record lives beside the binary
//!
//! Not in the state directory, which is where every other machine-local fact
//! wsp keeps lives. A sandbox gets its own `WSP_STATE` — that is what makes it
//! a sandbox — and the thing being described here is the one file a sandbox
//! *cannot* have its own copy of. A record of who installed it is a fact about
//! that file, so it follows the file: `~/.local/bin/.wsp.install.json`, with
//! the lock beside it. `--to` therefore gets its own lock and its own record
//! for free, which is also what makes this testable without touching the
//! binary this machine is running.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::cmd_verify::{agent_key, git, toplevel};
use crate::store::Store;
use crate::util;
use crate::Args;

/// The one file every pane re-execs into.
fn default_dest() -> PathBuf {
    util::home().join(".local/bin/wsp")
}

/// Who is installing, named the way the panel names a pane. The pane rather
/// than the workspace, unlike a build tree: a build tree is a possession and
/// wants the durable name, and this is a sentence about something happening
/// right now, which wants the precise one.
fn who() -> String {
    match crate::herdr::Env::read().pane_id {
        Some(p) if !p.trim().is_empty() => p,
        _ => agent_key(),
    }
}

// ---- the lock ----------------------------------------------------------

/// How long to wait for somebody else's copy. A copy of a release binary is
/// tens of milliseconds; two seconds is already fifty of them.
const PATIENCE: Duration = Duration::from_millis(2000);

/// After this the holder is not slow, it is dead. Longer than the state lock's
/// thirty seconds because the two protect different things: that one is a
/// couple of file writes, and this one has a `cargo`-warm filesystem cache and
/// a several-megabyte copy on the other side of it.
const STALE: Duration = Duration::from_secs(60);

/// Somebody else's lock, read for the sentence it can be turned into.
#[derive(Debug)]
struct Held {
    who: String,
    why: String,
    pid: u64,
    age: i64,
}

impl Held {
    /// The sentence the refusal is made of. The pid is in it because the one
    /// question a wait raises is whether the holder is still alive, and `ps
    /// 4123` answers it without anybody having to find the lock file first.
    fn line(&self) -> String {
        let why = if self.why.trim().is_empty() { String::new() } else { format!(" — {}", self.why) };
        format!("held by {} (pid {}) for {}{}", self.who, self.pid, util::duration_human(self.age), why)
    }
}

fn lock_path(dst: &Path) -> PathBuf {
    let name = dst.file_name().and_then(|s| s.to_str()).unwrap_or("wsp");
    dst.parent().unwrap_or(Path::new(".")).join(format!(".{name}.install.lock"))
}

fn holder(path: &Path) -> Option<Held> {
    let text = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    let age = fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().unwrap_or_default().as_secs() as i64)
        .unwrap_or(0);
    Some(Held {
        who: v.get("who").and_then(|x| x.as_str()).unwrap_or("someone").to_string(),
        why: v.get("why").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        pid: v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0),
        age,
    })
}

/// Held for as long as the value lives, and given back by `Drop` — including
/// on the paths that refuse *inside* the lock, which is most of them. A lock
/// released only on the success path is a lock that wedges the machine the
/// first time the command says no.
struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Take the lock over `dst`, or come back with whoever has it.
///
/// `Err(None)` is a lock we could neither take nor read — an unwritable
/// directory, most likely, which the copy is about to fail on anyway.
fn take(dst: &Path, why: &str) -> Result<Lock, Option<Held>> {
    let path = lock_path(dst);
    let _ = fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    let start = std::time::Instant::now();
    loop {
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut fh) => {
                let body = json!({
                    "who": who(),
                    "why": why,
                    "pid": std::process::id(),
                    "at": util::now_iso(),
                });
                let _ = writeln!(fh, "{body}");
                return Ok(Lock { path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let held = holder(&path);
                if held.as_ref().map(|h| h.age).unwrap_or(0) as u64 > STALE.as_secs() {
                    // A minute in, whoever held this is not coming back, and
                    // the alternative to breaking it is every agent on the
                    // machine queued behind a corpse.
                    let _ = fs::remove_file(&path);
                    continue;
                }
                if start.elapsed() > PATIENCE {
                    return Err(held);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return Err(None),
        }
    }
}

// ---- the record --------------------------------------------------------

/// One install that happened.
struct Record {
    at: String,
    who: String,
    why: String,
    commit: Option<String>,
    dirty: bool,
    /// Size and mtime of what landed — [`util::stamp`]'s pair, so a live binary
    /// that no longer matches is a binary somebody installed by hand.
    size: u64,
    mtime: u64,
}

impl Record {
    fn from(v: &Value) -> Record {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        Record {
            at: s("at"),
            who: s("who"),
            why: s("why"),
            commit: v.get("commit").and_then(|x| x.as_str()).map(|s| s.to_string()),
            dirty: v.get("dirty").and_then(|x| x.as_bool()).unwrap_or(false),
            size: v.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
            mtime: v.get("mtime").and_then(|x| x.as_u64()).unwrap_or(0),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "at": self.at,
            "who": self.who,
            "why": self.why,
            "commit": self.commit,
            "dirty": self.dirty,
            "size": self.size,
            "mtime": self.mtime,
        })
    }

    /// "by w1:p6 12m ago — installing fdefcab"
    fn line(&self) -> String {
        let ago = util::since(&self.at);
        let why = if self.why.trim().is_empty() { String::new() } else { format!(" — {}", self.why) };
        format!("by {} {} ago{}", self.who, util::duration_human(ago), why)
    }
}

fn record_path(dst: &Path) -> PathBuf {
    let name = dst.file_name().and_then(|s| s.to_str()).unwrap_or("wsp");
    dst.parent().unwrap_or(Path::new(".")).join(format!(".{name}.install.json"))
}

/// The installs this machine remembers, newest first.
fn history(dst: &Path) -> Vec<Record> {
    let Ok(text) = fs::read_to_string(record_path(dst)) else {
        return Vec::new();
    };
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    v.get("installs")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().map(Record::from).collect())
        .unwrap_or_default()
}

/// Twenty is arbitrary and generous: this file grows by one line on a
/// deliberate act a few times a day, and the question it answers — "who has
/// been installing today, and what" — is worth a week of them.
const KEEP: usize = 20;

fn remember(dst: &Path, rec: &Record) {
    let mut all: Vec<Value> = vec![rec.to_json()];
    all.extend(history(dst).iter().take(KEEP - 1).map(|r| r.to_json()));
    let body = json!({ "path": dst.display().to_string(), "installs": all });
    let _ = crate::store::write_atomic(
        &record_path(dst),
        &serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into()),
    );
}

// ---- what we are holding -----------------------------------------------

/// What the binary in our hands carries, asked of the binary.
///
/// The doc that stood here called this "the weaker question that can be
/// answered today": render-057 would stamp the commit *into* the binary so any
/// copy of it could be asked, and until then the tree the source sat in was
/// the best stand-in available. render-057 shipped — `build.rs` stamps it,
/// `--version` prints it, [`ask`] reads it back — and this went on asking the
/// tree, with `newer` as the apology for the gap.
///
/// The gap is `worklist-042`. A build tree has moved on since its last release
/// build in the ordinary case, which is the same case somebody runs `-n` in to
/// decide; so the stand-in was wrong precisely when it was consulted, and it
/// was wrong in the worst available shape — a short hash, printed by a tool,
/// under the word `carries`. Nobody re-checks that by hand. `worklist-024`
/// corrected a version number in prose by trusting this command, and was only
/// right because it happened to read the `live` line, which comes from the
/// installed file.
///
/// So the identity comes from the artefact, and the tree is kept for the one
/// thing no stamp can carry — *which* files the uncommitted work was in — and
/// only for as long as the tree still describes the artefact.
struct Built {
    /// What the artefact says it was built from. `None` when it will not say
    /// and there was no tree to stand in for it either.
    commit: Option<String>,
    /// Whether that build carried work no commit held. The `+dirty` half of
    /// the stamp, and the load-bearing one: a hash describes everything about
    /// a build except the patch on top of it, and the patch is what goes
    /// missing.
    dirty: bool,
    /// Which files those were. Read from the tree, because nothing else knows,
    /// and empty unless the tree still describes the artefact. In a `wsp
    /// verify` tree it is exactly the patch under test.
    dirty_files: Vec<String>,
    /// Whether the artefact answered. False means `commit` is the tree's HEAD
    /// standing in, and everything printed from it says so.
    asked: bool,
    /// The tree's own HEAD, where there is a tree. Not the answer any more —
    /// the thing the answer is checked against, so a build its tree has walked
    /// away from can be named as one.
    head: Option<String>,
    /// Whether a source file in that tree is younger than the binary.
    newer: bool,
    /// When it was built, from [`written`] — nanoseconds, and the reason for
    /// them is in that function.
    built: u64,
}

impl Built {
    /// One word for this build, the same one a `wsp watch` registers for
    /// itself: `c52f3c8`, `c52f3c8+dirty`, or empty.
    ///
    /// Through [`crate::stamp_word`] rather than formatted here. Both sides of
    /// the comparison in [`stale_watchers`] have to be one shape, and this
    /// file keeping its own copy of the rule — over its own idea of the commit
    /// — is how they came to disagree about the same binary.
    fn stamp(&self) -> String {
        crate::stamp_word(self.commit.as_deref().unwrap_or_default(), self.dirty)
    }

    /// Where `commit` came from, in one word, for the reader that is a script.
    /// The printed output hedges a stand-in on the line itself; `--json` has no
    /// line to hedge, and a caller parsing this is exactly the caller that will
    /// not see a yellow word go past.
    fn commit_from(&self) -> &'static str {
        match (self.asked, self.commit.is_some()) {
            (true, _) => "artefact",
            (false, true) => "tree",
            (false, false) => "unknown",
        }
    }

    /// True when the tree the source sits in no longer describes it.
    ///
    /// Two ways, and the second is the one mtimes cannot see: a source file
    /// younger than the binary, or a HEAD that is not the commit the binary
    /// says it carries. `git commit` moves HEAD and touches no working file,
    /// so the tree can leave a build behind without a single mtime moving.
    fn left_behind(&self) -> bool {
        self.newer
            || matches!(
                (&self.head, &self.commit),
                (Some(h), Some(c)) if self.asked && h != c
            )
    }
}

/// When a file was last written, in nanoseconds since the epoch.
///
/// [`util::stamp`] keeps whole seconds, which is right for what it is for —
/// noticing that a binary was replaced, where the size moves too. This is
/// ordering two events against each other, and its whole job is deciding
/// whether the install that is already live happened after the build in your
/// hand. Two release builds finishing in one second is not fanciful on a
/// machine with four agents on it, and at whole-second resolution that
/// comparison silently answers "no" — which is precisely the revert this
/// command exists to refuse. APFS records nanoseconds; use them.
fn written(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// Newest mtime under `dir`, source files only. Cheap: a Rust crate's `src` is
/// tens of files, and this runs once per install.
fn newest(dir: &Path, out: &mut u64) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            newest(&path, out);
        } else if path.extension().and_then(|x| x.to_str()) == Some("rs") {
            *out = (*out).max(written(&path));
        }
    }
}

/// `tree` is where the binary was built, when the caller knows — and for the
/// case this command is mostly used on, only the caller can know it. A `wsp
/// verify` build tree keeps its `CARGO_TARGET_DIR` *beside* the tree rather
/// than inside it, which is what makes the build warm across a `git clean`; so
/// asking git about the directory the binary sits in finds no repository at
/// all, and a binary built minutes ago out of a known commit would report
/// "unknown — not a git tree".
fn provenance(src: &Path, tree: Option<PathBuf>) -> Built {
    let built = written(src);
    let dir = src.parent().unwrap_or(Path::new("."));
    let tree = tree.filter(|t| t.is_dir()).or_else(|| toplevel(dir));
    let head = tree
        .as_deref()
        .and_then(|t| git(t, &["rev-parse", "--short", "HEAD"]))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let files: Vec<String> = tree
        .as_deref()
        .and_then(|t| git(t, &["status", "--porcelain", "--untracked-files=all"]))
        .map(|out| {
            out.lines()
                .filter_map(|l| l.get(3..))
                .map(|p| p.rsplit(" -> ").next().unwrap_or(p).trim().trim_matches('"').to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut youngest = 0u64;
    if let Some(t) = tree.as_deref() {
        newest(&t.join("src"), &mut youngest);
        youngest = youngest.max(written(&t.join("Cargo.toml")));
    }
    let newer = youngest > built;

    // The artefact first. Running it costs a process and is a strictly smaller
    // act than the copy about to be made — `health` already runs the live one
    // for the same answer, and a binary too broken to print its own version is
    // one worth finding out about before every pane on the machine re-execs
    // into it. A stamp with no commit in it is an unstamped binary, which is
    // not an answer, so the tree gets to stand in for that too.
    let stamp = ask(src).filter(|s| !s.commit.is_empty());
    let asked = stamp.is_some();
    let commit = stamp.as_ref().map(|s| s.commit.clone()).or_else(|| head.clone());
    let dirty = match &stamp {
        Some(s) => s.dirty,
        None => !files.is_empty(),
    };
    // The stamp says *whether* there was uncommitted work and never which
    // files, because nothing can carry that. The tree can, and its list is the
    // artefact's for exactly as long as the tree is where the artefact left
    // it: same HEAD, nothing rebuilt since. Past that it is a list of files
    // about the tree, printed under the word `carries`, which is this row in
    // miniature.
    let describes = !asked || (!newer && head == commit);
    let dirty_files = if describes && dirty { files } else { Vec::new() };

    Built { commit, dirty, dirty_files, asked, head, newer, built }
}

/// True when what is live was installed *after* the binary in our hands was
/// built. That is the race from the losing side: our build cannot contain
/// whatever theirs shipped, so installing it now is a silent revert of it.
///
/// The one exception is worth having because it is the common case in a week
/// of two agents on one repository: if the record says the live binary is the
/// same commit as ours and neither build carried uncommitted work, then the
/// two binaries differ in build path and nothing else, and refusing would be
/// noise. Any dirt on either side and the exception is off — an uncommitted
/// patch is exactly the thing the commit hash does not describe.
fn overtaken(built: u64, live_at: u64, live: Option<&Record>, ours: &Built) -> bool {
    if live_at <= built {
        return false;
    }
    if ours.dirty {
        return true;
    }
    match (live, ours.commit.as_deref()) {
        (Some(rec), Some(c)) => !(rec.commit.as_deref() == Some(c) && !rec.dirty),
        _ => true,
    }
}

// ---- what is live, asked of the file itself -----------------------------

/// A binary's own account of what it was built from — the stamp `build.rs`
/// compiles in, read back out of `wsp --version`.
///
/// Asked by running it, rather than by reading the record beside it or the
/// tree it came out of. Those two describe an *install*, and both stop being
/// true the moment somebody copies a binary about with `install -m 755`, which
/// is how every install before `wsp install` existed was made. The stamp
/// travels with the bytes, so this is the one question that stays answerable.
struct Stamp {
    /// Empty when the binary answered without one: built outside a checkout,
    /// or built before `build.rs` existed.
    commit: String,
    dirty: bool,
}

/// `wsp 0.1.0 (c52f3c8+dirty)` → `Stamp`. `None` if it did not answer at all.
fn ask(bin: &Path) -> Option<Stamp> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let inner = text
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(inner, _)| inner.trim().to_string())
        .unwrap_or_default();
    let (commit, dirty) = match inner.split_once('+') {
        Some((c, flag)) => (c.to_string(), flag.trim() == "dirty"),
        None => (inner, false),
    };
    Some(Stamp { commit, dirty })
}

/// What the binary at `dst` says it carries, as the one word a register or a
/// record can be compared against, or `None` when it will not say.
///
/// Every caller of this used to reach for the record beside the file instead.
/// The record is the only thing that knows *who* installed it, *when* and
/// *why*, and it is not what knows *what*: a binary put there with `install -m
/// 755` — which is how every install before this command was made — writes no
/// record at all, and the bytes still answer.
fn carried_by(dst: &Path) -> Option<String> {
    ask(dst).map(|s| crate::stamp_word(&s.commit, s.dirty)).filter(|w| !w.is_empty())
}

/// What `doctor` says about the binary every pane re-execs into.
///
/// Notes rather than problems, all of them, and the reason is the same one
/// `herdr_health` gives for not calling a machine without herdr broken: being
/// a few commits behind is the ordinary state of an installed binary between
/// one deliberate install and the next, and a check that shows red every hour
/// of every day is a check that gets skipped along with the ones that mean
/// something. What is wanted here is not an alarm. It is that "is the thing I
/// am looking at the thing I just committed" has an answer at all — the one
/// question that would have closed 2026-08-16 in a minute rather than five
/// hours.
///
/// `roots` is what the projects declare, which is why this lives where
/// `doctor` can call it: the commit in the binary is a bare hash until some
/// repository on this machine recognises it, and wsp knows which repositories
/// those are because somebody wrote them down. The first root that has the
/// commit is the one it was built in — a hash nothing here has ever seen is a
/// binary from another machine, which is a fact and not a fault.
pub fn health(roots: &[PathBuf], notes: &mut Vec<String>) {
    let dst = default_dest();
    let where_ = util::contract(&dst);
    if !dst.is_file() {
        notes.push(format!("nothing installed at {where_}"));
        return;
    }
    let Some(stamp) = ask(&dst) else {
        notes.push(format!("the wsp at {where_} would not answer `--version`"));
        return;
    };
    notes.push(against(&stamp, roots));
}

/// The sentence itself, given a stamp and the repositories that might know it.
fn against(stamp: &Stamp, roots: &[PathBuf]) -> String {
    if stamp.commit.is_empty() {
        return "installed wsp does not say what it was built from — it predates the build stamp, and one `wsp install` makes it answerable".to_string();
    }

    // ", built from a dirty tree…" is the half worth saying whether or not any
    // root recognises the commit: a hash names a tree everybody can read, and
    // dirt names a tree only one agent ever saw.
    let dirt = match stamp.dirty {
        true => ", built from a dirty tree, so it carries work nobody has committed",
        false => "",
    };
    let known = roots
        .iter()
        .find(|r| git(r, &["cat-file", "-e", &format!("{}^{{commit}}", stamp.commit)]).is_some());
    let Some(root) = known else {
        return format!("installed wsp is {}, which no declared root has{dirt}", stamp.commit);
    };
    // One call for both directions. Behind is the ordinary case; ahead means
    // the binary was built from a commit this checkout's HEAD has not got —
    // an unlanded branch, or a tree that has been reset since.
    let counts = git(root, &["rev-list", "--left-right", "--count", &format!("HEAD...{}", stamp.commit)])
        .unwrap_or_default();
    let mut fields = counts.split_whitespace().map(|n| n.parse::<u64>().unwrap_or(0));
    let (behind, ahead) = (fields.next().unwrap_or(0), fields.next().unwrap_or(0));
    let repo = util::contract(root);
    let standing = match (behind, ahead) {
        (0, 0) => format!("HEAD in {repo}"),
        (0, a) => format!("{a} commit(s) {repo}'s HEAD has not got"),
        (b, 0) => format!("{b} commit(s) behind HEAD in {repo}"),
        (b, a) => format!("{b} commit(s) behind HEAD in {repo}, and {a} it has not got"),
    };
    let fix = match behind > 0 || stamp.dirty {
        true => " — `wsp verify --release` then `wsp install`",
        false => "",
    };
    format!("installed wsp is {} — {standing}{dirt}{fix}", stamp.commit)
}

// ---- the copy ----------------------------------------------------------

/// Put `bytes` at `dst` and prove they arrived.
///
/// Through a temporary in the destination's own directory — same filesystem,
/// so the `rename` is a rename rather than a copy — and never by writing `dst`
/// in place. A binary being executed right now is the worst file on the machine
/// to write into: `rename` leaves every running panel holding the old inode,
/// intact, until it re-execs of its own accord, and there is no instant at
/// which the path resolves to half a file.
///
/// Read back and compared afterwards because `committing.md` step 6 asks for
/// that `cmp` by hand, and the reason it asks is that an install can look like
/// it worked and not have.
fn place(bytes: &[u8], dst: &Path) -> Result<(), String> {
    let dir = dst.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", util::contract(dir)))?;
    let name = dst.file_name().and_then(|s| s.to_str()).unwrap_or("wsp");
    let tmp = dir.join(format!(".{name}.install.{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(format!("{}: {e}", util::contract(&tmp)));
    }
    if let Err(e) = fs::rename(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("{}: {e}", util::contract(dst)));
    }
    match fs::read(dst) {
        Ok(landed) if landed == bytes => Ok(()),
        Ok(landed) => Err(format!(
            "what landed is not what was built — {} bytes against {}",
            landed.len(),
            bytes.len()
        )),
        Err(e) => Err(format!("cannot read back {}: {e}", util::contract(dst))),
    }
}

// ---- the command -------------------------------------------------------

/// A binary to install, and the tree it came out of.
#[derive(Debug)]
struct Source {
    path: PathBuf,
    /// The checkout it was built from, when that is knowable — see
    /// [`provenance`] for why it cannot be recovered from the path.
    tree: Option<PathBuf>,
    origin: &'static str,
}

/// Where the binary to install comes from, in the order worth trying.
///
/// A named path first, then this agent's `wsp verify` tree — which is a build
/// at HEAD plus this agent's own patch and nothing else, so it is both the warm
/// one and the only one whose contents anybody can account for.
///
/// The `target/release` beside you is last, and how much that is worth now
/// depends on which tree you are standing in. In a per-task checkout it is your
/// own build of your own tree: accountable, just not pinned to HEAD. In the
/// trunk it is whatever the tree happened to hold at build time, including half
/// of somebody else's edit, and that is the one worth a warning.
fn source(store: &Store, named: Option<&str>) -> Result<Source, String> {
    if let Some(named) = named {
        let p = util::real(named);
        for c in [p.clone(), p.join("target/release/wsp"), p.join("wsp")] {
            if c.is_file() {
                return Ok(Source { path: c, tree: None, origin: "named" });
            }
        }
        return Err(format!("no binary at {}", util::contract(&p)));
    }

    let cwd = std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let repo = toplevel(&cwd).ok_or_else(|| {
        format!("{} is not in a git repository — name the binary to install", util::contract(&cwd))
    })?;
    built_here(store, &repo)
}

/// The same order, asked of one tree.
///
/// Split from [`source`] so the question can be put from a per-task checkout
/// without a process-wide `chdir`. Which tree it is asked of is the whole of
/// robustness-057 — the answer differs between the trunk and a checkout, and a
/// test that cannot choose the tree cannot catch it going wrong again.
fn built_here(store: &Store, repo: &Path) -> Result<Source, String> {
    // Asked of `verify` rather than worked out here. Computing it locally meant
    // computing it from the tree this is standing in, and inside a per-task
    // checkout that is not the trunk — so this looked for a release build under
    // a name `verify` never writes, found nothing, and fell through to the
    // shared `target/release` while reporting it as shared.
    let sc = crate::cmd_verify::last_build(store, repo, &agent_key());
    let mine = sc.target.join("release/wsp");
    if mine.is_file() {
        return Ok(Source { path: mine, tree: Some(sc.tree), origin: "verify" });
    }
    let beside = repo.join("target/release/wsp");
    if beside.is_file() {
        // "Everybody is editing this tree" stopped being true of every tree
        // when `wsp checkout` arrived: in a per-task checkout the release build
        // beside you is your own, and warning about somebody else's edits in it
        // is a warning that is wrong in the alarming direction — which is the
        // kind `cmd_checkout` argues gets ignored along with the true ones.
        let origin = if sc.checkout.is_some() { "own" } else { "shared" };
        return Ok(Source { path: beside, tree: Some(repo.to_path_buf()), origin });
    }
    // The path is only worth naming once a build has written one down. Without
    // that pointer `mine` is this agent's own cold tree, and a build now lands
    // in whichever warm tree was free — so naming it would be the same lie in
    // miniature as the lookup above once was: a sentence telling you to run a
    // command that will not write the file it names.
    Err(match crate::cmd_verify::built_at(&sc.dir) {
        Some(_) => format!(
            "nothing built to install — `wsp verify --release` builds it at {}",
            util::contract(&mine)
        ),
        None => "nothing built to install — `wsp verify --release` builds it".to_string(),
    })
}

/// The sentence the next agent reads off the lock, and what saying it cost the
/// positional argument.
#[derive(Debug, PartialEq)]
struct Why {
    /// `None` when nobody gave one — the caller's cue to fall back to the
    /// commit.
    text: Option<String>,
    /// The bare `-` on the command line was this flag's stream and not the
    /// binary to install. See [`why_given`].
    took_dash: bool,
}

/// Where the `--why` comes from: typed, a stream, or the file `--from` names.
///
/// **`worklist-045`.** This field had one spelling and it was the one the house
/// rule forbids. Every brief in the fleet says *give a wsp verb its prose
/// through a file*, because a shell evaluates backticks inside double quotes
/// and a `--tell` carrying a code span once ran it — and `block`, `park`,
/// `decide`, `note`, `flag`, `tell` and the prose editors all take `-` or
/// `--from`. `--why` took neither. So an agent doing the documented thing got
/// `wsp: no binary at -`, and the failure is worse than an absent form in two
/// ways: a lone dash is one character and no flag was taking it as a value, so
/// the parser handed it to the optional `<path>` and the error named an
/// argument nobody had mentioned; and it is the same message a genuinely
/// missing binary produces, so it is indistinguishable from a real fault about
/// a real file. What a caller does next is fall back to a quoted string, which
/// is the spelling the rule exists to prevent — the gap did not merely lack a
/// safe form, it returned you to the unsafe one.
///
/// It is not a minor field. `worklist-042` counted seven outputs of the install
/// identity, and this is the one written for a person: the sentence the next
/// agent reads off the lock, and the reason quoted back by every later `-n`.
/// The custodian's own `--why` at the phase-four barrier ran to three lines and
/// carried a self-correction — a paragraph, typed between double quotes,
/// because the safe spelling was not there.
///
/// # Why `--from` is read only after `--why`
///
/// `--to` is where the binary goes, so a bare `--from` reads as where it comes
/// from, and that is the one misreading this addition invites — it would record
/// a path as the reason and install something else entirely, quietly, which is
/// the fault this whole command exists to refuse. `--why --from FILE` cannot be
/// read that way, so that is the shape, and `--from` on its own is answered
/// with both readings rather than obeyed. Everywhere else in the CLI a bare
/// `--from` is complete, and it is not here for the one reason that this verb
/// already owns the other half of the pair.
fn why_given(args: &Args) -> Result<Why, i32> {
    let (reason, took_dash) = reason_given(args)?;
    let (raw, named) = match reason {
        Reason::Absent => return Ok(Why { text: None, took_dash }),
        Reason::Typed(s) => {
            // The check every other intake makes, on the one path a shell can
            // reach: a `--why` typed between double quotes comes back carrying
            // a command's output instead of the command's name. See
            // [`crate::util::terminal_output`] — and the second line is the
            // repair, because the caller cannot see the substitution in what
            // arrived.
            if let Some(what) = util::terminal_output(&s) {
                eprintln!("wsp: {what}");
                eprintln!("     a backtick inside double quotes runs a command. `--why -` reads the sentence from a stream, where a shell never sees it");
                return Err(2);
            }
            return Ok(Why { text: Some(crate::cmd_task::fold(&s)), took_dash });
        }
        Reason::Stream => (piped()?, "stdin".to_string()),
        Reason::File(path) => {
            let named = util::contract(&util::expand(&path));
            match crate::cmd_task::read_source(&path) {
                Ok(text) => (text, named),
                Err(e) => {
                    eprintln!("wsp: cannot read {named}: {e}");
                    return Err(1);
                }
            }
        }
    };

    // Folded, because the lock and the record are one-line sentences —
    // "held by w1:p6 for 3s — installing fdefcab" — and a paragraph pasted into
    // one is a line nobody can read either side of. Folding keeps every word
    // and costs the paragraph breaks, which is the trade `note` makes for the
    // same reason.
    let text = crate::cmd_task::fold(&raw);
    if text.is_empty() {
        // An empty stream reads exactly like no `--why` at all, and the two
        // want different things done about them: one is a caller who left the
        // default alone, the other is a caller whose paragraph went nowhere.
        eprintln!("wsp: nothing on {named} — the lock would say nothing about why");
        return Err(2);
    }
    Ok(Why { text: Some(text), took_dash })
}

/// The three places a `--why` comes from, settled before anything is read.
///
/// Separate from the reading so the grammar can be tested as grammar. What went
/// wrong here was a parse — one token read as a different argument — and a test
/// that has to pipe a stream in to reach it is a test of the wrong thing.
#[derive(Debug, PartialEq)]
enum Reason {
    /// Nobody gave one, and the commit will stand in.
    Absent,
    Typed(String),
    Stream,
    File(String),
}

/// Which of the three, and whether it took the bare `-` off the positionals.
fn reason_given(args: &Args) -> Result<(Reason, bool), i32> {
    let typed = args.get("why");
    // `--why` that swallowed nothing: the token after it began with `-`, so the
    // parser left the flag standing for itself. That, and an explicit `--why -`,
    // are the stream — the reading `cmd_task::prose_source` gives a lone
    // `--from` and `cmd_worklist::stop_prose` gives a lone `--stop`.
    let asked_for_prose = matches!(typed.as_deref(), Some("true" | "-"));
    // And this is where the dash went. `wsp install --why -` reaches here as
    // the flag above plus a bare `-` among the positionals, which is the slot
    // the binary to install lives in; `wsp install -` on its own is the same
    // positional with no flag in front of it, and is still an error about a
    // binary. The two readings are one token apart and the flag is what tells
    // them apart.
    let took_dash = typed.as_deref() == Some("true") && args.rest.iter().any(|a| a == "-");
    let sentence = typed.filter(|_| !asked_for_prose).filter(|s| !s.trim().is_empty());

    match (args.get("from"), sentence) {
        (Some(_), Some(_)) => {
            // Two sources for one sentence is refused rather than resolved:
            // picking either is the silent loss in a smaller hat, which is the
            // reading `cmd_agent::from_source` gives the same collision.
            eprintln!("wsp: a sentence and --from are two reasons — give one");
            Err(2)
        }
        (Some(_), None) if !asked_for_prose => {
            eprintln!("wsp: --from is where a --why is read from — `wsp install --why --from FILE`");
            eprintln!("     the binary to install is the positional: `wsp install <path>`");
            Err(2)
        }
        // A lone `--from`, or `--from -`, is the stream: a dash is not a path,
        // it is the conventional name for stdin.
        (Some(path), None) if matches!(path.as_str(), "true" | "-") => Ok((Reason::Stream, took_dash)),
        (Some(path), None) => Ok((Reason::File(path), took_dash)),
        (None, None) if asked_for_prose => Ok((Reason::Stream, took_dash)),
        (None, None) => Ok((Reason::Absent, took_dash)),
        (None, Some(s)) => Ok((Reason::Typed(s), took_dash)),
    }
}

/// The stream, refused before it is read when there is nobody on the other end.
///
/// Reading a terminal is not an empty reason, it is a command that stops and
/// says nothing while it swallows the keys — the one failure worse than the
/// silent one this is all about.
fn piped() -> Result<String, i32> {
    if util::stdin_is_tty() {
        eprintln!("wsp: nothing is piped in — `--why -` reads the sentence from a stream");
        return Err(2);
    }
    crate::cmd_task::read_source("-").map_err(|e| {
        eprintln!("wsp: cannot read stdin: {e}");
        1
    })
}

pub fn install(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let json_out = args.json();
    let dry = args.has("dry-run");

    let dst = args.get("to").map(|s| util::expand(&s)).unwrap_or_else(default_dest);

    // A sandbox is the one place this verb cannot be exercised honestly. `wsp
    // sandbox` hands an instance its own store, state and herdr session so that
    // a verb which writes can be run for real — and what this one writes is the
    // single file no sandbox has a copy of. Without `--to` it would reach
    // straight out of the sandbox and replace the binary every live pane
    // re-execs into, which is exactly the leak robustness-021 was flagged for.
    if std::env::var_os("WSP_BIN").is_some() && !args.has("to") {
        eprintln!(
            "wsp: this is a sandbox, and {} is not the sandbox's — name a destination with --to",
            util::contract(&dst)
        );
        return 2;
    }

    // Before the source is resolved, because `--why -` puts a bare `-` exactly
    // where the binary to install goes and this is what takes it back. Reading
    // the stream here also means a `--why` that could not be read is answered
    // before anything has been said about what would be installed.
    let why = match why_given(args) {
        Ok(w) => w,
        Err(code) => return code,
    };
    let mut positional = args.rest.iter().map(String::as_str);
    let named = match why.took_dash {
        true => positional.find(|a| *a != "-"),
        false => positional.next(),
    };

    let Source { path: src, tree, origin } = match source(store, named) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };
    if src == dst {
        eprintln!("wsp: {} is the destination", util::contract(&src));
        return 2;
    }

    let ours = provenance(&src, tree);
    let live = util::stamp(&dst);
    let last = history(&dst).into_iter().next();
    // A record whose stamp is not what is on disk describes a binary that has
    // been replaced since — by hand, by `install -m 755`, which is how every
    // install before this command was made. Say so rather than quoting it as
    // if it were the truth about the live file.
    let recorded = last
        .as_ref()
        .filter(|r| live.map(|(size, mtime)| r.size == size && r.mtime == mtime).unwrap_or(false));

    // A default rather than nothing: the lock and the record are read by
    // somebody who was not here, and "installing fdefcab" at least says what.
    let why = why.text.unwrap_or_else(|| match &ours.commit {
        Some(c) => format!("installing {c}"),
        None => "installing".to_string(),
    });

    let bytes = match fs::read(&src) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wsp: cannot read {}: {e}", util::contract(&src));
            return 2;
        }
    };
    let same = fs::read(&dst).map(|cur| cur == bytes).unwrap_or(false);

    let describe = |p: &util::Paint| {
        println!(
            "{} {} {}",
            p.dim("source"),
            util::contract(&src),
            p.dim(&format!(
                "built {} ago{}",
                util::duration_human(util::epoch_secs() - (ours.built / 1_000_000_000) as i64),
                match origin {
                    "verify" => ", in this agent's verify tree",
                    "own" => ", in this agent's checkout",
                    "shared" => ", in the shared checkout",
                    _ => "",
                }
            ))
        );
        // What it carries, and — when the artefact would not say — that this
        // is a stand-in. The word `carries` is read as a fact about the bytes,
        // so a line under it that is really about the tree has to admit it on
        // the same line, where somebody skimming three lines of output cannot
        // miss it.
        let carries = match (&ours.commit, ours.dirty, ours.dirty_files.len()) {
            (None, _, _) => p.yellow("unknown — it does not say, and there is no tree to ask"),
            (Some(c), false, _) => c.clone(),
            // Dirt the artefact admits to and the tree can no longer itemise:
            // the honest report is the fact without the list, rather than the
            // tree's current list under a heading about the binary.
            (Some(c), true, 0) => p.yellow(&format!("{c} + work no commit holds")),
            (Some(c), true, n) => p.yellow(&format!("{c} + {n} file(s) not committed")),
        };
        println!(
            "{} {}{}",
            p.dim("carries"),
            carries,
            match ours.asked {
                true => String::new(),
                false => p.yellow(" — the binary does not say; this is the tree it sits in"),
            }
        );
        for f in ours.dirty_files.iter().take(8) {
            println!("  {}", p.dim(f));
        }
        if ours.left_behind() {
            let now = match (&ours.head, &ours.commit) {
                (Some(h), Some(c)) if ours.asked && h != c => format!(", which is at {h}"),
                _ => String::new(),
            };
            println!(
                "{} the tree has changed since this was built{now} — `wsp verify --release` first",
                p.yellow("stale")
            );
        }
        // The shared checkout is the one build whose contents nobody can
        // account for; committing.md step 5 is explicit that there is nothing
        // on the other side of that tradeoff.
        if origin == "shared" && ours.dirty {
            println!(
                "{} {}",
                p.yellow("shared"),
                "this build came out of the tree everybody is editing"
            );
        }
        // What is live, asked of the file — the same rule this whole screen now
        // follows. The record beside it keeps who, when and why, which nothing
        // else on the machine knows, and it is the wrong place to learn *what*:
        // a binary put there by hand writes no record at all, and a record
        // written by a wsp older than `worklist-042` holds whatever that
        // agent's tree HEAD happened to be rather than what it shipped.
        let what = live
            .and_then(|_| carried_by(&dst))
            .or_else(|| recorded.and_then(|r| r.commit.clone()));
        // A hand install writes nothing down, so its age used to be all that
        // could be said about it. The stamp travels in the bytes, so what is in
        // it can now be said too — which is the one line of the shared-state
        // table this command could not fill in.
        let by_hand = |m: u64, verb: &str, tail: &str| {
            format!(
                "{}{verb} {} ago{tail}",
                what.as_deref().map(|c| format!("{c}, ")).unwrap_or_default(),
                util::duration_human(util::epoch_secs() - m as i64)
            )
        };
        let standing = match (live, recorded, last.is_some()) {
            (None, _, _) => p.dim("nothing there"),
            (Some(_), Some(r), _) => p.dim(&match &what {
                Some(c) => format!("{c} {}", r.line()),
                None => r.line(),
            }),
            (Some((_, m)), None, true) => {
                p.yellow(&by_hand(m, "changed", " by hand — wsp's record is of an older one"))
            }
            (Some((_, m)), None, false) => {
                p.dim(&by_hand(m, "installed", ", by nobody wsp knows"))
            }
        };
        println!("{} {} {}", p.dim("live"), util::contract(&dst), standing);
    };

    if !json_out {
        describe(&p);
    }

    if same {
        if json_out {
            println!(
                "{}",
                json!({"ok": true, "installed": false, "reason": "already live",
                       "source": util::contract(&src), "dest": util::contract(&dst),
                       "commit": ours.commit, "commit_from": ours.commit_from()})
            );
        } else {
            println!("{} {}", p.green("✓"), "already live — byte for byte what you built");
        }
        return 0;
    }

    let backwards = overtaken(ours.built, written(&dst), recorded, &ours);
    if backwards && !args.has("force") {
        let held = recorded.map(|r| r.line()).unwrap_or_else(|| match carried_by(&dst) {
            Some(c) => format!("{c} put there by hand — nothing beside it says who or why"),
            None => "by hand — nothing here says what is in it".to_string(),
        });
        if json_out {
            println!(
                "{}",
                json!({"ok": false, "installed": false, "reason": "overtaken",
                       "live": held, "source": util::contract(&src)})
            );
        } else {
            eprintln!(
                "{} what is live was installed after this was built, {}",
                p.red("✗"),
                held
            );
            eprintln!(
                "  {}",
                p.dim("installing it now reverts whatever that shipped. `wsp verify --release` to rebuild, or --force if you mean it")
            );
        }
        return 1;
    }

    if dry {
        if json_out {
            println!(
                "{}",
                json!({"ok": true, "installed": false, "reason": "dry run",
                       "source": util::contract(&src), "dest": util::contract(&dst),
                       "commit": ours.commit, "commit_from": ours.commit_from(),
                       "dirty": ours.dirty, "dirty_files": ours.dirty_files,
                       "stale": ours.left_behind(),
                       "lock": holder(&lock_path(&dst)).map(|h| h.line())})
            );
        } else {
            match holder(&lock_path(&dst)) {
                Some(h) => println!("{} {}", p.yellow("lock"), h.line()),
                None => println!("{} {}", p.dim("lock"), "free"),
            }
            println!("{} {}", p.dim("would install"), util::contract(&dst));
        }
        return 0;
    }

    let lock = match take(&dst, &why) {
        Ok(l) => l,
        Err(held) => {
            let line = held.map(|h| h.line()).unwrap_or_else(|| {
                format!("cannot lock {}", util::contract(&lock_path(&dst)))
            });
            if json_out {
                println!("{}", json!({"ok": false, "installed": false, "reason": "locked", "lock": line}));
            } else {
                eprintln!("{} another install is in flight — {}", p.red("✗"), line);
                eprintln!("  {}", p.dim("it takes a moment; run this again when it is done"));
            }
            return 1;
        }
    };

    // Inside the lock, and only now: whoever we waited for has just finished
    // writing, so what was live when we printed the header is not what is live
    // now. The check that matters is the one made here.
    let live_now = util::stamp(&dst);
    let last_now = history(&dst).into_iter().next();
    let recorded_now = last_now
        .as_ref()
        .filter(|r| live_now.map(|(size, mtime)| r.size == size && r.mtime == mtime).unwrap_or(false));
    if !args.has("force")
        && overtaken(ours.built, written(&dst), recorded_now, &ours)
    {
        let held = recorded_now.map(|r| r.line()).unwrap_or_else(|| match carried_by(&dst) {
            Some(c) => format!("{c} put there by hand — nothing beside it says who or why"),
            None => "by hand — nothing there says what is in it".to_string(),
        });
        if json_out {
            println!("{}", json!({"ok": false, "installed": false, "reason": "overtaken", "live": held}));
        } else {
            eprintln!("{} somebody installed while we waited, {}", p.red("✗"), held);
        }
        return 1;
    }

    if let Err(e) = place(&bytes, &dst) {
        drop(lock);
        eprintln!("wsp: {e}");
        return 2;
    }
    let (size, mtime) = util::stamp(&dst).unwrap_or((bytes.len() as u64, util::epoch_secs() as u64));
    let rec = Record {
        at: util::now_iso(),
        who: who(),
        why: why.clone(),
        commit: ours.commit.clone(),
        dirty: ours.dirty,
        size,
        mtime,
    };
    remember(&dst, &rec);
    drop(lock);

    if json_out {
        println!(
            "{}",
            json!({
                "ok": true,
                "installed": true,
                "source": util::contract(&src),
                "dest": util::contract(&dst),
                "commit": ours.commit,
                "commit_from": ours.commit_from(),
                "dirty": ours.dirty,
                "dirty_files": ours.dirty_files,
                "bytes": size,
                "who": rec.who,
                "why": why,
                // The same answer the printed line gives, for the reader that
                // is a script. Absent from neither: a caller parsing this is
                // exactly the caller that will not see a yellow line go past.
                "keeps_the_old_one": stale_watchers(store, &ours)
                    .iter()
                    .map(|w| json!({ "key": w.key, "scope": w.scope, "pid": w.pid, "build": w.build }))
                    .collect::<Vec<_>>(),
            })
        );
        return 0;
    }

    println!(
        "{} {} {}",
        p.green("✓"),
        p.bold(&format!(
            "installed {} → {}",
            ours.commit.clone().unwrap_or_else(|| "unknown".into()),
            util::contract(&dst)
        )),
        p.dim(&format!("{:.1} MB, read back and compared", size as f64 / 1_048_576.0))
    );
    println!(
        "{}",
        p.dim("panels, detail panes and the daemon re-exec into it within a tick — `wsp peek` to look at one")
    );
    keeps_the_old_one(store, &ours, &p);
    0
}

/// Name the long-lived processes that will **not** pick this up.
///
/// **`worklist-033`.** The line above is true and is the whole of what this
/// command has ever said, and what it leaves out is the half that cost a day:
/// `wsp watch` does not re-`exec`, so a governor's watch goes on reporting in
/// the vocabulary it was born with until somebody restarts it. Four instances
/// on 2026-08-19 across two seats, and every detection was a person
/// remembering — one of them a seat that had *just installed the fix to its own
/// watch* and went on running the fault.
///
/// Only what claimed a process and is still alive, and never the daemon: it
/// `exec`s within a tick, so its register is behind on purpose for a moment and
/// naming it here would make this line noise on every install. `--once` records
/// describe no process at all. That leaves exactly the watches, which is
/// exactly the set that is wrong.
///
/// Against what was just installed rather than against the binary running this
/// command — those are usually the same file and are not always, and the
/// question a reader has is about the one that is now live.
fn stale_watchers(store: &Store, ours: &Built) -> Vec<crate::cmd_watch::Registered> {
    // Both sides of this comparison are now what a binary says about itself: a
    // watch registers [`crate::build_stamp`], and [`Built::stamp`] is the same
    // rule applied to the stamp read back out of the artefact. Built from the
    // tree instead, this named the wrong set in both directions — a watch
    // already running these bytes reported as keeping the old one, and one
    // genuinely a build behind passing unnamed, which is the fault this line
    // exists to catch.
    let live = ours.stamp();
    if live.is_empty() {
        return Vec::new();
    }
    let watches: Vec<_> = crate::cmd_watch::registered(store)
        .into_iter()
        .filter(|w| w.watching() && !w.daemon && !w.build.is_empty() && w.build != live)
        .collect();
    let alive = crate::place_super::alive(&watches.iter().map(|w| w.pid).collect::<Vec<_>>());
    watches.into_iter().filter(|w| alive.contains(&w.pid)).collect()
}

fn keeps_the_old_one(store: &Store, ours: &Built, p: &crate::util::Paint) {
    for w in stale_watchers(store, ours) {
        println!(
            "{} {}",
            p.yellow("keeps the old one"),
            p.dim(&format!(
                "the watch on {} ({}, pid {}) is {} and does not re-exec — ^C and `wsp watch` again, or it reports {}'s logic all night",
                w.scope, w.key, w.pid, w.build, w.build
            ))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt as _;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-install-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn built(commit: Option<&str>, dirty: bool, at: u64) -> Built {
        Built {
            commit: commit.map(|s| s.to_string()),
            dirty,
            dirty_files: Vec::new(),
            asked: true,
            head: commit.map(|s| s.to_string()),
            newer: false,
            built: at,
        }
    }

    fn record(commit: &str, dirty: bool) -> Record {
        Record {
            at: util::now_iso(),
            who: "w2:p1".into(),
            why: format!("installing {commit}"),
            commit: Some(commit.to_string()),
            dirty,
            size: 1,
            mtime: 0,
        }
    }

    fn line(argv: &[&str]) -> crate::Args {
        crate::Args::parse(argv.iter().map(|s| (*s).to_string()).collect())
    }

    /// The fault, at the altitude it happened at. `wsp install --why -` was
    /// read as an install of a binary called `-`, because a lone dash is one
    /// character, no flag was taking it as a value, and the optional `<path>`
    /// was standing there to catch it. The error then named an argument nobody
    /// had typed.
    ///
    /// The two readings are one token apart, so both are asserted here: the
    /// dash belongs to `--why` when `--why` is in front of it, and to the
    /// binary when it is not. A test of only the first would pass on a fix that
    /// swallowed every dash on the line.
    #[test]
    fn a_dash_after_why_is_the_stream_and_a_dash_on_its_own_is_still_a_binary() {
        let (reason, took) = reason_given(&line(&["install", "--why", "-"])).unwrap();
        assert_eq!(reason, Reason::Stream);
        assert!(took, "the dash was left in the positionals for `<path>` to eat");

        let (reason, took) = reason_given(&line(&["install", "-"])).unwrap();
        assert_eq!(reason, Reason::Absent, "a bare dash is not a reason");
        assert!(!took, "nothing asked for a stream, so nothing may take the path");
        // A store this never reads — the named-path branch answers before it
        // asks — but `source` takes one, and pointing it at the real one is how
        // a test comes to read whoever is working today.
        let dir = scratch("dash");
        let store = Store::at(dir.clone(), dir.clone());
        assert_eq!(
            source(&store, Some("-")).err().as_deref(),
            Some("no binary at -"),
            "the error a genuinely missing binary should still get"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// And the binary is still found when both are on the line, in either
    /// order — the dash is the one positional `--why` may take, and it takes
    /// exactly that one.
    #[test]
    fn the_stream_takes_the_dash_and_leaves_the_path() {
        for argv in [
            ["install", "--why", "-", "/tmp/wsp"],
            ["install", "/tmp/wsp", "--why", "-"],
        ] {
            let args = line(&argv);
            let (_, took) = reason_given(&args).unwrap();
            let mut positional = args.rest.iter().map(String::as_str);
            let named = match took {
                true => positional.find(|a| *a != "-"),
                false => positional.next(),
            };
            assert_eq!(named, Some("/tmp/wsp"), "{argv:?} lost the binary");
        }
    }

    /// `--why --from FILE` is the file form, and a bare `--from` is not.
    ///
    /// Everywhere else in the CLI `--from FILE` on its own is complete. Here it
    /// is not, and the reason is `--to`: this verb already owns the other half
    /// of that pair, so `--from` reads as *where the binary comes from* — and
    /// obeying that reading would record a path as the reason and install
    /// something else, silently, which is the whole class of fault this command
    /// exists to refuse. So it is answered with both readings instead.
    #[test]
    fn from_names_the_file_a_why_is_read_from_only_once_a_why_has_asked() {
        assert_eq!(
            reason_given(&line(&["install", "--why", "--from", "why.md"])).unwrap(),
            (Reason::File("why.md".into()), false)
        );
        assert_eq!(
            reason_given(&line(&["install", "--from", "why.md"])),
            Err(2),
            "a bare --from was obeyed as prose, and it reads as the build"
        );
        assert_eq!(
            reason_given(&line(&["install", "--why", "typed", "--from", "why.md"])),
            Err(2),
            "two sources for one sentence"
        );
    }

    /// A sentence still goes on the line, and the paragraph it is not still
    /// arrives whole.
    #[test]
    fn a_typed_why_is_read_as_the_sentence_it_is() {
        assert_eq!(
            reason_given(&line(&["install", "--why", "installing the barrier fix"])).unwrap(),
            (Reason::Typed("installing the barrier fix".into()), false)
        );
        assert_eq!(
            reason_given(&line(&["install"])).unwrap(),
            (Reason::Absent, false),
            "no --why at all is the commit's job, not an error"
        );
    }

    /// The reason this field needed a stream at all: the house rule is *give a
    /// wsp verb its prose through a file*, because a shell runs every backtick
    /// inside double quotes. A stream never meets one, so what a file holds is
    /// what the lock says — code spans, line breaks and all, folded to the one
    /// line the lock and the record are written on.
    #[test]
    fn what_a_file_holds_is_what_the_lock_says() {
        let dir = scratch("why");
        let path = dir.join("why.md");
        fs::write(&path, "Phase four G1.\n\nCarries `worklist-045` — the stream form.\n").unwrap();

        let args = line(&["install", "--why", "--from", path.to_str().unwrap()]);
        let why = why_given(&args).unwrap();
        assert_eq!(
            why.text.as_deref(),
            Some("Phase four G1. Carries `worklist-045` — the stream form."),
            "the backticks did not survive, or the paragraph was not folded onto one line"
        );
        assert!(!why.took_dash);

        fs::write(&path, "   \n\n").unwrap();
        assert_eq!(
            why_given(&line(&["install", "--why", "--from", path.to_str().unwrap()])),
            Err(2),
            "a file with nothing in it read as a caller who wanted the default"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The whole point: while one install is in flight the next one is refused,
    /// and refused with a sentence naming who and why. "busy" is not something
    /// an agent — or a person at 20:31 with three commits in hand — can act on.
    #[test]
    fn a_second_install_is_refused_by_name_rather_than_queued_behind_silence() {
        let dir = scratch("lock");
        let dst = dir.join("wsp");

        let first = take(&dst, "installing fdefcab").expect("the first install could not lock");
        let second = take(&dst, "installing 4e1aac2");
        assert!(second.is_err(), "two installs held the same destination at once");
        let held = second.err().unwrap().expect("the refusal did not say who holds it");
        // Read out of the lock file rather than asserted against a literal: the
        // environment this runs in is not one this test gets to choose, and the
        // claim being made is that the holder is named at all.
        assert_eq!(held.who, who());
        assert!(held.line().contains("installing fdefcab"), "{}", held.line());
        assert!(held.pid > 0);

        // And it is given back by Drop, so a refusal inside the lock — which is
        // most of the ways this command says no — does not wedge the next one.
        drop(first);
        assert!(take(&dst, "third").is_ok(), "the lock was not released");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A holder that died is not a holder. The alternative is every agent on
    /// the machine queued behind a corpse until somebody notices — the same
    /// judgement `Store::locked` makes, on a longer clock because what this
    /// one protects is a several-megabyte copy rather than two file writes.
    #[test]
    fn a_lock_left_by_a_dead_process_is_broken_rather_than_waited_on() {
        let dir = scratch("stale");
        let dst = dir.join("wsp");
        let path = lock_path(&dst);
        fs::write(&path, r#"{"who":"w9:p9","why":"installing 0a2c9e2","pid":1}"#).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(STALE.as_secs() + 30);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();

        let t = std::time::Instant::now();
        let lock = take(&dst, "installing fdefcab");
        assert!(lock.is_ok(), "a minute-old lock wedged the install");
        assert!(t.elapsed() < PATIENCE, "we waited out a lock we were entitled to break");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The race, from the losing side. Our binary was built at 20:00; theirs
    /// went live at 20:05. Installing ours now reverts theirs and nothing says
    /// so — which is the failure this command exists for, and the one case
    /// where refusing beats warning.
    #[test]
    fn a_build_older_than_what_is_live_does_not_go_in_quietly() {
        let ours = built(Some("0a2c9e2"), false, 1_200);
        assert!(overtaken(ours.built, 1_500, Some(&record("4e1aac2", false)), &ours));

        // The ordinary case: what is live is older than our build, so our build
        // contains it.
        assert!(!overtaken(ours.built, 900, Some(&record("0a2c9e2", false)), &ours));

        // Nothing there at all.
        assert!(!overtaken(ours.built, 0, None, &ours));
    }

    /// Whole seconds are not enough to order two events on this machine. Two
    /// release builds finishing inside one second is ordinary with four agents
    /// in a checkout, and at `util::stamp`'s resolution the install that came
    /// second would compare as "not after" the build it is about to revert —
    /// the refusal would simply not fire, on the one occasion it is needed.
    #[test]
    fn two_builds_in_the_same_second_are_still_in_an_order() {
        let dir = scratch("nanos");
        let (early, late) = (dir.join("early"), dir.join("late"));
        let base = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        for (path, at) in
            [(&early, base + Duration::from_millis(120)), (&late, base + Duration::from_millis(880))]
        {
            fs::write(path, b"a binary").unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(at))
                .unwrap();
        }

        assert_eq!(
            util::stamp(&early).unwrap().1,
            util::stamp(&late).unwrap().1,
            "the fixture does not reproduce the case — the two are in different seconds"
        );
        assert!(written(&late) > written(&early), "one second of resolution lost the order");

        let ours = built(Some("0a2c9e2"), false, written(&early));
        assert!(
            overtaken(ours.built, written(&late), Some(&record("4e1aac2", false)), &ours),
            "an install 760ms after our build read as simultaneous, and would have reverted it"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Two agents building the same commit in their own trees produce two
    /// binaries that differ in build path and nothing else. Refusing the second
    /// would be noise, and a lock people work around is a lock they stop using.
    /// Any uncommitted work on either side and the exception is off: a commit
    /// hash says nothing about the patch on top of it.
    #[test]
    fn the_same_commit_installed_twice_is_not_a_revert_unless_something_is_uncommitted() {
        let clean = built(Some("fdefcab"), false, 1_200);
        assert!(!overtaken(clean.built, 1_500, Some(&record("fdefcab", false)), &clean));

        let ours_dirty = built(Some("fdefcab"), true, 1_200);
        assert!(
            overtaken(ours_dirty.built, 1_500, Some(&record("fdefcab", false)), &ours_dirty),
            "a patched build went in over a newer install of the same commit"
        );
        assert!(
            overtaken(clean.built, 1_500, Some(&record("fdefcab", true)), &clean),
            "we installed over somebody's patched build as if it were the plain commit"
        );

        // No record at all is the hand-installed case: something newer is there
        // and nothing can say what is in it, so it is not ours to overwrite.
        assert!(overtaken(clean.built, 1_500, None, &clean));
    }

    /// The copy is a rename, and it is checked. Writing a binary in place is
    /// the one thing that must never happen here — a running panel would be
    /// executing a file changing shape underneath it — and an install that
    /// silently half-worked is worse than one that failed.
    #[test]
    fn what_lands_is_what_was_built_and_it_lands_whole() {
        let dir = scratch("place");
        let dst = dir.join("wsp");
        fs::write(&dst, b"old binary").unwrap();
        let before = fs::metadata(&dst).unwrap().ino();

        place(b"new binary, longer", &dst).expect("the install failed");
        assert_eq!(fs::read(&dst).unwrap(), b"new binary, longer");
        assert_ne!(before, fs::metadata(&dst).unwrap().ino(), "the file was written in place");
        assert_eq!(
            fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
            0o755,
            "the installed binary is not executable"
        );

        // Nothing left in the directory but the binary: a temporary abandoned
        // beside ~/.local/bin/wsp is one more thing to explain later.
        let strays: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "wsp")
            .collect();
        assert!(strays.is_empty(), "the copy left {strays:?} behind");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The record is what makes the loser find out, so it has to describe the
    /// file it is beside — and it has to admit when it does not. Every install
    /// before this command was a hand-run `install -m 755`, which writes no
    /// record; a record quoted over the top of one of those would be a
    /// confident answer about a binary nobody wrote down.
    #[test]
    fn a_record_that_no_longer_matches_the_file_is_not_quoted_as_the_truth() {
        let dir = scratch("record");
        let dst = dir.join("wsp");
        fs::write(&dst, b"installed by wsp").unwrap();
        let (size, mtime) = util::stamp(&dst).unwrap();
        remember(
            &dst,
            &Record {
                at: util::now_iso(),
                who: "w1:p6".into(),
                why: "installing fdefcab".into(),
                commit: Some("fdefcab".into()),
                dirty: false,
                size,
                mtime,
            },
        );

        let last = history(&dst).into_iter().next().expect("the install was not written down");
        assert_eq!(last.commit.as_deref(), Some("fdefcab"));
        assert!(last.line().contains("w1:p6"), "{}", last.line());
        let live = util::stamp(&dst).unwrap();
        assert!(last.size == live.0 && last.mtime == live.1, "the record does not describe the file");

        // Somebody installs by hand over the top of it.
        fs::write(&dst, b"installed by hand, a different length").unwrap();
        let live = util::stamp(&dst).unwrap();
        let last = history(&dst).into_iter().next().unwrap();
        assert!(
            !(last.size == live.0 && last.mtime == live.1),
            "a hand install would have been reported as wsp's own"
        );

        // …and the record survives it, so the next install still knows who was
        // here before.
        assert_eq!(history(&dst).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A binary that answers `--version` with a hash, and one that does not.
    ///
    /// Asked by running the file, which is the whole claim: the record beside
    /// a binary describes an install and stops being true the moment somebody
    /// copies the bytes somewhere else, and the stamp does not.
    #[test]
    fn what_a_binary_carries_is_asked_of_the_binary() {
        let dir = scratch("stamp");
        let fake = |name: &str, says: &str| {
            let p = dir.join(name);
            fs::write(&p, format!("#!/bin/sh\necho '{says}'\n")).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            p
        };

        let s = ask(&fake("dirty", "wsp 0.1.0 (c52f3c8+dirty)")).expect("it did not answer");
        assert_eq!(s.commit, "c52f3c8");
        assert!(s.dirty, "the +dirty half was dropped, which is the half that matters");

        let s = ask(&fake("clean", "wsp 0.1.0 (c52f3c8)")).expect("it did not answer");
        assert_eq!((s.commit.as_str(), s.dirty), ("c52f3c8", false));

        // Every wsp installed before `build.rs` existed. It answers, and what
        // it says is that it cannot say — which is a different thing from a
        // binary that will not run, and is reported differently.
        let s = ask(&fake("old", "wsp 0.1.0")).expect("an unstamped binary should still answer");
        assert!(s.commit.is_empty());
        assert!(
            against(&s, &[]).contains("predates the build stamp"),
            "{}",
            against(&s, &[])
        );

        assert!(ask(&dir.join("absent")).is_none(), "a binary that is not there answered");

        // And the one word the rest of the file compares against, made through
        // `main` so a `wsp watch`'s register and an install record cannot come
        // to hold two shapes for one build.
        assert_eq!(carried_by(&fake("word", "wsp 0.1.0 (c52f3c8+dirty)")).as_deref(), Some("c52f3c8+dirty"));
        assert_eq!(carried_by(&fake("plain", "wsp 0.1.0 (c52f3c8)")).as_deref(), Some("c52f3c8"));
        // An unstamped binary answers, and what it says is that it cannot say.
        // A word that is not a build is worse than no word: it would be
        // compared, and it would match every other one of its kind.
        assert!(carried_by(&fake("mute", "wsp 0.1.0")).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// `worklist-042`, which is the whole of this row: what a build carries is
    /// asked of the build, and the tree it is sitting in does not get to
    /// answer for it.
    ///
    /// The tree has moved on from its last release build in the ordinary case
    /// — that is what a build tree *is* between one `wsp verify --release` and
    /// the next — and that is the same case somebody runs `-n` in, to decide
    /// whether to install. So the tree's HEAD under the word `carries` was a
    /// short hash, printed by a tool, wrong exactly when it was read.
    #[test]
    fn what_the_source_carries_is_asked_of_the_source_and_not_of_its_tree() {
        let dir = scratch("provenance");
        let repo = dir.join("repo");
        fs::create_dir_all(repo.join("src")).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "--quiet", "-b", "master"]);
        let commit = |n: &str| {
            fs::write(repo.join("src/lib.rs"), format!("// {n}\n")).unwrap();
            run(&["add", "-A"]);
            run(&["commit", "--quiet", "-m", n]);
            git(&repo, &["rev-parse", "--short", "HEAD"]).unwrap().trim().to_string()
        };
        commit("one");

        // A binary that says what it is, in a tree that has since moved on.
        let bin = |name: &str, says: &str| {
            let b = dir.join(name);
            fs::write(&b, format!("#!/bin/sh\necho '{says}'\n")).unwrap();
            fs::set_permissions(&b, fs::Permissions::from_mode(0o755)).unwrap();
            b
        };
        let src = bin("stamped", "wsp 0.1.0 (be58dac)");
        let head = commit("two");
        assert_ne!(head, "be58dac");

        let ours = provenance(&src, Some(repo.clone()));
        assert_eq!(
            ours.commit.as_deref(),
            Some("be58dac"),
            "the tree's HEAD answered for a binary that was standing right there"
        );
        assert_eq!(ours.commit_from(), "artefact");
        assert_eq!(ours.head.as_deref(), Some(head.as_str()), "the tree's HEAD is kept, as the check");
        assert!(ours.left_behind(), "a build its tree has walked away from was not called stale");

        // And a stale build is stale on the commit alone. `git commit` moves
        // HEAD without touching one working file, so the mtime comparison that
        // was the only test here cannot see it.
        let untouched = provenance(&src, Some(repo.clone()));
        assert!(!untouched.newer || untouched.left_behind());

        // The word both sides of the watch comparison are made of. Formatted
        // once, in `main`, so `stale_watchers` and a running `wsp watch` cannot
        // come to disagree about one binary.
        let dirty_src = bin("patched", "wsp 0.1.0 (be58dac+dirty)");
        let patched = provenance(&dirty_src, Some(repo.clone()));
        assert!(patched.dirty, "the +dirty half was dropped");
        assert_eq!(patched.stamp(), "be58dac+dirty");
        assert_eq!(crate::stamp_word("be58dac", true), patched.stamp());
        // The tree has files no commit holds — this one's own `src/lib.rs` is
        // committed, so it does not — but whatever it has, it is not this
        // binary's patch, and a list of them under `carries` would be the same
        // fault one level down.
        assert!(
            patched.dirty_files.is_empty(),
            "the tree's uncommitted files were reported as the artefact's: {:?}",
            patched.dirty_files
        );

        // Kept, and it is the reason the tree is still read at all: a tree that
        // has *not* moved does describe the build sitting in it, and its list
        // of uncommitted files is the only answer anywhere to "which patch is
        // in these bytes". No stamp can carry a file list.
        fs::write(repo.join("src/patch.rs"), "// no commit holds this\n").unwrap();
        let here = bin("here", &format!("wsp 0.1.0 ({head}+dirty)"));
        let standing = provenance(&here, Some(repo.clone()));
        assert!(!standing.left_behind(), "a build its tree is still standing on was called stale");
        assert_eq!(
            standing.dirty_files,
            vec!["src/patch.rs".to_string()],
            "the patch under test stopped being named"
        );

        // What is left of the old behaviour, and it is the right amount of it:
        // a binary that will not say — every wsp built before `build.rs`, and
        // anything that is not a wsp — falls back to the tree, and says so.
        let mute = bin("old", "wsp 0.1.0");
        let stood_in = provenance(&mute, Some(repo.clone()));
        assert_eq!(stood_in.commit.as_deref(), Some(head.as_str()));
        assert_eq!(stood_in.commit_from(), "tree");
        assert!(!stood_in.asked);
        // …and standing in, it is not evidence that the tree left anything
        // behind: there is nothing to disagree with.
        assert!(!stood_in.left_behind() || stood_in.newer);

        // Nothing to ask and nothing to ask of: no tree, and a file that will
        // not run. The one honest answer is that there is no answer.
        let brick = dir.join("brick");
        fs::write(&brick, b"not a program").unwrap();
        let nothing = provenance(&brick, None);
        assert!(nothing.commit.is_none(), "something was invented for a file that cannot answer");
        assert_eq!(nothing.commit_from(), "unknown");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The half of this that is not a printed line. `overtaken` is the refusal
    /// that stops one agent's install silently reverting another's, and its one
    /// exception turns on the two commits being equal. Fed the tree's HEAD, the
    /// exception fired on a *coincidence* — my tree happens to stand where your
    /// install came from — and let through the exact revert it exists to refuse.
    #[test]
    fn the_revert_check_compares_what_is_in_the_binaries_and_not_where_a_tree_stands() {
        // Live is `c0f9113`, installed after our build. Our binary is really
        // `be58dac`; the tree around it has since moved to `c0f9113`.
        let ours = built(Some("be58dac"), false, 1_200);
        assert!(
            overtaken(ours.built, 1_500, Some(&record("c0f9113", false)), &ours),
            "a newer install was overwritten because the tree agreed with it"
        );

        // And the exception it is worth keeping: the same build, arrived at in
        // two trees, is not a revert.
        let same = built(Some("c0f9113"), false, 1_200);
        assert!(!overtaken(same.built, 1_500, Some(&record("c0f9113", false)), &same));
    }

    /// The question the whole task is about: is what is installed what was
    /// last committed. A hash on its own cannot say — it needs a repository
    /// that recognises it, which is why `doctor` passes in the roots the
    /// projects declare rather than this hunting for checkouts.
    #[test]
    fn an_installed_binary_is_placed_against_the_head_of_the_tree_that_knows_it() {
        let dir = scratch("standing");
        let repo = dir.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "--quiet", "-b", "master"]);
        let commit = |n: &str| {
            fs::write(repo.join("f.txt"), format!("{n}\n")).unwrap();
            run(&["add", "f.txt"]);
            run(&["commit", "--quiet", "-m", n]);
            git(&repo, &["rev-parse", "--short", "HEAD"]).unwrap().trim().to_string()
        };
        let first = commit("one");
        let roots = vec![repo.clone()];

        let stamp = |c: &str, dirty: bool| Stamp { commit: c.to_string(), dirty };
        let at_head = against(&stamp(&first, false), &roots);
        assert!(at_head.contains("HEAD"), "{at_head}");
        assert!(!at_head.contains("behind"), "a binary at HEAD was reported as behind: {at_head}");

        commit("two");
        let behind = against(&stamp(&first, false), &roots);
        assert!(behind.contains("1 commit(s) behind HEAD"), "{behind}");
        assert!(behind.contains(&util::contract(&repo)), "it did not say which tree: {behind}");

        // Dirt is the load-bearing half: the five hours on 2026-08-16 went on a
        // binary carrying two finished features that no commit held, and a
        // hash describes everything about a build except that.
        let dirty = against(&stamp(&first, true), &roots);
        assert!(dirty.contains("nobody has committed"), "{dirty}");

        // A commit no checkout here has ever seen — a binary from another
        // machine. A fact, not a fault, and never dressed up as a comparison
        // that was not made.
        let elsewhere = against(&stamp("0a2c9e2", false), &roots);
        assert!(elsewhere.contains("no declared root has"), "{elsewhere}");
        assert!(!elsewhere.contains("behind"), "{elsewhere}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The lock and the record follow the file rather than the state directory,
    /// which is the one thing a sandbox replaces. Two destinations are two
    /// locks — which is also what lets this be tested at all, without touching
    /// the binary this machine is running.
    #[test]
    fn the_lock_belongs_to_the_file_and_not_to_this_wsps_state() {
        let a = lock_path(Path::new("/Users/x/.local/bin/wsp"));
        let b = lock_path(Path::new("/tmp/sandbox/bin/wsp"));
        assert_eq!(a, PathBuf::from("/Users/x/.local/bin/.wsp.install.lock"));
        assert_ne!(a, b, "two destinations shared one lock");
        assert_eq!(
            record_path(Path::new("/Users/x/.local/bin/wsp")),
            PathBuf::from("/Users/x/.local/bin/.wsp.install.json")
        );
    }

    /// robustness-057, from the checkout side. `install` used to work the build
    /// tree's name out itself, from the tree it was standing in — which inside
    /// `.worktrees/<task>` is the task and not the repository — so it looked
    /// where `verify` never writes. The visible failure was not an error: it
    /// fell through to the `target/release` lying beside it and reported that
    /// as the build you had just made. So the neighbour is planted here too,
    /// and finding it is the failure this asserts against.
    #[test]
    fn from_a_checkout_install_looks_where_verify_actually_built() {
        let iso = util::isolated("install-from-checkout");
        let store = Store::at(iso.home(), iso.state());
        let checkout = iso.path("wsp/.worktrees/robustness-057");

        let warm = iso.path("state/warm/wsp-0/target/release");
        fs::create_dir_all(&warm).unwrap();
        fs::write(warm.join("wsp"), b"the build verify made").unwrap();
        let sc = crate::cmd_verify::scratch(&store, &checkout, &agent_key());
        fs::create_dir_all(&sc.dir).unwrap();
        fs::write(
            sc.dir.join(crate::cmd_verify::BUILT_AT),
            format!("{}\n", warm.parent().unwrap().display()),
        )
        .unwrap();

        let beside = checkout.join("target/release");
        fs::create_dir_all(&beside).unwrap();
        fs::write(beside.join("wsp"), b"whatever was lying about").unwrap();

        let found = built_here(&store, &checkout).expect("install found nothing to install");
        assert_eq!(found.path, warm.join("wsp"), "install looked somewhere verify never wrote");
        assert_eq!(found.origin, "verify", "somebody else's build, reported as this agent's");
    }

    /// The other half of the same complaint: the sentence you get when there is
    /// nothing to install told you to run a command at a path that command
    /// would not write, because a build lands in whichever warm tree is free.
    /// A path is named when a build wrote one down, and not otherwise.
    #[test]
    fn a_path_is_named_only_when_a_build_has_written_one_down() {
        let iso = util::isolated("install-nothing-built");
        let store = Store::at(iso.home(), iso.state());
        let checkout = iso.path("wsp/.worktrees/robustness-057");

        let err = built_here(&store, &checkout).expect_err("something was installable");
        assert!(err.starts_with("nothing built to install"), "{err}");
        assert!(!err.contains('/'), "named a path nothing has written: {err}");

        // Now a build has been here — the tree it used is a fact, and worth
        // saying, even though this run of it produced no release binary.
        let warm = iso.path("state/warm/wsp-0/target");
        fs::create_dir_all(&warm).unwrap();
        let sc = crate::cmd_verify::scratch(&store, &checkout, &agent_key());
        fs::create_dir_all(&sc.dir).unwrap();
        fs::write(sc.dir.join(crate::cmd_verify::BUILT_AT), format!("{}\n", warm.display()))
            .unwrap();

        let err = built_here(&store, &checkout).expect_err("something was installable");
        assert!(
            err.ends_with(&util::contract(&warm.join("release/wsp"))),
            "the tree the build actually used went unsaid: {err}"
        );
    }
}
