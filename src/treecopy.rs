//! The tree copy: one row's worktree, up to an executor and back, with hashes
//! deciding every step (`docs/remote-agents.md`, section 6).
//!
//! Nothing here is git's to decide. The far side is an ordinary directory
//! (`<exec_dir>/trees/<row>`) with a baseline `git init` so the agent can commit;
//! what *changed* is a comparison of manifests, one line per file — `mode sha256
//! path`, sorted — and a manifest's own sha256 is the *tree hash*. Four things are
//! kept, and the reason each exists is a refusal it makes possible:
//!
//! - **base**: the manifest at copy-up, on both sides. Its hash is what `verify`
//!   and `review` compare.
//! - **synced**: the seat's manifest of what it last agreed on with the far side.
//!   It starts as the base and moves as files are copied back. Without it a second
//!   checkpoint would read the first one's own copy as "somebody changed this on
//!   the seat", and refuse the agent's next edit of a file it had already edited.
//! - **copy.json**: seat HEAD at copy-up, how far the trunk has moved since
//!   (a number, not silence — wsp-072's complaint), and what has been carried.
//! - **verified**: the tree hash `verify` ran on.
//!
//! The transport is `rsync -a` over the machine's existing ControlMaster, so there
//! is no second authentication; where rsync is missing on either side it is `tar`
//! over the same ssh. The far build is never the verdict: the verdict is `verify`
//! on the tree that came back.
//!
//! The seat's own tests use a stand-in `ssh` (a script that drops the connection
//! options and runs the command in a local `sh`), which is faithful to the one
//! thing the real one does here: join the words and hand them to a shell.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use crate::store::Store;

// ---- what is left out ---------------------------------------------------

/// Directories not copied, at any depth. `.git` is also skipped when it is a
/// *file*, which is what a worktree has: it points at an absolute path in the
/// seat's main repository, and means nothing over there.
const LEFT_OUT_DIRS: [&str; 4] = ["target", "node_modules", ".venv", ".worktrees"];

/// The same list as flags, for rsync and tar, and as a `find` expression for the
/// far manifest — three spellings of one rule, and a test holds them to it.
fn rsync_excludes() -> Vec<String> {
    let mut v = vec!["--exclude=.git".to_string()];
    v.extend(LEFT_OUT_DIRS.iter().map(|d| format!("--exclude={d}/")));
    v.push("--exclude=*.log".into());
    v
}

fn tar_excludes() -> Vec<String> {
    let mut v = vec!["--exclude=.git".to_string()];
    v.extend(LEFT_OUT_DIRS.iter().map(|d| format!("--exclude={d}")));
    v.push("--exclude=*.log".into());
    v
}

/// The far manifest, as a script. Run by `sh -c`, with the tree as `$1`.
///
/// "A ten-line script beside the shim", not a wsp binary: the far side has `find`
/// and a sha tool, and nothing of ours.
const FAR_MANIFEST: &str = r#"cd "$1" || exit 3
if command -v sha256sum >/dev/null 2>&1; then H=sha256sum; else H="shasum -a 256"; fi
find . -name .git -prune -o \( -type d \( -name target -o -name node_modules -o -name .venv -o -name .worktrees \) \) -prune -o \( -type f -o -type l \) ! -name '*.log' -print | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r p; do
  if [ -L "$p" ]; then m=120000; h=$(readlink "$p" | $H | cut -d' ' -f1)
  else if [ -x "$p" ]; then m=755; else m=644; fi; h=$($H < "$p" | cut -d' ' -f1); fi
  printf '%s %s %s\n' "$m" "$h" "$p"
done
"#;

// ---- sha256 -------------------------------------------------------------

/// sha256, in hex. Written out rather than depended on: this crate has one
/// dependency and a manifest needs a hash on the seat that agrees with the far
/// side's `sha256sum`, which a test checks against it.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

// ---- manifests ----------------------------------------------------------

/// One file as the manifest knows it: `755` or `644` (the executable bit and
/// nothing else, because the two machines disagree about the rest), or `120000`
/// for a link, hashed by its target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub mode: String,
    pub sha: String,
}

impl Entry {
    /// `mode sha`, for a sentence that names both hashes.
    fn show(&self) -> String {
        format!("{} {}", self.mode, &self.sha[..self.sha.len().min(12)])
    }
}

/// Every copied file, by path. A `BTreeMap` over `String` sorts by bytes, which
/// is what `LC_ALL=C sort` does on the far side, so the two render identically.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest(pub BTreeMap<String, Entry>);

impl Manifest {
    /// The manifest of a directory on this machine.
    pub fn of_dir(root: &Path) -> Result<Manifest, String> {
        let mut m = BTreeMap::new();
        walk(root, root, &mut m)?;
        Ok(Manifest(m))
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (p, e) in &self.0 {
            out.push_str(&format!("{} {} {}\n", e.mode, e.sha, p));
        }
        out
    }

    /// Lines that do not read are skipped: a manifest is rebuilt from the tree,
    /// so a damaged one costs a recompute and not a wrong answer.
    pub fn parse(text: &str) -> Manifest {
        let mut m = BTreeMap::new();
        for line in text.lines() {
            let mut f = line.splitn(3, ' ');
            if let (Some(mode), Some(sha), Some(path)) = (f.next(), f.next(), f.next()) {
                if sha.len() == 64 && !path.is_empty() {
                    m.insert(path.to_string(), Entry { mode: mode.into(), sha: sha.into() });
                }
            }
        }
        Manifest(m)
    }

    /// The manifest's own hash: *the* tree hash.
    pub fn tree_hash(&self) -> String {
        sha256_hex(self.render().as_bytes())
    }

    /// The paths where `self` and `other` disagree, either way, sorted.
    pub fn differing(&self, other: &Manifest) -> Vec<String> {
        let mut out: Vec<String> = self
            .0
            .iter()
            .filter(|(p, e)| other.0.get(*p) != Some(*e))
            .map(|(p, _)| p.clone())
            .chain(other.0.keys().filter(|p| !self.0.contains_key(*p)).cloned())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Entry>) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if name == ".git" || (ft.is_dir() && LEFT_OUT_DIRS.contains(&name.as_str())) {
            continue;
        }
        if ft.is_dir() {
            walk(root, &path, out)?;
            continue;
        }
        if name.ends_with(".log") {
            continue;
        }
        let rel = path.strip_prefix(root).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
        if rel.contains('\n') {
            return Err(format!("a path with a newline in it cannot be in a manifest: {rel:?}"));
        }
        let entry = if ft.is_symlink() {
            let t = fs::read_link(&path).map_err(|e| format!("{rel}: {e}"))?;
            Entry { mode: "120000".into(), sha: sha256_hex(format!("{}\n", t.to_string_lossy()).as_bytes()) }
        } else if ft.is_file() {
            use std::os::unix::fs::PermissionsExt;
            let mode = e.metadata().map_err(|e| format!("{rel}: {e}"))?.permissions().mode();
            let data = fs::read(&path).map_err(|e| format!("{rel}: {e}"))?;
            Entry { mode: if mode & 0o111 != 0 { "755" } else { "644" }.into(), sha: sha256_hex(&data) }
        } else {
            continue;
        };
        out.insert(rel, entry);
    }
    Ok(())
}

