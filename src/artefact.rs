//! `wsp artefact put | versions | restore` — an asset is iterated in place, and
//! every earlier version stays reachable (`wsp-376`).
//!
//! Ed, 2026-11-05: "being able to iterate on an asset without making a copy
//! with a new name or replacing the original". Both used to happen:
//! `report-v2.html` beside `report.html`, listed by compound's tab as two
//! artefacts, or the file overwritten and the earlier one gone.
//!
//! **Layout.** The current version stays where every reader already looks,
//! `<root>/<owner>/<file>`, so links, refs and compound's tab are unchanged.
//! Earlier versions live beside it, out of sight:
//!
//! ```text
//! <root>/<owner>/report.html                    the current version
//! <root>/<owner>/.versions/report.html/1.html   an earlier one
//! <root>/<owner>/.versions/report.html/index.jsonl   one line per earlier one
//! <root>/<owner>/.versions/report.html/current.json  what put last wrote
//! ```
//!
//! The directory is named for the whole file name, extension included, so
//! `report.html` and `report.md` never share a numbering. compound's reader
//! lists only regular files whose names do not begin with a dot, and only row
//! directories (`<word>-<digits>`), so `.versions` is never an artefact there.
//!
//! **Catching a plain overwrite.** `current.json` records the hash of the
//! file `put` last wrote. The next `put` hashes the file again: a match means
//! the current file is exactly version `n` and moves into `.versions` under
//! that number; a mismatch means somebody `cp`'d over it, so what is there is
//! saved as a version of its own (noted as changed outside `put`) before the
//! new one is written. The bytes the overwrite destroyed are gone, but what
//! replaced them is not.
//!
//! The root is `$WSP_ARTEFACTS` or `~/.compound/artefacts`; the pure functions
//! below take it explicitly, so a test never touches the real one.

use crate::util;
use crate::Args;
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage:
  wsp artefact put <owner> <file> [--as <name>] [--note \"…\"|-]
  wsp artefact versions <owner>/<name> [--json]
  wsp artefact restore <owner>/<name> <n> [--note \"…\"|-]";

/// What the brief says to an agent that writes an asset. One sentence, no
/// newline, because the brief draws it as a single row.
pub const BRIEF_LINE: &str = "an asset you write for the person goes in with `wsp artefact put <row> FILE [--note \"…\"]`, \
     and a new version of one is another `put` of the same name, never a new name (`-v2`) and never a \
     copy over the file by hand: `wsp artefact versions <row>/<name>` shows every earlier one";

/// Who wrote a version.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Who {
    pub row: Option<String>,
    pub seat: Option<String>,
}

impl Who {
    fn here() -> Who {
        let row = std::env::var("WSP_TASK").ok().filter(|s| !s.is_empty());
        Who { row, seat: crate::cmd_agent::my_pane() }
    }
    fn json(&self) -> Value {
        json!({ "row": self.row, "seat": self.seat })
    }
    fn of(v: &Value) -> Who {
        let s = |k: &str| v.get("by").and_then(|b| b.get(k)).and_then(Value::as_str).map(String::from);
        Who { row: s("row"), seat: s("seat") }
    }
    fn show(&self) -> String {
        match (&self.row, &self.seat) {
            (Some(r), Some(s)) => format!("{r}/{s}"),
            (Some(r), None) => r.clone(),
            (None, Some(s)) => s.clone(),
            (None, None) => "unknown".into(),
        }
    }
}

/// One version, current or earlier.
#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    pub n: u64,
    pub time: String,
    pub who: Who,
    pub note: String,
    pub hash: String,
    pub bytes: u64,
    pub current: bool,
    /// Current only: the file no longer matches what `put` wrote.
    pub edited: bool,
    pub path: PathBuf,
}

impl Version {
    fn from_record(v: &Value, path: PathBuf, current: bool) -> Version {
        Version {
            n: v.get("n").and_then(Value::as_u64).unwrap_or(0),
            time: v.get("time").and_then(Value::as_str).unwrap_or("").into(),
            who: Who::of(v),
            note: v.get("note").and_then(Value::as_str).unwrap_or("").into(),
            hash: v.get("hash").and_then(Value::as_str).unwrap_or("").into(),
            bytes: v.get("bytes").and_then(Value::as_u64).unwrap_or(0),
            current,
            edited: false,
            path,
        }
    }
    fn json(&self) -> Value {
        json!({
            "n": self.n, "time": self.time, "by": self.who.json(), "note": self.note,
            "hash": self.hash, "bytes": self.bytes, "current": self.current,
            "edited_outside_put": self.edited, "path": self.path.display().to_string(),
        })
    }
}

