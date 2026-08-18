//! `wsp install` — put a built binary at `~/.local/bin/wsp`, one at a time.
//!
//! Every panel, every detail pane, every daemon and every agent re-execs into
//! that one file. Being shared is the whole point of it, so nothing isolates
//! it: a build tree does not make it yours (t-260816-054 says so outright) and
//! `wsp sandbox` works around it by running the built binary by path
//! (t-260816-056). What was left over is the copy itself. Nothing serialised
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
//!   the binary: who, when, from where, at what commit. So the next install can
//!   say what it is replacing, and can refuse when the live binary was
//!   installed *after* the one in your hand was built — which is precisely the
//!   race, seen from the losing side, and the one moment when going ahead
//!   quietly reverts somebody.
//!
//! - **Nothing execs a half-written file.** The bytes go to a temporary in the
//!   destination's own directory and arrive by `rename`, which is atomic and
//!   which — unlike writing in place — leaves a running process holding the
//!   old inode rather than a file changing shape underneath it. Then the copy
//!   is read back and compared, because `committing.md` step 6 asks for exactly
//!   that check by hand and a check done by hand is a check that gets skipped.
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

/// What can be said about a built binary by looking at the tree it came out of.
///
/// Not the same question as t-260816-050's, which stamps the commit *into* the
/// binary so that any copy of it can be asked. This is the weaker one that can
/// be answered today, and it is enough for the decision in hand: the source is
/// still sitting in the tree it was built in, so that tree's HEAD and dirt
/// describe it — as long as nothing has moved since, which is why `newer`
/// exists.
struct Built {
    commit: Option<String>,
    /// Files the tree has that its HEAD does not. In a `wsp verify` tree this
    /// is exactly the patch under test, which is the honest answer to "what
    /// uncommitted work does this binary carry".
    dirty: Vec<String>,
    /// Whether a source file in that tree is younger than the binary. If one
    /// is, the tree moved after the build and `commit` and `dirty` describe
    /// something the binary is not.
    newer: bool,
    /// When it was built, from [`written`] — nanoseconds, and the reason for
    /// them is in that function.
    built: u64,
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
    let commit = tree
        .as_deref()
        .and_then(|t| git(t, &["rev-parse", "--short", "HEAD"]))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let dirty: Vec<String> = tree
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
    Built { commit, dirty, newer: youngest > built, built }
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
    if !ours.dirty.is_empty() {
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
fn source(store: &Store, args: &Args) -> Result<Source, String> {
    if let Some(named) = args.rest.first() {
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

    // Asked of `verify` rather than worked out here. Computing it locally meant
    // computing it from the tree this is standing in, and inside a per-task
    // checkout that is not the trunk — so this looked for a release build under
    // a name `verify` never writes, found nothing, and fell through to the
    // shared `target/release` while reporting it as shared.
    let sc = crate::cmd_verify::last_build(store, &repo, &agent_key());
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
        return Ok(Source { path: beside, tree: Some(repo), origin });
    }
    Err(format!(
        "nothing built to install — `wsp verify --release` builds it at {}",
        util::contract(&mine)
    ))
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
    // re-execs into, which is exactly the leak t-260816-076 was flagged for.
    if std::env::var_os("WSP_BIN").is_some() && !args.has("to") {
        eprintln!(
            "wsp: this is a sandbox, and {} is not the sandbox's — name a destination with --to",
            util::contract(&dst)
        );
        return 2;
    }

    let Source { path: src, tree, origin } = match source(store, args) {
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

    let why = args
        .get("why")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| match &ours.commit {
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
        match &ours.commit {
            Some(c) if ours.dirty.is_empty() => println!("{} {}", p.dim("carries"), c),
            Some(c) => println!(
                "{} {}",
                p.dim("carries"),
                p.yellow(&format!("{c} + {} file(s) not committed", ours.dirty.len()))
            ),
            None => println!("{} {}", p.dim("carries"), p.yellow("unknown — not a git tree")),
        }
        for f in ours.dirty.iter().take(8) {
            println!("  {}", p.dim(f));
        }
        if ours.newer {
            println!(
                "{} {}",
                p.yellow("stale"),
                "the tree has changed since this was built — `wsp verify --release` first"
            );
        }
        // The shared checkout is the one build whose contents nobody can
        // account for; committing.md step 5 is explicit that there is nothing
        // on the other side of that tradeoff.
        if origin == "shared" && !ours.dirty.is_empty() {
            println!(
                "{} {}",
                p.yellow("shared"),
                "this build came out of the tree everybody is editing"
            );
        }
        let standing = match (live, recorded, last.is_some()) {
            (None, _, _) => p.dim("nothing there"),
            (Some(_), Some(r), _) => p.dim(&match &r.commit {
                Some(c) => format!("{c} {}", r.line()),
                None => r.line(),
            }),
            // A binary with no matching record is one somebody put there with
            // `install -m 755`, which is how every install before this command
            // was made. Its age is all that can honestly be said about it.
            (Some((_, m)), None, true) => p.yellow(&format!(
                "changed {} ago by hand — wsp's record is of an older one",
                util::duration_human(util::epoch_secs() - m as i64)
            )),
            (Some((_, m)), None, false) => p.dim(&format!(
                "installed {} ago, by nobody wsp knows",
                util::duration_human(util::epoch_secs() - m as i64)
            )),
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
                       "commit": ours.commit})
            );
        } else {
            println!("{} {}", p.green("✓"), "already live — byte for byte what you built");
        }
        return 0;
    }

    let backwards = overtaken(ours.built, written(&dst), recorded, &ours);
    if backwards && !args.has("force") {
        let held = recorded
            .map(|r| r.line())
            .unwrap_or_else(|| "by hand — nothing here says what is in it".to_string());
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
                       "commit": ours.commit, "dirty": ours.dirty,
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
        let held = recorded_now
            .map(|r| r.line())
            .unwrap_or_else(|| "by hand — nothing there says what is in it".to_string());
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
        dirty: !ours.dirty.is_empty(),
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
                "dirty": ours.dirty,
                "bytes": size,
                "who": rec.who,
                "why": why,
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
    0
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

    fn built(commit: Option<&str>, dirty: &[&str], at: u64) -> Built {
        Built {
            commit: commit.map(|s| s.to_string()),
            dirty: dirty.iter().map(|s| s.to_string()).collect(),
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
        let ours = built(Some("0a2c9e2"), &[], 1_200);
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

        let ours = built(Some("0a2c9e2"), &[], written(&early));
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
        let clean = built(Some("fdefcab"), &[], 1_200);
        assert!(!overtaken(clean.built, 1_500, Some(&record("fdefcab", false)), &clean));

        let ours_dirty = built(Some("fdefcab"), &["src/cmd_install.rs"], 1_200);
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
        let _ = fs::remove_dir_all(&dir);
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
}