// ---- the far side -------------------------------------------------------

/// Single-quote a word for a shell.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A machine, as far as copying is concerned: how to run a command there and where
/// the row's things live. Built from the machine's record by [`Remote::of_machine`];
/// a test builds one with a stand-in `ssh`.
#[derive(Clone, Debug)]
pub struct Remote {
    /// The ssh program. `ssh`, or a stand-in.
    pub ssh: String,
    /// The `Host` alias.
    pub host: String,
    /// The ControlMaster socket the tunnel holds, so nothing authenticates twice.
    pub control: Option<PathBuf>,
    /// Absolute, on the far machine. Trees and per-row state live under it and
    /// nowhere else.
    pub exec_dir: String,
    /// Use `tar` over ssh even where rsync is there.
    pub tar: bool,
}

impl Remote {
    /// The connection [`crate::tunnel`] holds open for `m`, reused: the same
    /// `cm-<name>` path in the same directory, so a copy rides the master.
    pub fn of_machine(store: &Store, m: &crate::model::Machine, exec_dir: &str) -> Remote {
        let dir = store.machine_socket(&m.name).parent().map(Path::to_path_buf).unwrap_or_else(|| store.state.clone());
        Remote {
            ssh: "ssh".into(),
            host: m.ssh.clone(),
            control: Some(dir.join(format!("cm-{}", m.name))),
            exec_dir: exec_dir.trim_end_matches('/').to_string(),
            tar: false,
        }
    }

    pub fn tree_of(&self, row: &str) -> String {
        format!("{}/trees/{row}", self.exec_dir)
    }

    pub fn state_of(&self, row: &str) -> String {
        format!("{}/state/{row}", self.exec_dir)
    }

    /// The words that start an ssh to this machine, up to the host.
    fn ssh_words(&self) -> Vec<String> {
        let mut v = vec![self.ssh.clone()];
        if let Some(c) = &self.control {
            v.push("-S".into());
            v.push(c.display().to_string());
        }
        v.push("-o".into());
        v.push("BatchMode=yes".into());
        v
    }

    fn run_in(&self, cmd: &str, stdin: Option<&[u8]>) -> Result<String, String> {
        let mut words = self.ssh_words();
        let prog = words.remove(0);
        let mut c = Command::new(prog);
        c.args(words).arg(&self.host).arg(cmd);
        c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| format!("{}: {e}", self.ssh))?;
        if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
            let _ = si.write_all(data);
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "`{}` on {} said: {}",
                cmd.chars().take(80).collect::<String>(),
                self.host,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn run(&self, cmd: &str) -> Result<String, String> {
        self.run_in(cmd, None)
    }

    /// The same connection, as one `-e` argument for rsync.
    fn rsync_e(&self) -> String {
        self.ssh_words().join(" ")
    }

    /// rsync is on both ends and not switched off.
    fn rsync_usable(&self) -> bool {
        !self.tar
            && Command::new("rsync").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success())
            && self.run("command -v rsync").is_ok()
    }

    /// The manifest of a tree over there.
    fn manifest(&self, tree: &str) -> Result<Manifest, String> {
        let text = self.run(&format!("sh -c {} sh {}", shq(FAR_MANIFEST), shq(tree)))?;
        Ok(Manifest::parse(&text))
    }
}

/// A shell pipeline, run here.
fn sh(cmd: &str) -> Result<String, String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        // Both streams: rsync and tar say why on either, and an empty reason
        // ("rsync up failed: ") is no reason.
        let err = String::from_utf8_lossy(&out.stderr);
        let said = String::from_utf8_lossy(&out.stdout);
        Err(format!("{}{}", err.trim(), if said.trim().is_empty() { String::new() } else { format!(" {}", said.trim()) }).trim().to_string())
    }
}

/// git on the seat, in `dir`. `GIT_INDEX_FILE` is dropped for the reason
/// `cmd_verify::git` gives: an agent's shell may have a private one exported.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {}: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        ))
    }
}

// ---- the record ---------------------------------------------------------

/// What copy-up wrote down, and what has moved since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyRecord {
    pub row: String,
    pub host: String,
    pub far_tree: String,
    /// The seat's HEAD when the tree was copied.
    pub seat_head: String,
    pub trunk: String,
    /// Commits the trunk was ahead of `seat_head` at copy-up. A number.
    pub trunk_ahead: u64,
    pub base_tree: String,
    /// The baseline commit on the far side.
    pub far_base: String,
    /// The tree held no uncommitted work when it was copied, which is what makes
    /// the agent's commits applicable as patches.
    pub seat_clean: bool,
    /// The far commit whose patches have been applied on the seat.
    pub applied_through: String,
}

impl CopyRecord {
    fn to_json(&self) -> Value {
        json!({
            "row": self.row, "host": self.host, "far_tree": self.far_tree,
            "seat_head": self.seat_head, "trunk": self.trunk, "trunk_ahead": self.trunk_ahead,
            "base_tree": self.base_tree, "far_base": self.far_base,
            "seat_clean": self.seat_clean, "applied_through": self.applied_through,
        })
    }

    fn from_json(v: &Value) -> Option<CopyRecord> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(CopyRecord {
            row: s("row")?,
            host: s("host")?,
            far_tree: s("far_tree")?,
            seat_head: s("seat_head")?,
            trunk: s("trunk")?,
            trunk_ahead: v.get("trunk_ahead")?.as_u64()?,
            base_tree: s("base_tree")?,
            far_base: s("far_base")?,
            seat_clean: v.get("seat_clean")?.as_bool()?,
            applied_through: s("applied_through")?,
        })
    }

    /// One line for `wsp show`: where the copy stands against the trunk.
    pub fn line(&self) -> String {
        format!(
            "copied to {}:{} from {} · trunk was {} commit{} ahead of it",
            self.host,
            self.far_tree,
            &self.seat_head[..self.seat_head.len().min(8)],
            self.trunk_ahead,
            if self.trunk_ahead == 1 { "" } else { "s" }
        )
    }
}

/// Where a row's copy records live on the seat.
pub fn record_dir(store: &Store, row: &str) -> PathBuf {
    store.state.join("trees").join(row)
}

/// The row's copy record, when it has been copied anywhere.
pub fn record_of(store: &Store, row: &str) -> Option<CopyRecord> {
    let text = fs::read_to_string(record_dir(store, row).join("copy.json")).ok()?;
    CopyRecord::from_json(&serde_json::from_str(&text).ok()?)
}

/// A path the seat changed since the base, and the agent changed too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stale {
    pub path: String,
    /// What the two sides last agreed on, as `mode sha`; `absent` for none.
    pub base: String,
    /// What the seat holds now.
    pub seat: String,
    /// What the far side holds.
    pub far: String,
}

impl Stale {
    pub fn words(&self) -> String {
        format!(
            "{} — the seat changed it since the copy (agreed {}, seat now {}), and the agent changed it too (far {}); not overwritten",
            self.path, self.base, self.seat, self.far
        )
    }
}

/// What a copy-back did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Back {
    pub copied: Vec<String>,
    pub deleted: Vec<String>,
    pub refused: Vec<Stale>,
}