/// What a `put` or `restore` did.
#[derive(Debug, PartialEq)]
pub struct Put {
    pub n: u64,
    pub path: PathBuf,
    /// Versions kept beside the current one, after this.
    pub kept: usize,
    /// The current file had been changed by hand and was saved as this version.
    pub rescued: Option<u64>,
    /// The content was already the current version; nothing was written.
    pub unchanged: bool,
}

/// FNV-1a over the bytes, 64 bits, as 16 hex digits. Change detection, not
/// security: the question is only whether the file is the one `put` wrote.
pub fn hash_of(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// `$WSP_ARTEFACTS`, else `~/.compound/artefacts`.
pub fn root() -> PathBuf {
    match std::env::var_os("WSP_ARTEFACTS").filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => util::home().join(".compound/artefacts"),
    }
}

fn plain(what: &str, s: &str) -> Result<(), String> {
    if s.is_empty() || s.contains(['/', '\0']) || s.starts_with('.') {
        return Err(format!("`{s}` is not a usable {what}: it must be one plain word, with no `/` and no leading `.`"));
    }
    Ok(())
}

fn ext_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(d) if d > 0 => &name[d..],
        _ => "",
    }
}

fn vdir(root: &Path, owner: &str, file: &str) -> PathBuf {
    root.join(owner).join(".versions").join(file)
}