fn show(e: Option<&Entry>) -> String {
    e.map_or_else(|| "absent".to_string(), Entry::show)
}

// ---- the copy -----------------------------------------------------------

/// One row's copy: the machine, the seat's tree, and where the seat keeps its
/// records of it.
pub struct Copy {
    pub remote: Remote,
    pub row: String,
    pub tree: PathBuf,
    pub dir: PathBuf,
}

impl Copy {
    pub fn new(store: &Store, remote: Remote, row: &str, tree: &Path) -> Copy {
        Copy { remote, row: row.to_string(), tree: tree.to_path_buf(), dir: record_dir(store, row) }
    }

    fn far_tree(&self) -> String {
        self.remote.tree_of(&self.row)
    }

    fn far_state(&self) -> String {
        self.remote.state_of(&self.row)
    }

    fn read_manifest(&self, name: &str) -> Manifest {
        Manifest::parse(&fs::read_to_string(self.dir.join(name)).unwrap_or_default())
    }

    fn write_manifest(&self, name: &str, m: &Manifest) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        fs::write(self.dir.join(name), m.render()).map_err(|e| e.to_string())
    }

    fn record(&self) -> Result<CopyRecord, String> {
        let text = fs::read_to_string(self.dir.join("copy.json"))
            .map_err(|_| format!("{} has not been copied up: no record of a copy", self.row))?;
        serde_json::from_str(&text).ok().and_then(|v| CopyRecord::from_json(&v)).ok_or_else(|| "the copy record does not read".to_string())
    }

    fn write_record(&self, r: &CopyRecord) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        fs::write(self.dir.join("copy.json"), serde_json::to_string_pretty(&r.to_json()).unwrap_or_default())
            .map_err(|e| e.to_string())
    }

    /// Copy the tree up, start its far repository, and prove the two sides hold
    /// the same files — or leave nothing there.
    ///
    /// Refuses a row that is already copied, on either side: a second copy-up
    /// over a tree the agent has been working in is the one thing here that loses
    /// work.
    pub fn copy_up(&self, trunk: &str) -> Result<CopyRecord, String> {
        if !self.remote.exec_dir.starts_with('/') {
            return Err(format!("exec_dir `{}` is not an absolute path on {}", self.remote.exec_dir, self.remote.host));
        }
        if self.row.is_empty() || self.row.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')) {
            return Err(format!("`{}` is not a row name a far path can be made of", self.row));
        }
        if self.dir.join("copy.json").exists() {
            return Err(format!("{} is already copied up ({}); a second copy-up would overwrite the agent's tree", self.row, self.dir.display()));
        }
        let (ft, fs_dir) = (self.far_tree(), self.far_state());
        let probe = format!("if [ -e {s} ] || [ -n \"$(ls -A {t} 2>/dev/null)\" ]; then echo busy; fi", s = shq(&fs_dir), t = shq(&ft));
        if self.remote.run(&probe)?.trim() == "busy" {
            return Err(format!("{}:{} already holds something for {} — not copying over it", self.remote.host, ft, self.row));
        }

        let seat_head = git(&self.tree, &["rev-parse", "HEAD"])?.trim().to_string();
        let ahead_range = format!("HEAD..{trunk}");
        let ahead: u64 = git(&self.tree, &["rev-list", "--count", &ahead_range])
            .map_err(|e| format!("cannot say how far {trunk} is ahead: {e}"))?
            .trim()
            .parse()
            .map_err(|_| "rev-list --count did not give a number".to_string())?;
        let clean = git(&self.tree, &["status", "--porcelain", "--untracked-files=all"])?.trim().is_empty();
        let base = Manifest::of_dir(&self.tree)?;

        self.remote.run(&format!("mkdir -p {} {}", shq(&ft), shq(&fs_dir)))?;
        let result = self.send_tree(&ft).and_then(|_| {
            let far = self.remote.manifest(&ft)?;
            let off = base.differing(&far);
            if off.is_empty() {
                Ok(())
            } else {
                Err(format!("the copy does not match by hash at: {}", off.iter().take(8).cloned().collect::<Vec<_>>().join(", ")))
            }
        });
        if let Err(e) = result {
            // The far side was empty a moment ago (checked above), so clearing it
            // is putting it back, not deleting anything.
            let _ = self.remote.run(&format!("rm -rf {} {}", shq(&ft), shq(&fs_dir)));
            return Err(e);
        }

        let init = format!(
            "cd {t} && git init -q && git add -A && git -c user.name=wsp -c user.email=wsp@localhost commit -q --allow-empty -m {m} && git rev-parse HEAD",
            t = shq(&ft),
            m = shq(&format!("baseline: {} as copied from seat {}", self.row, seat_head))
        );
        let far_base = self.remote.run(&init)?.trim().to_string();
        self.remote.run_in(&format!("cat > {}", shq(&format!("{fs_dir}/base.manifest"))), Some(base.render().as_bytes()))?;

        let rec = CopyRecord {
            row: self.row.clone(),
            host: self.remote.host.clone(),
            far_tree: ft,
            seat_head,
            trunk: trunk.to_string(),
            trunk_ahead: ahead,
            base_tree: base.tree_hash(),
            far_base: far_base.clone(),
            seat_clean: clean,
            applied_through: far_base,
        };
        self.write_manifest("base.manifest", &base)?;
        self.write_manifest("synced.manifest", &base)?;
        self.write_record(&rec)?;
        Ok(rec)
    }

    fn send_tree(&self, far: &str) -> Result<(), String> {
        let src = self.tree.display().to_string();
        if self.remote.rsync_usable() {
            let mut cmd = format!("rsync -a --delete {} -e {} {}/ {}:{}/", rsync_excludes().iter().map(|x| shq(x)).collect::<Vec<_>>().join(" "), shq(&self.remote.rsync_e()), shq(&src), shq(&self.remote.host), shq(far));
            cmd.push_str(" 2>&1");
            sh(&cmd).map(|_| ()).map_err(|e| format!("rsync up failed: {e}"))
        } else {
            let ssh = self.remote.ssh_words().iter().map(|w| shq(w)).collect::<Vec<_>>().join(" ");
            let cmd = format!(
                "tar -C {} {} -cf - . | {} {} {}",
                shq(&src),
                tar_excludes().iter().map(|x| shq(x)).collect::<Vec<_>>().join(" "),
                ssh,
                shq(&self.remote.host),
                shq(&format!("tar -C {} -xf -", shq(far)))
            );
            sh(&cmd).map(|_| ()).map_err(|e| format!("tar up failed: {e}"))
        }
    }

    /// Bring the far files down: only those whose far hash differs from what the
    /// two sides last agreed on, and never one the seat changed since.
    ///
    /// For each path in that set: the seat already holds the far version → agreed,
    /// nothing to copy; the seat still holds the agreed version → copied (or
    /// deleted); anything else is somebody else's change, and is *refused, named
    /// with all three hashes, and left alone*. The rest are still copied: a stale
    /// file is not a reason to hold back a checkpoint.
    pub fn copy_back(&self) -> Result<Back, String> {
        self.record()?;
        let mut synced = self.read_manifest("synced.manifest");
        let far = self.remote.manifest(&self.far_tree())?;
        let seat = Manifest::of_dir(&self.tree)?;
        let mut back = Back::default();
        let mut fetch: Vec<String> = Vec::new();

        for path in far.differing(&synced) {
            let (agreed, theirs, mine) = (synced.0.get(&path), far.0.get(&path), seat.0.get(&path));
            if mine == theirs {
                match theirs {
                    Some(e) => synced.0.insert(path.clone(), e.clone()),
                    None => synced.0.remove(&path),
                };
            } else if mine == agreed {
                match theirs {
                    Some(_) => fetch.push(path.clone()),
                    None => {
                        let p = self.tree.join(&path);
                        fs::remove_file(&p).map_err(|e| format!("{path}: {e}"))?;
                        prune_empty_parents(&self.tree, &p);
                        synced.0.remove(&path);
                        back.deleted.push(path.clone());
                    }
                }
            } else {
                back.refused.push(Stale { path: path.clone(), base: show(agreed), seat: show(mine), far: show(theirs) });
            }
        }

        if !fetch.is_empty() {
            self.fetch_files(&fetch)?;
            // What arrived is what the far side said it held: a file that changed
            // in between is not recorded as agreed, and is picked up next time.
            let now = Manifest::of_dir(&self.tree)?;
            for p in &fetch {
                if now.0.get(p) == far.0.get(p) {
                    synced.0.insert(p.clone(), far.0[p].clone());
                    back.copied.push(p.clone());
                } else {
                    return Err(format!("{p} did not arrive as the far side's version (it changed while it was copied, or the transport mangled it)"));
                }
            }
        }
        self.write_manifest("synced.manifest", &synced)?;
        Ok(back)
    }

    fn fetch_files(&self, files: &[String]) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let list = self.dir.join("fetch.list");
        fs::write(&list, files.join("\n") + "\n").map_err(|e| e.to_string())?;
        let (far, seat) = (self.far_tree(), self.tree.display().to_string());
        let out = if self.remote.rsync_usable() {
            sh(&format!(
                "rsync -a --files-from={} -e {} {}:{}/ {}/ 2>&1",
                shq(&list.display().to_string()),
                shq(&self.remote.rsync_e()),
                shq(&self.remote.host),
                shq(&far),
                shq(&seat)
            ))
        } else {
            let ssh = self.remote.ssh_words().iter().map(|w| shq(w)).collect::<Vec<_>>().join(" ");
            sh(&format!(
                "{} {} {} < {} | tar -C {} -xf -",
                ssh,
                shq(&self.remote.host),
                shq(&format!("tar -C {} -cf - -T -", shq(&far))),
                shq(&list.display().to_string()),
                shq(&seat)
            ))
        };
        let _ = fs::remove_file(&list);
        out.map(|_| ()).map_err(|e| format!("copy back failed: {e}"))
    }

    /// The agent's commits, applied on the seat with `git am`, so the log, the
    /// authors and the messages survive (open decision 1 in the doc: patches).
    ///
    /// Needs the seat tree to have been clean at copy-up — the patches are against
    /// that — and to hold nothing but what checkpoints copied back, which are
    /// reverted first (a checkpoint copies *files*, and `am` of a commit whose
    /// files are already there refuses). A path the seat changed itself is named
    /// and nothing is touched. Returns how many commits came.
    pub fn carry_commits(&self) -> Result<usize, String> {
        let mut rec = self.record()?;
        if !rec.seat_clean {
            return Err("the seat's tree held uncommitted work when it was copied, so the agent's commits are not a patch series against it; the files come back, the commits do not".into());
        }
        let ft = self.far_tree();
        let fg = |args: &str| self.remote.run(&format!("git -C {} {}", shq(&ft), args));
        let head = fg("rev-parse HEAD")?.trim().to_string();
        if head == rec.applied_through {
            return Ok(0);
        }
        fg(&format!("merge-base --is-ancestor {} HEAD", rec.applied_through))
            .map_err(|_| "the far history no longer contains what was already carried back (rewritten there?)".to_string())?;
        let n: usize = fg(&format!("rev-list --count {}..HEAD", rec.applied_through))?.trim().parse().unwrap_or(0);

        // Fetch the patches first: nothing on the seat changes until they are here.
        let pdir_far = format!("{}/patches", self.far_state());
        self.remote.run(&format!("rm -rf {p} && git -C {t} format-patch -q -o {p} {a}..HEAD >/dev/null", p = shq(&pdir_far), t = shq(&ft), a = rec.applied_through))?;
        let pdir = self.dir.join("patches");
        let _ = fs::remove_dir_all(&pdir);
        fs::create_dir_all(&pdir).map_err(|e| e.to_string())?;
        let down = if self.remote.rsync_usable() {
            sh(&format!("rsync -a -e {} {}:{}/ {}/ 2>&1", shq(&self.remote.rsync_e()), shq(&self.remote.host), shq(&pdir_far), shq(&pdir.display().to_string())))
        } else {
            let ssh = self.remote.ssh_words().iter().map(|w| shq(w)).collect::<Vec<_>>().join(" ");
            sh(&format!("{} {} {} | tar -C {} -xf -", ssh, shq(&self.remote.host), shq(&format!("tar -C {} -cf - .", shq(&pdir_far))), shq(&pdir.display().to_string())))
        };
        down.map_err(|e| format!("fetching the patches failed: {e}"))?;
        let mut patches: Vec<PathBuf> = fs::read_dir(&pdir)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |x| x == "patch"))
            .collect();
        patches.sort();
        if patches.len() != n {
            return Err(format!("expected {n} patches and fetched {}", patches.len()));
        }

        // Put the seat's tree back to its commit, but only where the dirt is ours.
        let mut synced = self.read_manifest("synced.manifest");
        let seat = Manifest::of_dir(&self.tree)?;
        let mut reverted: Vec<String> = Vec::new();
        let mut stale: Vec<String> = Vec::new();
        let status = git(&self.tree, &["status", "--porcelain", "-z", "--untracked-files=all"])?;
        let dirt: Vec<(String, bool)> = status
            .split('\0')
            .filter(|l| l.len() > 3)
            .map(|l| (l[3..].to_string(), l.starts_with("??")))
            .collect();
        for (p, _) in &dirt {
            if seat.0.get(p) != synced.0.get(p) {
                stale.push(format!("{p} (seat {}, agreed {})", show(seat.0.get(p)), show(synced.0.get(p))));
            }
        }
        if !stale.is_empty() {
            return Err(format!("the seat changed files the agent's commits touch, which are not the copied-back ones: {}", stale.join("; ")));
        }
        let mut revert_failed: Option<String> = None;
        for (p, untracked) in &dirt {
            if *untracked {
                let _ = fs::remove_file(self.tree.join(p));
                prune_empty_parents(&self.tree, &self.tree.join(p));
            } else if let Err(e) = git(&self.tree, &["checkout", "HEAD", "--", p]) {
                revert_failed = Some(e);
                break;
            }
            reverted.push(p.clone());
        }
        // From here the seat no longer holds what `synced` says for these paths,
        // so `synced` is made to say what the seat holds *before* anything else can
        // fail. Then a failed `am` leaves the next `copy_back` seeing far differ
        // from synced with the seat still equal to it, and the files come again —
        // instead of a record that says "agreed" over a tree missing the work.
        let held = Manifest::of_dir(&self.tree)?;
        for p in &reverted {
            match held.0.get(p) {
                Some(e) => synced.0.insert(p.clone(), e.clone()),
                None => synced.0.remove(p),
            };
        }
        self.write_manifest("synced.manifest", &synced)?;
        if let Some(e) = revert_failed {
            return Err(format!("could not put the seat's tree back to its commit: {e}"));
        }

        let before = git(&self.tree, &["rev-parse", "HEAD"])?.trim().to_string();
        let mut am: Vec<String> = vec!["am".into(), "--quiet".into()];
        am.extend(patches.iter().map(|p| p.display().to_string()));
        let am_refs: Vec<&str> = am.iter().map(String::as_str).collect();
        if let Err(e) = git(&self.tree, &am_refs) {
            let _ = git(&self.tree, &["am", "--abort"]);
            return Err(format!("git am refused the agent's commits: {e}"));
        }
        let touched = git(&self.tree, &["diff", "--name-only", "-z", &before, "HEAD"])?;
        let now = Manifest::of_dir(&self.tree)?;
        for p in touched.split('\0').filter(|p| !p.is_empty()).map(str::to_string).chain(reverted) {
            match now.0.get(&p) {
                Some(e) => synced.0.insert(p, e.clone()),
                None => synced.0.remove(&p),
            };
        }
        self.write_manifest("synced.manifest", &synced)?;
        rec.applied_through = head;
        self.write_record(&rec)?;
        Ok(n)
    }

    /// The full sync-back before `verify` and `review`: the commits, then the
    /// files that are not in any.
    pub fn sync_back(&self) -> Result<(usize, Back), String> {
        let commits = if self.record()?.seat_clean { self.carry_commits()? } else { 0 };
        Ok((commits, self.copy_back()?))
    }

    // ---- the receipt ----------------------------------------------------

    /// The tree hash of the seat's tree as it stands.
    pub fn tree_hash(&self) -> Result<String, String> {
        Ok(Manifest::of_dir(&self.tree)?.tree_hash())
    }

    /// `verify` ran on this tree: write its hash down.
    pub fn note_verified(&self) -> Result<String, String> {
        let h = self.tree_hash()?;
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        fs::write(self.dir.join("verified"), format!("{h}\n")).map_err(|e| e.to_string())?;
        Ok(h)
    }
}

/// Set by the executor's `wsp` shim on every call it forwards.
pub const EXECUTOR_ENV: &str = "WSP_EXECUTOR";
/// Set by the shim on the tree verbs only (`docs/remote-agents.md` section 5).
pub const TREE_ENV: &str = "WSP_EXECUTOR_TREE";

/// The sentence for a verb an executor may not run. The shim refuses these
/// before it asks the seat; the seat says the same if one arrives anyway.
pub fn executor_refusal(verb: &str) -> Option<String> {
    matches!(verb, "checkout" | "land").then(|| {
        format!("`wsp {verb}` is not run from an executor — the seat made this row's worktree and a verifier lands it; finish with `wsp review`")
    })
}

/// A tree verb from an executor: bring the agent's work back, and refuse when
/// it cannot be brought back whole. A stale conflict names every path and
/// overwrites nothing, and the verb does not run.
pub fn sync_before(copy: &Copy) -> Result<(), String> {
    let (_, back) = copy.sync_back()?;
    if back.refused.is_empty() {
        return Ok(());
    }
    let paths: Vec<String> = back.refused.iter().map(Stale::words).collect();
    Err(format!(
        "{} cannot be synced back — the seat changed what the agent changed:\n  {}\nresolve those paths on the seat, then run the verb again",
        copy.row,
        paths.join("\n  ")
    ))
}