fn version_file(dir: &Path, n: u64, file: &str) -> PathBuf {
    dir.join(format!("{n}{}", ext_of(file)))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn index_of(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("index.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// A lock on one artefact's version directory, so two seats putting at once
/// each see the other's result. Stale after a minute.
struct Lock(PathBuf);

impl Lock {
    fn take(dir: &Path) -> Result<Lock, String> {
        let path = dir.join(".lock");
        for _ in 0..100 {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Lock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let old = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|d| d.as_secs() > 60);
                    if old {
                        let _ = fs::remove_file(&path);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(format!("cannot lock {}: {e}", dir.display())),
            }
        }
        Err(format!("{} is locked by another put", dir.display()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn record(v: &Version) -> Value {
    json!({ "n": v.n, "time": v.time, "by": v.who.json(), "note": v.note, "hash": v.hash, "bytes": v.bytes })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_file_name(format!(".{}.{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("x"), std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })
}

/// Write `bytes` as the new current version of `<owner>/<file>`, keeping the
/// one it replaces.
pub fn put_bytes(root: &Path, owner: &str, file: &str, bytes: &[u8], note: &str, who: &Who, in_place: bool) -> Result<Put, String> {
    plain("owner", owner)?;
    plain("name", file)?;
    let cur = root.join(owner).join(file);
    let dir = vdir(root, owner, file);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let _lock = Lock::take(&dir)?;

    let hash = hash_of(bytes);
    let rec = read_json(&dir.join("current.json"));
    let index = index_of(&dir);
    let top = index
        .iter()
        .filter_map(|v| v.get("n").and_then(Value::as_u64))
        .chain(rec.as_ref().and_then(|r| r.get("n").and_then(Value::as_u64)))
        .max()
        .unwrap_or(0);
    let mut next = top + 1;
    let mut rescued = None;

    if let Ok(old) = fs::read(&cur) {
        let old_hash = hash_of(&old);
        let recorded = rec.as_ref().filter(|r| r.get("hash").and_then(Value::as_str) == Some(old_hash.as_str()));
        if recorded.is_some() && old_hash == hash {
            let n = recorded.and_then(|r| r.get("n")).and_then(Value::as_u64).unwrap_or(top);
            return Ok(Put { n, path: cur, kept: index.len(), rescued: None, unchanged: true });
        }
        // The file `put` wrote is archived under its own number; anything else
        // is somebody's `cp` and becomes a version of its own.
        let archived = match recorded {
            Some(r) => Version::from_record(r, PathBuf::new(), false),
            None => {
                let first = rec.is_none() && index.is_empty();
                rescued = Some(next);
                let n = next;
                next += 1;
                Version {
                    n,
                    time: fs::metadata(&cur)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| util::iso_at(d.as_secs() as i64))
                        .unwrap_or_else(util::now_iso),
                    who: Who::default(),
                    note: match first {
                        true => "there before the first put".into(),
                        false => "changed outside put (copied or edited over the current file)".into(),
                    },
                    hash: old_hash,
                    bytes: old.len() as u64,
                    current: false,
                    edited: false,
                    path: PathBuf::new(),
                }
            }
        };
        if in_place && recorded.is_none() {
            // `put` of the current file onto itself after editing it in place:
            // there is nothing earlier to keep, only a new version to record.
            rescued = None;
            next -= 1;
        } else {
            fs::write(version_file(&dir, archived.n, file), &old).map_err(|e| format!("cannot keep version {}: {e}", archived.n))?;
            let line = format!("{}\n", record(&archived));
            use std::io::Write;
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("index.jsonl"))
                .and_then(|mut f| f.write_all(line.as_bytes()))
                .map_err(|e| format!("cannot index version {}: {e}", archived.n))?;
        }
    }

    if !in_place {
        fs::create_dir_all(root.join(owner)).map_err(|e| format!("cannot create {}: {e}", root.join(owner).display()))?;
        write_atomic(&cur, bytes)?;
    }
    let now = Version {
        n: next,
        time: util::now_iso(),
        who: who.clone(),
        note: note.to_string(),
        hash,
        bytes: bytes.len() as u64,
        current: true,
        edited: false,
        path: cur.clone(),
    };
    write_atomic(&dir.join("current.json"), format!("{}\n", record(&now)).as_bytes())?;
    Ok(Put { n: next, path: cur, kept: index_of(&dir).len(), rescued, unchanged: false })
}

/// `put` from a file on disk. `name` defaults to the file's own name; a `name`
/// with no extension takes the file's.
pub fn put(root: &Path, owner: &str, src: &Path, name: Option<&str>, note: &str, who: &Who) -> Result<Put, String> {
    let bytes = fs::read(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    let base = src.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let file = match name {
        Some(n) if ext_of(n).is_empty() => format!("{n}{}", ext_of(base)),
        Some(n) => n.to_string(),
        None => base.to_string(),
    };
    plain("name", &file)?;
    let cur = root.join(owner).join(&file);
    let same = match (fs::canonicalize(src), fs::canonicalize(&cur)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    put_bytes(root, owner, &file, &bytes, note, who, same)
}

/// `<owner>/<name>` as its file name: the name as given if that file or its
/// versions exist, else the one file whose name without extension matches.
pub fn resolve(root: &Path, spec: &str) -> Result<(String, String), String> {
    let Some((owner, name)) = spec.split_once('/') else {
        return Err(format!("`{spec}` is not <owner>/<name>"));
    };
    plain("owner", owner)?;
    plain("name", name)?;
    let known = |f: &str| root.join(owner).join(f).is_file() || vdir(root, owner, f).is_dir();
    if known(name) {
        return Ok((owner.into(), name.into()));
    }
    let mut hits: Vec<String> = Vec::new();
    for dir in [root.join(owner), root.join(owner).join(".versions")] {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let f = e.file_name().to_string_lossy().into_owned();
            let stem = f.strip_suffix(ext_of(&f)).unwrap_or(&f);
            if !f.starts_with('.') && stem == name && !hits.contains(&f) && known(&f) {
                hits.push(f);
            }
        }
    }
    match hits.len() {
        1 => Ok((owner.into(), hits.remove(0))),
        0 => Err(format!("no artefact `{name}` under {owner} in {}", root.display())),
        _ => Err(format!("`{name}` is ambiguous under {owner}: {}", hits.join(", "))),
    }
}

/// Every version, oldest first, the current one last.
pub fn versions(root: &Path, owner: &str, file: &str) -> Vec<Version> {
    let dir = vdir(root, owner, file);
    let mut out: Vec<Version> = index_of(&dir)
        .iter()
        .map(|r| {
            let n = r.get("n").and_then(Value::as_u64).unwrap_or(0);
            Version::from_record(r, version_file(&dir, n, file), false)
        })
        .collect();
    out.sort_by_key(|v| v.n);
    let cur = root.join(owner).join(file);
    if let Ok(bytes) = fs::read(&cur) {
        let h = hash_of(&bytes);
        let rec = read_json(&dir.join("current.json"));
        let mut v = match &rec {
            Some(r) => Version::from_record(r, cur.clone(), true),
            None => Version::from_record(&json!({}), cur.clone(), true),
        };
        v.edited = rec.as_ref().and_then(|r| r.get("hash")).and_then(Value::as_str) != Some(h.as_str());
        if v.edited {
            v.hash = h;
            v.bytes = bytes.len() as u64;
        }
        out.push(v);
    }
    out
}

/// Make version `n` current, as a new version.
pub fn restore(root: &Path, owner: &str, file: &str, n: u64, note: &str, who: &Who) -> Result<Put, String> {
    let all = versions(root, owner, file);
    let Some(v) = all.iter().find(|v| v.n == n) else {
        let have: Vec<String> = all.iter().map(|v| v.n.to_string()).collect();
        return Err(format!("{owner}/{file} has no version {n} (it has {})", have.join(", ")));
    };
    if v.current && !v.edited {
        return Err(format!("version {n} of {owner}/{file} is already the current one"));
    }
    let bytes = fs::read(&v.path).map_err(|e| format!("cannot read version {n}: {e}"))?;
    let note = match note.is_empty() {
        true => format!("restored from version {n}"),
        false => format!("restored from version {n}: {note}"),
    };
    put_bytes(root, owner, file, &bytes, &note, who, false)
}

fn note_of(args: &Args) -> Result<String, String> {
    match args.get("note") {
        Some(n) if n == "-" => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map_err(|e| format!("cannot read stdin: {e}"))?;
            Ok(s.split_whitespace().collect::<Vec<_>>().join(" "))
        }
        Some(n) => Ok(n.split_whitespace().collect::<Vec<_>>().join(" ")),
        None => Ok(String::new()),
    }
}

fn say_put(what: &str, file: &str, owner: &str, p: &Put) {
    if p.unchanged {
        println!("{owner}/{file} is already version {}: nothing written", p.n);
        return;
    }
    println!("{what} {owner}/{file}: version {} is current, {} earlier kept in .versions", p.n, p.kept);
    if let Some(n) = p.rescued {
        println!("the file had been changed outside `wsp artefact put`; what was there is saved as version {n}");
    }
    println!("{}", p.path.display());
}

pub fn artefact(_store: &crate::store::Store, args: &Args) -> i32 {
    let root = root();
    let result = match args.rest.first().map(String::as_str) {
        Some("put") => cmd_put(&root, args),
        Some("versions") | Some("ls") => cmd_versions(&root, args),
        Some("restore") => cmd_restore(&root, args),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("wsp artefact: {e}");
            2
        }
    }
}

fn cmd_put(root: &Path, args: &Args) -> Result<(), String> {
    let (Some(owner), Some(file)) = (args.rest.get(1), args.rest.get(2)) else { return Err(USAGE.into()) };
    let note = note_of(args)?;
    let name = args.get("as");
    let p = put(root, owner, Path::new(file), name.as_deref(), &note, &Who::here())?;
    let shown = p.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    say_put("put", &shown, owner, &p);
    Ok(())
}

fn cmd_versions(root: &Path, args: &Args) -> Result<(), String> {
    let Some(spec) = args.rest.get(1) else { return Err(USAGE.into()) };
    let (owner, file) = resolve(root, spec)?;
    let all = versions(root, &owner, &file);
    if args.json() {
        let list: Vec<Value> = all.iter().map(Version::json).collect();
        println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
        return Ok(());
    }
    if all.is_empty() {
        println!("{owner}/{file}: no versions");
        return Ok(());
    }
    for v in &all {
        let mark = match (v.current, v.edited) {
            (true, true) => "current*",
            (true, false) => "current",
            _ => "",
        };
        println!("{:>3}  {:<8}  {:<20}  {:<22}  {}", v.n, mark, v.time, v.who.show(), v.note);
    }
    if all.iter().any(|v| v.edited) {
        println!("* the current file was changed outside `wsp artefact put`; the next put saves it as a version");
    }
    Ok(())
}

fn cmd_restore(root: &Path, args: &Args) -> Result<(), String> {
    let (Some(spec), Some(n)) = (args.rest.get(1), args.rest.get(2)) else { return Err(USAGE.into()) };
    let n: u64 = n.parse().map_err(|_| format!("`{n}` is not a version number"))?;
    let (owner, file) = resolve(root, spec)?;
    let p = restore(root, &owner, &file, n, &note_of(args)?, &Who::here())?;
    say_put("restored", &file, &owner, &p);
    Ok(())
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["artefact"],
        run: artefact,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 630,
                text: r##"  wsp artefact put <owner> <file> [--as <name>] [--note "…"|-]
                                    write an asset for the person into ~/.compound/
                                    artefacts/<owner>/: the file it replaces moves to
                                    .versions first, so a new version is another put
                                    of the same name, never `-v2` and never a copy over
                                    it (a copy by hand is caught and saved as a version)"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 640,
                text: r##"  wsp artefact versions <owner>/<name> [--json]
                                    every version, oldest first, the current one last"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 650,
                text: r##"  wsp artefact restore <owner>/<name> <n>
                                    make version n current, as a new version"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-artefact-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn src(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join("src").join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }

    fn who() -> Who {
        Who { row: Some("wsp-376".into()), seat: Some("cpd-1".into()) }
    }

    fn lines(root: &Path, owner: &str, file: &str) -> Vec<Value> {
        index_of(&vdir(root, owner, file))
    }

    #[test]
    fn three_puts_of_one_name_leave_the_current_file_and_two_versions_with_their_index_lines() {
        let t = temp("three");
        let root = t.join("artefacts");
        for (i, text) in ["one", "two", "three"].iter().enumerate() {
            let f = src(&t, "report.html", text);
            let p = put(&root, "wsp-376", &f, None, &format!("pass {}", i + 1), &who()).unwrap();
            assert_eq!(p.n, i as u64 + 1);
        }
        let cur = root.join("wsp-376/report.html");
        assert_eq!(fs::read_to_string(&cur).unwrap(), "three");
        let dir = root.join("wsp-376/.versions/report.html");
        assert_eq!(fs::read_to_string(dir.join("1.html")).unwrap(), "one");
        assert_eq!(fs::read_to_string(dir.join("2.html")).unwrap(), "two");
        let idx = lines(&root, "wsp-376", "report.html");
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0]["n"], 1);
        assert_eq!(idx[0]["note"], "pass 1");
        assert_eq!(idx[0]["by"]["row"], "wsp-376");
        assert_eq!(idx[0]["by"]["seat"], "cpd-1");
        assert_eq!(idx[1]["n"], 2);
        assert_eq!(idx[1]["hash"], hash_of(b"two"));
        // Nothing else sits in the row's folder where a reader lists files.
        let mut names: Vec<_> = fs::read_dir(root.join("wsp-376")).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, [".versions", "report.html"]);
        let all = versions(&root, "wsp-376", "report.html");
        assert_eq!(all.iter().map(|v| (v.n, v.current)).collect::<Vec<_>>(), [(1, false), (2, false), (3, true)]);
        assert!(!all[2].edited);
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn restore_makes_an_old_version_current_as_a_new_one_and_loses_nothing() {
        let t = temp("restore");
        let root = t.join("artefacts");
        for text in ["one", "two", "three"] {
            put(&root, "wsp-376", &src(&t, "r.md", text), None, "", &who()).unwrap();
        }
        let p = restore(&root, "wsp-376", "r.md", 1, "the first was better", &who()).unwrap();
        assert_eq!(p.n, 4);
        assert_eq!(fs::read_to_string(root.join("wsp-376/r.md")).unwrap(), "one");
        let all = versions(&root, "wsp-376", "r.md");
        assert_eq!(all.len(), 4);
        let kept: Vec<String> = all.iter().filter(|v| !v.current).map(|v| fs::read_to_string(&v.path).unwrap()).collect();
        assert_eq!(kept, ["one", "two", "three"]);
        assert_eq!(all[3].note, "restored from version 1: the first was better");
        assert!(restore(&root, "wsp-376", "r.md", 9, "", &who()).unwrap_err().contains("no version 9"));
        assert!(restore(&root, "wsp-376", "r.md", 4, "", &who()).unwrap_err().contains("already the current"));
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn a_plain_cp_over_the_current_file_is_caught_by_the_next_put_and_saved_as_a_version() {
        let t = temp("cp");
        let root = t.join("artefacts");
        put(&root, "wsp-376", &src(&t, "a.html", "one"), None, "", &who()).unwrap();
        let cur = root.join("wsp-376/a.html");
        fs::copy(src(&t, "by-hand.html", "typed over it"), &cur).unwrap();
        let seen = versions(&root, "wsp-376", "a.html");
        assert!(seen.last().unwrap().edited, "versions says the file moved");
        let p = put(&root, "wsp-376", &src(&t, "a.html", "two"), None, "after", &who()).unwrap();
        assert_eq!(p.rescued, Some(2));
        assert_eq!(p.n, 3);
        assert_eq!(fs::read_to_string(&cur).unwrap(), "two");
        let dir = root.join("wsp-376/.versions/a.html");
        assert_eq!(fs::read_to_string(dir.join("2.html")).unwrap(), "typed over it");
        let idx = lines(&root, "wsp-376", "a.html");
        assert_eq!(idx.len(), 1);
        assert!(idx[0]["note"].as_str().unwrap().contains("outside put"));
        // …and the one after that finds the file as put left it.
        let p = put(&root, "wsp-376", &src(&t, "a.html", "three"), None, "", &who()).unwrap();
        assert_eq!((p.rescued, p.n), (None, 4));
        assert_eq!(fs::read_to_string(dir.join("3.html")).unwrap(), "two");
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn a_file_already_there_before_the_first_put_is_kept() {
        let t = temp("before");
        let root = t.join("artefacts");
        fs::create_dir_all(root.join("wsp-376")).unwrap();
        fs::write(root.join("wsp-376/old.html"), "made by hand").unwrap();
        let p = put(&root, "wsp-376", &src(&t, "old.html", "new"), None, "", &who()).unwrap();
        assert_eq!((p.rescued, p.n), (Some(1), 2));
        assert_eq!(fs::read_to_string(root.join("wsp-376/.versions/old.html/1.html")).unwrap(), "made by hand");
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn the_same_content_again_writes_nothing_and_as_names_the_artefact() {
        let t = temp("same");
        let root = t.join("artefacts");
        let f = src(&t, "draft.html", "x");
        put(&root, "tweaks-next", &f, Some("effort"), "", &who()).unwrap();
        assert!(root.join("tweaks-next/effort.html").is_file(), "a name with no extension takes the file's");
        let p = put(&root, "tweaks-next", &f, Some("effort"), "", &who()).unwrap();
        assert!(p.unchanged);
        assert_eq!(lines(&root, "tweaks-next", "effort.html").len(), 0);
        assert_eq!(resolve(&root, "tweaks-next/effort").unwrap().1, "effort.html");
        assert_eq!(resolve(&root, "tweaks-next/effort.html").unwrap().1, "effort.html");
        assert!(resolve(&root, "tweaks-next/nope").is_err());
        assert!(put(&root, "../x", &f, None, "", &who()).is_err());
        assert!(put(&root, "wsp-1", &f, Some(".hid"), "", &who()).is_err());
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn putting_the_current_file_onto_itself_records_it_without_a_copy_of_itself() {
        let t = temp("inplace");
        let root = t.join("artefacts");
        put(&root, "wsp-376", &src(&t, "a.html", "one"), None, "", &who()).unwrap();
        let cur = root.join("wsp-376/a.html");
        fs::write(&cur, "edited in place").unwrap();
        let p = put(&root, "wsp-376", &cur, None, "edited", &who()).unwrap();
        assert_eq!(p.n, 2);
        assert_eq!(fs::read_to_string(&cur).unwrap(), "edited in place");
        assert!(!versions(&root, "wsp-376", "a.html").last().unwrap().edited);
        let _ = fs::remove_dir_all(&t);
    }
}