/// What the seat does with a verb the shim marked as an executor's: refuse
/// `checkout` and `land`; for the tree verbs, sync back and return the seat's
/// tree, which the verb then runs in. `None` for every other call.
pub fn from_executor(store: &Store, verb: &str) -> Option<Result<PathBuf, String>> {
    if std::env::var_os(EXECUTOR_ENV).is_none() {
        return None;
    }
    if let Some(why) = executor_refusal(verb) {
        return Some(Err(why));
    }
    if std::env::var_os(TREE_ENV).is_none() {
        return None;
    }
    Some((|| {
        let row = std::env::var("WSP_TASK").ok().filter(|s| !s.is_empty()).ok_or("`wsp` from an executor needs WSP_TASK to find the row's tree")?;
        let tree = crate::cmd_checkout::member_tree(store, &row).ok_or_else(|| format!("{row} has no worktree on the seat"))?;
        let rec = record_of(store, &row).ok_or_else(|| format!("{row} has not been copied to an executor"))?;
        let m = store
            .machines()
            .into_iter()
            .find(|m| m.ssh == rec.host)
            .ok_or_else(|| format!("no machine is reached as `{}`, where {row} was copied", rec.host))?;
        let remote = Remote::of_machine(store, &m, &m.exec_dir);
        sync_before(&Copy::new(store, remote, &row, &tree))?;
        Ok(tree)
    })())
}

fn prune_empty_parents(root: &Path, file: &Path) {
    let mut d = file.parent();
    while let Some(dir) = d {
        if dir == root || fs::remove_dir(dir).is_err() {
            break;
        }
        d = dir.parent();
    }
}

/// Record what `verify` ran on, for a row that has been copied. Quiet for every
/// other row, which is nearly all of them.
pub fn note_verified(store: &Store, row: &str, tree: &Path) -> Option<String> {
    let rec = record_of(store, row)?;
    let c = Copy::new(store, Remote { ssh: String::new(), host: rec.host, control: None, exec_dir: String::new(), tar: false }, row, tree);
    c.note_verified().ok()
}

/// `review`'s refusal: the tree being handed over is not the tree `verify` ran
/// on. `Ok` for a row that was never copied or never verified — the check is for
/// a mismatch, and it catches one whichever side drifted.
pub fn review_refusal(store: &Store, row: &str, tree: &Path) -> Result<(), String> {
    if record_of(store, row).is_none() {
        return Ok(());
    }
    let Ok(was) = fs::read_to_string(record_dir(store, row).join("verified")) else { return Ok(()) };
    let was = was.trim();
    let now = Manifest::of_dir(tree)?.tree_hash();
    if was == now {
        Ok(())
    } else {
        Err(format!(
            "{row}'s tree is not the one `wsp verify` ran on (verified {}, tree now {}) — verify again, then review",
            &was[..was.len().min(12)],
            &now[..12]
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that goes when the test does.
    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Dir {
            let p = std::env::temp_dir().join(format!("wsp-treecopy-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()).replace(['(', ')', ' '], ""));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The stand-in: drops ssh's options and the host, joins what is left and
    /// hands it to a shell — which is all the real one does with a command.
    fn stand_in(dir: &Path) -> String {
        let p = dir.join("fake-ssh");
        fs::write(
            &p,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -S|-o) shift 2;; -*) shift;; *) break;; esac; done\nshift\nexec sh -c \"$*\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p.display().to_string()
    }

    fn run(dir: &Path, cmd: &str) -> String {
        let out = Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{cmd}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    struct Rig {
        _root: Dir,
        seat: PathBuf,
        far_root: PathBuf,
        copy: Copy,
    }

    fn rig(tag: &str, tar: bool) -> Rig {
        let root = Dir::new(tag);
        let seat = root.0.join("seat");
        let far_root = root.0.join("far");
        fs::create_dir_all(&seat).unwrap();
        fs::create_dir_all(&far_root).unwrap();
        run(&seat, "git init -q -b master . && git config user.name Seat && git config user.email s@x");
        run(&seat, "mkdir -p src sub/deep target/debug node_modules/x .venv logs && echo one > src/a.rs && echo two > src/b.rs && echo deep > sub/deep/c.txt && printf '#!/bin/sh\\n' > run.sh && chmod +x run.sh && ln -s src/a.rs link && echo junk > target/debug/out && echo n > node_modules/x/i.js && echo v > .venv/v && echo l > build.log && echo 'target/' > .gitignore");
        run(&seat, "git add -A && git commit -q -m base && git branch trunk-ahead && echo more > src/more.rs && git add -A && git commit -q -m ahead && git branch trunk && git reset -q --hard HEAD~1");
        let state = root.0.join("state");
        fs::create_dir_all(&state).unwrap();
        let remote = Remote {
            ssh: stand_in(&root.0),
            host: "far".into(),
            control: None,
            exec_dir: far_root.display().to_string(),
            tar,
        };
        let copy = Copy { remote, row: "agent-9".into(), tree: seat.clone(), dir: state.join("trees").join("agent-9") };
        Rig { _root: root, seat, far_root, copy }
    }

    impl Rig {
        fn far(&self) -> PathBuf {
            self.far_root.join("trees").join("agent-9")
        }
    }

    #[test]
    fn sha256_agrees_with_the_system_tool() {
        for data in ["", "abc", &"x".repeat(55), &"y".repeat(56), &"z".repeat(64), &"q".repeat(1000)] {
            let d = Dir::new("sha");
            let f = d.0.join("f");
            fs::write(&f, data).unwrap();
            let theirs = run(&d.0, "shasum -a 256 f | cut -d' ' -f1").trim().to_string();
            assert_eq!(sha256_hex(data.as_bytes()), theirs, "{} bytes", data.len());
        }
    }

    /// The three spellings of what is left out agree: the seat's walk, the far
    /// script's `find`, and the transport's flags.
    #[test]
    fn the_far_manifest_is_the_seats_by_construction() {
        let r = rig("manifest", false);
        let here = Manifest::of_dir(&r.seat).unwrap();
        let paths: Vec<&str> = here.0.keys().map(String::as_str).collect();
        assert_eq!(paths, [".gitignore", "link", "run.sh", "src/a.rs", "src/b.rs", "sub/deep/c.txt"], "target, node_modules, .venv, logs and .git are out");
        assert_eq!(here.0["run.sh"].mode, "755");
        assert_eq!(here.0["link"].mode, "120000");
        let far = r.copy.remote.manifest(&r.seat.display().to_string()).unwrap();
        assert_eq!(here, far, "one rule, two implementations");
        assert_eq!(here.tree_hash(), far.tree_hash());
    }

    #[test]
    fn copy_up_matches_by_hash_and_records_where_it_stands() {
        for tar in [false, true] {
            let r = rig(if tar { "up-tar" } else { "up-rsync" }, tar);
            let rec = r.copy.copy_up("trunk").unwrap();
            assert_eq!(rec.trunk_ahead, 1, "a number, not silence");
            assert_eq!(rec.seat_head, run(&r.seat, "git rev-parse HEAD").trim());
            assert!(rec.seat_clean);
            let far = r.far();
            assert!(far.join("src/a.rs").exists());
            assert!(!far.join("target").exists() && !far.join("node_modules").exists() && !far.join(".venv").exists() && !far.join("build.log").exists());
            assert!(far.join(".git").is_dir(), "a baseline repository of its own");
            assert_eq!(run(&far, "git log --format=%s").trim(), format!("baseline: agent-9 as copied from seat {}", rec.seat_head));
            let base = Manifest::of_dir(&r.seat).unwrap();
            assert_eq!(rec.base_tree, base.tree_hash());
            let far_base = fs::read_to_string(r.far_root.join("state/agent-9/base.manifest")).unwrap();
            assert_eq!(far_base, base.render(), "the manifest on both sides");
            assert_eq!(fs::read_to_string(r.copy.dir.join("base.manifest")).unwrap(), far_base);
            assert_eq!(CopyRecord::from_json(&serde_json::from_str(&fs::read_to_string(r.copy.dir.join("copy.json")).unwrap()).unwrap()).unwrap(), rec);
            assert!(rec.line().contains("1 commit ahead"));

            let again = r.copy.copy_up("trunk").unwrap_err();
            assert!(again.contains("already copied up"), "{again}");
        }
    }

    #[test]
    fn copy_up_refuses_a_far_directory_that_is_not_empty() {
        let r = rig("busy", false);
        fs::create_dir_all(r.far()).unwrap();
        fs::write(r.far().join("precious"), "x").unwrap();
        let e = r.copy.copy_up("trunk").unwrap_err();
        assert!(e.contains("already holds something"), "{e}");
        assert!(r.far().join("precious").exists(), "and nothing was written over it");
        assert!(!r.copy.dir.join("copy.json").exists());
    }

    #[test]
    fn a_trunk_that_does_not_resolve_is_an_error_and_not_a_zero() {
        let r = rig("notrunk", false);
        let e = r.copy.copy_up("no-such-branch").unwrap_err();
        assert!(e.contains("cannot say how far"), "{e}");
        assert!(!r.far().exists());
    }

    #[test]
    fn copy_back_brings_only_what_differs_and_deletions_and_then_nothing() {
        for tar in [false, true] {
            let r = rig(if tar { "back-tar" } else { "back-rsync" }, tar);
            r.copy.copy_up("trunk").unwrap();
            let far = r.far();
            run(&far, "echo edited > src/a.rs && rm src/b.rs && mkdir -p new/dir && echo n > new/dir/f && chmod +x sub/deep/c.txt && mkdir -p target && echo more > target/ignored && echo x > x.log");
            let b = r.copy.copy_back().unwrap();
            assert_eq!(b.copied, ["new/dir/f", "src/a.rs", "sub/deep/c.txt"]);
            assert_eq!(b.deleted, ["src/b.rs"]);
            assert!(b.refused.is_empty());
            assert_eq!(fs::read_to_string(r.seat.join("src/a.rs")).unwrap(), "edited\n");
            assert!(!r.seat.join("src/b.rs").exists());
            assert!(!r.seat.join("target/ignored").exists() && !r.seat.join("x.log").exists(), "the left-out set never comes back");
            assert_eq!(Manifest::of_dir(&r.seat).unwrap(), r.copy.remote.manifest(&far.display().to_string()).unwrap());

            let again = r.copy.copy_back().unwrap();
            assert_eq!(again, Back::default(), "a second run sends nothing");

            // A further edit of a file already copied back is not a conflict.
            run(&far, "echo again > src/a.rs");
            let third = r.copy.copy_back().unwrap();
            assert_eq!(third.copied, ["src/a.rs"]);
            assert!(third.refused.is_empty(), "{third:?}");

            // A revert to the base is copied back too.
            run(&far, "echo one > src/a.rs");
            assert_eq!(r.copy.copy_back().unwrap().copied, ["src/a.rs"]);
        }
    }

    #[test]
    fn a_path_the_seat_changed_is_refused_by_name_and_overwritten_by_nothing() {
        let r = rig("stale", false);
        r.copy.copy_up("trunk").unwrap();
        let far = r.far();
        run(&far, "echo agent > src/a.rs && echo agent-b > src/b.rs && rm sub/deep/c.txt");
        run(&r.seat, "echo person > src/a.rs && echo person > sub/deep/c.txt");
        let b = r.copy.copy_back().unwrap();
        assert_eq!(b.copied, ["src/b.rs"], "the rest still comes");
        let named: Vec<&str> = b.refused.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(named, ["src/a.rs", "sub/deep/c.txt"]);
        assert_eq!(fs::read_to_string(r.seat.join("src/a.rs")).unwrap(), "person\n", "never overwritten");
        assert_eq!(fs::read_to_string(r.seat.join("sub/deep/c.txt")).unwrap(), "person\n", "nor deleted");
        let w = b.refused[0].words();
        assert!(w.contains("src/a.rs") && w.contains("644") && w.contains("not overwritten"), "{w}");
        assert!(b.refused[1].far == "absent");

        // Still refused next time: the disagreement is not worn away by asking.
        assert_eq!(r.copy.copy_back().unwrap().refused.len(), 2);
    }

    /// The seat reaching the same content the agent did is agreement, not a
    /// conflict.
    #[test]
    fn the_seat_holding_the_far_version_already_is_agreement() {
        let r = rig("agree", false);
        r.copy.copy_up("trunk").unwrap();
        run(&r.far(), "echo same > src/a.rs");
        run(&r.seat, "echo same > src/a.rs");
        let b = r.copy.copy_back().unwrap();
        assert_eq!(b, Back::default());
    }

    #[test]
    fn the_agents_commits_come_back_as_patches_with_their_authors_and_messages() {
        for tar in [false, true] {
            let r = rig(if tar { "am-tar" } else { "am-rsync" }, tar);
            r.copy.copy_up("trunk").unwrap();
            let far = r.far();
            run(&far, "git config user.name Agent && git config user.email a@far && echo c1 > src/a.rs && git commit -qam 'first change' && echo c2 > src/new.rs && git add -A && git commit -qm 'second change' && echo uncommitted > src/b.rs");
            // A checkpoint ran first and dirtied the seat's tree with the same files.
            let cp = r.copy.copy_back().unwrap();
            assert_eq!(cp.copied.len(), 3);

            let (n, back) = r.copy.sync_back().unwrap();
            assert_eq!(n, 2);
            assert_eq!(back.copied, ["src/b.rs"], "the commits' files were agreed by the patches; only the uncommitted one is copied");
            assert!(back.refused.is_empty() && back.deleted.is_empty());
            let log = run(&r.seat, "git log --format='%an|%s' -3");
            assert_eq!(log.lines().take(2).collect::<Vec<_>>(), ["Agent|second change", "Agent|first change"]);
            assert_eq!(fs::read_to_string(r.seat.join("src/b.rs")).unwrap(), "uncommitted\n", "and the uncommitted file is still there");
            assert!(run(&r.seat, "git status --porcelain").contains("src/b.rs"), "uncommitted on the seat, as it is on the far side");
            assert_eq!(Manifest::of_dir(&r.seat).unwrap(), r.copy.remote.manifest(&far.display().to_string()).unwrap());

            // Nothing more to carry; then one more commit carries alone.
            assert_eq!(r.copy.carry_commits().unwrap(), 0);
            run(&far, "git commit -qam 'third change'");
            let (n, _) = r.copy.sync_back().unwrap();
            assert_eq!(n, 1);
            assert_eq!(run(&r.seat, "git log --format=%s -1").trim(), "third change");
        }
    }

    /// A failed `git am` must not leave the seat missing the agent's files with a
    /// record that says they are agreed: the next copy-back brings them again.
    #[test]
    fn a_failed_am_leaves_the_files_to_come_back() {
        let r = rig("am-fail", false);
        r.copy.copy_up("trunk").unwrap();
        run(&r.far(), "git config user.name A && git config user.email a@f && echo agent-a > src/a.rs && git commit -qam c && echo agent-b > src/b.rs");
        r.copy.copy_back().unwrap();
        // The seat's HEAD moves under the checkpoint: a.rs is committed as the
        // checkpoint left it, so the agent's patch no longer applies.
        run(&r.seat, "git commit -q -m moved -- src/a.rs");
        let e = r.copy.sync_back().unwrap_err();
        assert!(e.contains("git am refused"), "{e}");
        let b = r.copy.copy_back().unwrap();
        assert_eq!(b.copied, ["src/b.rs"], "{b:?}");
        assert_eq!(fs::read_to_string(r.seat.join("src/b.rs")).unwrap(), "agent-b\n");
    }

    #[test]
    fn commits_are_not_carried_over_a_tree_that_was_dirty_at_copy_up() {
        let r = rig("dirty", false);
        run(&r.seat, "echo wip > src/a.rs");
        let rec = r.copy.copy_up("trunk").unwrap();
        assert!(!rec.seat_clean);
        let e = r.copy.carry_commits().unwrap_err();
        assert!(e.contains("uncommitted work"), "{e}");
        run(&r.far(), "echo agent > src/b.rs");
        let (n, back) = r.copy.sync_back().unwrap();
        assert_eq!((n, back.copied), (0, vec!["src/b.rs".to_string()]), "the files still come");
    }

    #[test]
    fn carrying_commits_refuses_to_touch_a_path_the_seat_changed() {
        let r = rig("am-stale", false);
        r.copy.copy_up("trunk").unwrap();
        run(&r.far(), "git config user.name A && git config user.email a@f && echo agent > src/a.rs && git commit -qam c");
        run(&r.seat, "echo person > src/a.rs");
        let e = r.copy.carry_commits().unwrap_err();
        assert!(e.contains("src/a.rs"), "{e}");
        assert_eq!(fs::read_to_string(r.seat.join("src/a.rs")).unwrap(), "person\n");
    }

    #[test]
    fn review_refuses_a_tree_that_is_not_the_one_verify_ran_on() {
        let r = rig("receipt", false);
        let store = crate::store::Store::at(r._root.0.join("store"), r._root.0.join("storestate"));
        // Not copied: nothing to refuse.
        assert_eq!(review_refusal(&store, "agent-9", &r.seat), Ok(()));
        let copy = Copy::new(&store, r.copy.remote.clone(), "agent-9", &r.seat);
        copy.copy_up("trunk").unwrap();
        assert_eq!(review_refusal(&store, "agent-9", &r.seat), Ok(()), "copied but not yet verified");
        assert!(note_verified(&store, "agent-9", &r.seat).is_some());
        assert_eq!(review_refusal(&store, "agent-9", &r.seat), Ok(()), "the tree verify ran on");
        run(&r.seat, "echo drift >> src/a.rs");
        let e = review_refusal(&store, "agent-9", &r.seat).unwrap_err();
        assert!(e.contains("not the one `wsp verify` ran on"), "{e}");
        assert_eq!(note_verified(&store, "agent-8", &r.seat), None, "a row never copied records nothing");
    }

    #[test]
    fn a_tree_verb_syncs_back_before_it_runs() {
        let r = rig("verb-sync", false);
        r.copy.copy_up("trunk").unwrap();
        run(&r.far(), "echo agent > src/a.rs");
        assert_eq!(fs::read_to_string(r.seat.join("src/a.rs")).unwrap(), "one\n");
        sync_before(&r.copy).unwrap();
        assert_eq!(fs::read_to_string(r.seat.join("src/a.rs")).unwrap(), "agent\n", "the agent's work is on the seat before the verb");
    }

    #[test]
    fn a_tree_verb_is_refused_on_a_stale_conflict_and_nothing_is_overwritten() {
        let r = rig("verb-stale", false);
        r.copy.copy_up("trunk").unwrap();
        run(&r.far(), "echo agent > src/a.rs && echo agent-b > src/b.rs");
        run(&r.seat, "echo person > src/a.rs");
        let e = sync_before(&r.copy).unwrap_err();
        assert!(e.contains("src/a.rs") && e.contains("cannot be synced back"), "{e}");
        assert!(!e.contains("src/b.rs"), "only the conflict is named: {e}");
        assert_eq!(fs::read_to_string(r.seat.join("src/a.rs")).unwrap(), "person\n");
    }

    #[test]
    fn checkout_and_land_are_refused_from_an_executor_and_the_rest_are_not() {
        for v in ["checkout", "land"] {
            let w = executor_refusal(v).unwrap();
            assert!(w.contains(v) && w.contains("not run from an executor"), "{w}");
        }
        for v in ["verify", "review", "commit-help", "say", "note", "lease"] {
            assert_eq!(executor_refusal(v), None, "{v}");
        }
    }

    // ---- the shim itself, with a stand-in ssh -----------------------------

    /// The shim run with `ssh` replaced by `ssh_body` (a shell script body that
    /// sees the ssh arguments), returning (status, stdout, stderr, link file).
    fn shim(tag: &str, ssh_body: &str, args: &[&str]) -> (i32, String, String, PathBuf, Dir) {
        let d = Dir::new(tag);
        let bin = d.0.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let ssh = bin.join("ssh");
        fs::write(&ssh, format!("#!/bin/sh\n{ssh_body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        let link = d.0.join("link-down");
        let shim = Path::new(env!("CARGO_MANIFEST_DIR")).join("executor/wsp");
        let out = Command::new("sh")
            .arg(shim)
            .args(args)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("WSP_SEAT", "seat")
            .env("WSP_SEAT_ID", "cpd-3")
            .env("WSP_LINK_FILE", &link)
            .env("ARGS_OUT", d.0.join("args"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            link,
            d,
        )
    }

    #[test]
    fn an_unreachable_seat_is_exit_75_with_the_line() {
        let (code, _, err, link, _d) = shim("shim-down", "echo 'ssh: connect to host seat port 22: Operation timed out' >&2; exit 255", &["say", "hi"]);
        assert_eq!(code, 75, "{err}");
        assert!(err.contains("wsp: the seat is unreachable (link down since "), "{err}");
        assert!(err.contains(')'), "{err}");
        let since = fs::read_to_string(&link).unwrap();
        assert!(err.contains(since.trim()), "the time is the one recorded: {err} / {since}");
    }

    #[test]
    fn the_time_is_the_first_failure_and_a_success_clears_it() {
        let (_, _, _, link, d) = shim("shim-since", "exit 255", &["ls"]);
        fs::write(&link, "03:04:05\n").unwrap();
        let (code, _, err, _, _d2) = {
            // Same link file again: the earlier time stands.
            let shim = Path::new(env!("CARGO_MANIFEST_DIR")).join("executor/wsp");
            let out = Command::new("sh")
                .arg(shim)
                .arg("ls")
                .env_clear()
                .env("PATH", format!("{}:/usr/bin:/bin", d.0.join("bin").display()))
                .env("WSP_SEAT", "seat")
                .env("WSP_LINK_FILE", &link)
                .output()
                .unwrap();
            (out.status.code().unwrap_or(-1), String::new(), String::from_utf8_lossy(&out.stderr).into_owned(), (), ())
        };
        assert_eq!(code, 75);
        assert!(err.contains("since 03:04:05"), "{err}");
        // The link comes back: the file goes.
        fs::write(d.0.join("bin/ssh"), "#!/bin/sh\nexit 0\n").unwrap();
        let shim = Path::new(env!("CARGO_MANIFEST_DIR")).join("executor/wsp");
        let ok = Command::new("sh")
            .arg(shim)
            .arg("ls")
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", d.0.join("bin").display()))
            .env("WSP_SEAT", "seat")
            .env("WSP_LINK_FILE", &link)
            .status()
            .unwrap();
        assert_eq!(ok.code(), Some(0));
        assert!(!link.exists());
    }

    #[test]
    fn a_verb_that_ran_and_failed_keeps_its_own_status() {
        for want in [1, 2, 3, 74, 76] {
            let (code, out, err, link, _d) = shim(&format!("shim-ran-{want}"), &format!("echo 'wsp: no such row'; echo refused >&2; exit {want}"), &["show", "x"]);
            assert_eq!(code, want, "{err}");
            assert!(out.contains("no such row") && err.contains("refused"));
            assert!(!err.contains("unreachable"), "{err}");
            assert!(!link.exists());
        }
        let (code, ..) = shim("shim-ok", "exit 0", &["ls"]);
        assert_eq!(code, 0);
    }

    #[test]
    fn the_shim_marks_tree_verbs_and_only_those_and_refuses_checkout_and_land() {
        let record = "printf '%s\\n' \"$*\" > \"$ARGS_OUT\"";
        for v in ["verify", "review", "commit-help"] {
            let (code, _, err, _, d) = shim(&format!("shim-tree-{v}"), record, &[v, "--json"]);
            assert_eq!(code, 0, "{err}");
            let sent = fs::read_to_string(d.0.join("args")).unwrap();
            assert!(sent.contains("WSP_EXECUTOR_TREE=1") && sent.contains("WSP_EXECUTOR=1"), "{sent}");
            assert!(sent.contains("WSP_SEAT_ID='cpd-3'"), "{sent}");
        }
        let (_, _, _, _, d) = shim("shim-not-tree", record, &["say", "hello"]);
        let sent = fs::read_to_string(d.0.join("args")).unwrap();
        assert!(sent.contains("WSP_EXECUTOR=1") && !sent.contains("WSP_EXECUTOR_TREE"), "{sent}");
        for v in ["checkout", "land"] {
            let (code, _, err, _, d) = shim(&format!("shim-no-{v}"), record, &[v, "agent-9"]);
            assert_eq!(code, 2, "{err}");
            assert!(err.contains("is not run from an executor"), "{err}");
            assert!(!d.0.join("args").exists(), "the seat was never asked");
        }
    }
}
