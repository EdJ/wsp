//! A packet for a row that has none (wsp-344).
//!
//! `wsp packet` used to print a section somebody had written, so no row ran as
//! a packet unless a person wrote one first. This writes it: the row's overview,
//! then the context pack `~/claude/compounding/spikes/indexing-v2` builds from
//! the project's finished rows (the closest precedent's diff trimmed to the files
//! this one will likely touch, the signatures of what it touches, the test that
//! verifies it), then the likely files inlined while they fit under the cap.
//! The result is written into the row's `## Packet`, where a person can read and
//! edit it before the row is spawned.
//!
//! Which rows spawn with one unasked is [`kind_of`]: compounding's
//! `scripts/baseline/classify.py` as `gate.py` applies it to a brief, ported
//! because wsp cannot call a Python script at every spawn and the rules are
//! a handful of word lists. A row classed routine or extension takes a packet;
//! novel and discovery rows are spawned as ever unless somebody ran
//! `wsp packet <row>` on them, which is the row opting in.
//!
//! The builder is a separate program and an optional one. [`pack_bin`] finds it;
//! without it a spawn goes the ordinary way and `wsp packet` writes a packet
//! of the overview and the files it names, saying that the pack is missing.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::model::{Status, Task};
use crate::packet::{CAP, SECTION};
use crate::store::Store;
use crate::util;

// ---- what kind of work a brief is -------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Routine,
    Extension,
    Novel,
    Discovery,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Routine => "routine",
            Kind::Extension => "extension",
            Kind::Novel => "novel",
            Kind::Discovery => "discovery",
        }
    }

    /// The kinds a packet is the right brief for.
    pub fn small(self) -> bool {
        matches!(self, Kind::Routine | Kind::Extension)
    }
}

/// The change size `gate.kind_at_brief` assumes when it has no estimate.
const ASSUMED_CODE_LINES: usize = 200;

const FIX: &[&str] = &[
    "fix*", "fails", "fail", "failing", "failed", "red", "regress*", "stale", "leftover", "leftovers", "broken", "bug",
    "drops", "drop", "lost", "wrong", "flak*", "typo", "rebless*", "observed", "still", "never",
];
const MECH: &[&str] = &["split*", "inline*", "rename*", "extract*", "reformat*", "rebless*", "oversized"];
const INVEST_TITLE: &[&str] = &[
    "measure*", "investigat*", "spike", "prototype", "explor*", "decide", "slow", "why", "research", "profile",
];
const INVEST_PROSE: &[&str] = &["measured", "measure", "hang", "hangs", "cost", "costs", "slow", "investigat*", "unknown", "hypothesis", "profile"];

/// Phrases the word lists cannot say: every word in order.
const FIX_PHRASES: &[&[&str]] = &[
    &["does", "nothing"], &["no", "longer"], &["found", "by"], &["found", "at"], &["can", "t"], &["cant"], &["re", "bless*"],
];
const MECH_PHRASES: &[&[&str]] = &[&["re", "bless*"], &["bound", "covers"], &["move", "to"], &["moves", "to"], &["moved", "to"], &["move", "into"], &["moves", "into"], &["moved", "into"]];
const INVEST_TITLE_PHRASES: &[&[&str]] = &[
    &["find", "when"], &["find", "why"], &["find", "where"], &["name", "the", "cause"], &["root", "cause"], &["from", "scratch"],
];
const INVEST_PROSE_PHRASES: &[&[&str]] = &[&["root", "cause"], &["name", "the", "cause"]];

fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn stem_matches(pattern: &str, word: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(stem) => word.starts_with(stem),
        None => pattern == word,
    }
}

/// The distinct words of `ws` any pattern matches, plus the phrases found.
fn hits(ws: &[String], singles: &[&str], phrases: &[&[&str]]) -> BTreeSet<String> {
    let mut found: BTreeSet<String> = ws.iter().filter(|w| singles.iter().any(|p| stem_matches(p, w))).cloned().collect();
    for phrase in phrases {
        if ws.windows(phrase.len()).any(|w| w.iter().zip(phrase.iter()).all(|(a, b)| stem_matches(b, a))) {
            found.insert(phrase.join(" "));
        }
    }
    found
}

fn head(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// A brief's prose as classification reads it: without the answer an agent
/// appended later, whitespace flattened.
fn prose_of(overview: &str) -> String {
    let own = match overview.find("\n# Answer").or_else(|| overview.find("\n## Answer")) {
        Some(at) => &overview[..at],
        None => overview,
    };
    own.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn kind_of(title: &str, overview: &str) -> (Kind, &'static str) {
    let whole = prose_of(overview);
    let prose = head(&whole, 900);
    let title_words = words(title);
    let lead_words = words(&head(&prose, 300));
    let early_words = words(&head(&prose, 600));
    let new_crate = {
        let all = words(&format!("{title} {whole}"));
        all.windows(2).any(|w| w[0] == "new" && w[1] == "crate")
    };
    // A design row is novel work whatever its words: classify.py reads "design:"
    // only when no space follows it, so it files nearly all of them as extensions.
    let design = title.trim_start().to_lowercase().starts_with("design") || title.to_lowercase().contains("design:");
    let invest_title = design || !hits(&title_words, INVEST_TITLE, INVEST_TITLE_PHRASES).is_empty();
    let mechanism_wrong = {
        let lower = head(&prose, 600).to_lowercase();
        lower.match_indices("mechanism").any(|(at, m)| {
            let after: String = lower[at + m.len()..].chars().take(45).take_while(|c| *c != '.').collect();
            after.contains("wrong")
        })
    };
    let mut invest_prose = hits(&early_words, INVEST_PROSE, INVEST_PROSE_PHRASES);
    if mechanism_wrong {
        invest_prose.insert("mechanism wrong".into());
    }
    let invest_prose = invest_prose.len() >= 2;
    let fixy = !hits(&title_words, FIX, FIX_PHRASES).is_empty() || !hits(&lead_words, FIX, FIX_PHRASES).is_empty();
    let mech = !hits(&title_words, MECH, MECH_PHRASES).is_empty();

    if new_crate {
        return (Kind::Novel, "new crate");
    }
    if mech {
        return (Kind::Routine, "mechanical refactor");
    }
    if invest_title {
        return (Kind::Discovery, "investigate / measure-then-fix (title)");
    }
    if invest_prose && !fixy {
        return (Kind::Discovery, "investigate / measure-then-fix (prose)");
    }
    if fixy && ASSUMED_CODE_LINES <= 400 {
        return (Kind::Routine, "fix, small diff");
    }
    (Kind::Extension, "default: new capability on an existing seam")
}

/// A row's overview: what its author asked for, without the log and the details.
pub fn overview_of(t: &Task) -> String {
    t.section("Overview").unwrap_or_default().trim().to_string()
}

// ---- the context pack builder ----------------------------------------------

/// `WSP_PACK_BIN`, else the build in compounding's checkout of `indexing-v2`.
pub fn pack_bin() -> Result<PathBuf, String> {
    let named = std::env::var("WSP_PACK_BIN").ok().filter(|p| !p.is_empty()).map(|p| util::expand(&p));
    let path = named.clone().unwrap_or_else(|| util::expand("~/claude/compounding/spikes/indexing-v2/target/release/ixv2"));
    if path.is_file() {
        return Ok(path);
    }
    Err(match named {
        Some(p) => format!("WSP_PACK_BIN={} is not a file", util::contract(&p)),
        None => "the pack builder is not built: `cargo build --release` in ~/claude/compounding/spikes/indexing-v2, or set WSP_PACK_BIN"
            .to_string(),
    })
}

const PACK_TIMEOUT: Duration = Duration::from_secs(120);

fn run(bin: &Path, args: &[String], envs: &[(&str, String)], stdin: &str) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args).envs(envs.iter().map(|(k, v)| (*k, v.as_str()))).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", util::contract(bin)))?;
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(stdin.as_bytes());
    }
    let out = child.stdout.take();
    let err = child.stderr.take();
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = read(out.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let err = read(err.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() > PACK_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the pack builder ran past {}s and was stopped", PACK_TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if !status.success() {
        return Err(format!("the pack builder failed: {}", err.lines().last().unwrap_or("no message")));
    }
    Ok(out)
}

/// The top-level directories the builder reads Rust from.
fn roots_of(repo: &Path, rev: &str) -> String {
    let out = crate::cmd_verify::git(repo, &["ls-tree", "-d", "--name-only", rev]).unwrap_or_default();
    let skip = ["target", "vendor", "docs", "fixtures", "node_modules", "evidence"];
    let dirs: Vec<&str> = out.lines().filter(|d| !d.starts_with('.') && !skip.contains(d)).collect();
    dirs.join(",")
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

fn is_noise(p: &str) -> bool {
    p.contains("/vendor/")
        || p.contains("/fixtures/")
        || p.contains("/evidence/")
        || p.starts_with("vendor/")
        || p.starts_with("target/")
        || p.ends_with("Cargo.lock")
        || [".png", ".jpg", ".gif", ".snap", ".mp4"].iter().any(|e| p.ends_with(e))
}

/// The builder's table of finished rows, in its own file format: each row of the
/// project that is at review or done, with the files its commits touched. Rewritten
/// only when the trunk has moved.
fn precedent_table(store: &Store, project: &str, repo: &Path, rev: &str, file: &Path) -> Result<(), String> {
    let stamp = PathBuf::from(format!("{}.rev", file.display()));
    if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == rev) && file.is_file() {
        return Ok(());
    }
    let finished: Vec<Task> = store
        .tasks()
        .into_iter()
        .filter(|t| t.project.as_deref() == Some(project) && matches!(t.status(), Status::Review | Status::Done))
        .collect();
    let log = crate::cmd_verify::git(repo, &["log", rev, "--reverse", "--no-renames", "--numstat", "--format=@@%at%x09%s"])
        .ok_or_else(|| format!("git log of {} failed", util::contract(repo)))?;
    struct Row {
        first: i64,
        last: i64,
        commits: u32,
        files: std::collections::BTreeMap<String, u32>,
    }
    let mut rows: std::collections::HashMap<String, Row> = std::collections::HashMap::new();
    let mut current: Option<String> = None;
    for line in log.lines() {
        if let Some(h) = line.strip_prefix("@@") {
            let (ts, subject) = h.split_once('\t').unwrap_or(("0", h));
            let id = subject.split([':', ' ']).next().unwrap_or("");
            current = finished.iter().find(|t| t.id == id).map(|t| t.id.clone());
            if let Some(id) = &current {
                let ts: i64 = ts.parse().unwrap_or(0);
                let row = rows.entry(id.clone()).or_insert(Row { first: i64::MAX, last: 0, commits: 0, files: Default::default() });
                row.first = row.first.min(ts);
                row.last = row.last.max(ts);
                row.commits += 1;
            }
        } else if let Some(id) = &current {
            let mut p = line.splitn(3, '\t');
            if let (Some(a), Some(d), Some(path)) = (p.next(), p.next(), p.next()) {
                if !is_noise(path) {
                    let n = a.parse::<u32>().unwrap_or(0) + d.parse::<u32>().unwrap_or(0);
                    if let Some(row) = rows.get_mut(id) {
                        *row.files.entry(path.to_string()).or_insert(0) += n.max(1);
                    }
                }
            }
        }
    }
    let mut ordered: Vec<(&Task, &Row)> = finished.iter().filter_map(|t| rows.get(&t.id).map(|r| (t, r))).collect();
    ordered.sort_by_key(|(t, r)| (r.last, t.id.clone()));
    let mut text = String::new();
    for (t, r) in ordered {
        let mut files: Vec<(&String, &u32)> = r.files.iter().collect();
        files.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let files: Vec<String> = files.iter().map(|(p, n)| format!("{p}={n}")).collect();
        let num = t.id.rsplit('-').next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
        text.push_str(&format!(
            "{}\t{}\t\t{}\t{}\t{}\t{}\t{}\t{}\n",
            t.id,
            num,
            r.first,
            r.last,
            r.commits,
            esc(&t.title),
            esc(&files.join("\u{1f}")),
            esc(t.body.trim())
        ));
    }
    std::fs::write(file, text).map_err(|e| format!("{}: {e}", util::contract(file)))?;
    std::fs::write(&stamp, rev).map_err(|e| e.to_string())
}

pub struct Pack {
    pub text: String,
    /// The files the builder expects the change to touch, most likely first.
    pub likely: Vec<String>,
}

/// A command for the test the pack names, when the builder could not give one:
/// the builder knows compound's layout and leaves the line empty elsewhere.
fn test_command(repo: &Path, path: &str) -> Option<String> {
    let rest = path.strip_prefix("src/")?.strip_suffix(".rs")?;
    if !repo.join("Cargo.toml").is_file() {
        return None;
    }
    let module = rest.strip_suffix("/mod").unwrap_or(rest).replace('/', "::");
    Some(format!("cargo test {module}::"))
}

/// The pack's own text, tidied for a section a person reads: the empty `verify`
/// lines the builder leaves outside its home layout are filled, and the line that
/// advertises a search tool the packet's agent does not have is dropped.
fn tidy(pack: &str, project: &str, repo: &Path) -> String {
    let mut out = Vec::new();
    let lines: Vec<&str> = pack.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("search more:") || line.starts_with('(') && line.contains("tokens") {
            continue;
        }
        if line.trim_end() == "verify" {
            let path = lines.get(i + 1).and_then(|l| l.trim().strip_prefix("in ")).and_then(|l| l.split(':').next());
            match path.and_then(|p| test_command(repo, p)) {
                Some(cmd) => out.push(format!("verify  {cmd}")),
                None => continue,
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("CONTEXT PACK  compound@") {
            out.push(format!("CONTEXT PACK  {project}@{rest}"));
            continue;
        }
        out.push((*line).to_string());
    }
    out.join("\n")
}

/// A pack with no test of its own names the tests inside the file it ranks first.
fn with_verify(text: String, likely: &[String], repo: &Path) -> String {
    if text.lines().any(|l| l.starts_with("verify")) {
        return text;
    }
    match likely.first().and_then(|f| test_command(repo, f)) {
        Some(cmd) => format!("{}\nverify  {cmd}   (the tests in {}; narrow it with a filter)", text.trim_end(), likely[0]),
        None => text,
    }
}

/// Build the pack for a row from the project's trunk. `Err` says why there is none.
pub fn build_pack(store: &Store, t: &Task, kind: Kind, depth: &str) -> Result<Pack, String> {
    let bin = pack_bin()?;
    let project = t.project.clone().ok_or("the row has no project")?;
    let root = crate::resolve::Index::new(store.projects()).root_of(&project).ok_or_else(|| format!("{project} has no root"))?;
    let repo = crate::cmd_verify::toplevel(&util::expand(&root)).ok_or_else(|| format!("{root} is not a repository"))?;
    let rev = crate::cmd_checkout::trunk_tip(&root).ok_or("the trunk has no tip")?;
    let dir = store.state_file(&format!("packet/{project}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (index, table) = (dir.join("index.tsv"), dir.join("precedent.tsv"));
    precedent_table(store, &project, &repo, &rev, &table)?;
    let roots = roots_of(&repo, &rev);
    if roots.is_empty() {
        return Err("the repository has nothing the builder reads".into());
    }
    let args = |depth: &str| -> Vec<String> {
        ["pack", "--title", t.title.as_str(), "--body-file", "-", "--repo", &repo.to_string_lossy(), "--rev", &rev, "--index", &index.to_string_lossy(), "--precedent", &table.to_string_lossy(), "--depth", depth, "--kind", kind.name()]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    let envs = [("IXV2_ROOTS", roots)];
    let overview = overview_of(t);
    let meta = run(&bin, &args("meta"), &envs, &overview)?;
    let likely = serde_json::from_str::<serde_json::Value>(meta.trim())
        .ok()
        .and_then(|v| v["likely"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect::<Vec<_>>()))
        .unwrap_or_default();
    let text = run(&bin, &args(depth), &envs, &overview)?;
    Ok(Pack { text: with_verify(tidy(&text, &project, &repo), &likely, &repo), likely })
}

// ---- the section ------------------------------------------------------------

/// A line of the author's or the pack's words that the `## Packet` parser would
/// read as a directive is marked so it stays words.
fn as_words(text: &str) -> String {
    text.lines()
        .map(|l| {
            let bare = l.trim().trim_start_matches("- ").trim();
            let directive = ["file:", "attention:"].iter().any(|d| bare.starts_with(d)) || bare.eq_ignore_ascii_case("packet: off");
            if directive { format!("· {l}") } else { l.to_string() }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A file is inlined whole or not at all, and not when it alone would take more
/// than this much of the cap.
const FILE_SHARE: usize = 7_000;

pub struct Generated {
    pub section: String,
    pub kind: Kind,
    /// Why there is no pack, or how many files it expects the change to touch.
    pub pack: Result<usize, String>,
}

/// The section for `t` read from the tree at `root`. Never writes.
pub fn generate(store: &Store, t: &Task, root: &Path) -> Generated {
    generate_led(store, t, root, "")
}

/// [`generate`], with the files `lead` names (a leg's order) inlined before the
/// ones the pack expects.
pub fn generate_led(store: &Store, t: &Task, root: &Path, lead: &str) -> Generated {
    let (kind, _) = kind_of(&t.title, &overview_of(t));
    let mut pack = build_pack(store, t, kind, "full");
    let compose = |overview: &str, pack: Option<&Pack>| -> String {
        let mut s = format!("{}\n", as_words(overview));
        if let Some(p) = pack {
            s.push_str(&format!("\n{}\n", as_words(&p.text)));
        }
        s
    };
    let overview = overview_of(t);
    let mut text = compose(&overview, pack.as_ref().ok());
    // Over the cap with room for no file: the thin pack, then a cut overview.
    if text.len() + 200 > CAP {
        if let Ok(p) = build_pack(store, t, kind, "thin") {
            pack = Ok(p);
            text = compose(&overview, pack.as_ref().ok());
        }
    }
    if text.len() + 200 > CAP {
        let room = CAP.saturating_sub(text.len() - overview.len() + 400);
        let cut = overview.char_indices().map(|(i, _)| i).take_while(|i| *i <= room).last().unwrap_or(0);
        let cut = overview[..cut].rfind("\n\n").unwrap_or(cut);
        let shortened = format!("{}\n\n(The overview is cut here to fit the packet; the whole of it is `wsp show {}`.)", &overview[..cut], t.id);
        text = compose(&shortened, pack.as_ref().ok());
    }
    let mut candidates: Vec<String> = named_files(lead, root);
    let rest = match &pack {
        Ok(p) => p.likely.clone(),
        Err(_) => named_files(&overview, root),
    };
    for f in rest {
        if !candidates.contains(&f) {
            candidates.push(f);
        }
    }
    let mut files: Vec<String> = Vec::new();
    let mut spent = text.len() + 160;
    for f in candidates {
        let Ok(body) = std::fs::read_to_string(root.join(&f)) else { continue };
        let cost = body.len() + f.len() + 24;
        if body.len() <= FILE_SHARE && spent + cost <= CAP {
            spent += cost;
            files.push(f);
        }
    }
    let mut section = String::new();
    for f in &files {
        section.push_str(&format!("file: {f}\n"));
    }
    section.push_str(&text);
    Generated { section: section.trim_end().to_string(), kind, pack: pack.map(|p| p.likely.len()) }
}

/// Paths the overview names that exist in the tree, for a pack-less packet.
fn named_files(overview: &str, root: &Path) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in overview.split_whitespace() {
        let w = word.trim_end_matches(|c: char| "`\"'()[],;:*.".contains(c)).trim_start_matches(|c: char| "`\"'()[],;:*".contains(c));
        let looks = w.contains('/') || w.ends_with(".rs");
        if looks && !w.starts_with("http") && !w.starts_with("..") && !w.starts_with('/') && root.join(w).is_file() && !found.contains(&w.to_string()) {
            found.push(w.to_string());
        }
    }
    found
}

/// Whether a row gets a packet unasked: it has none, has not declined one, and
/// is the kind of work a packet briefs well.
pub fn eligible(t: &Task) -> Result<Kind, String> {
    if t.section(SECTION).is_some_and(|s| !s.trim().is_empty()) {
        return Err("it already has a packet".into());
    }
    let (kind, reason) = kind_of(&t.title, &overview_of(t));
    if kind.small() {
        Ok(kind)
    } else {
        Err(format!("it reads as {} work ({reason})", kind.name()))
    }
}

// ---- a leg's packet ---------------------------------------------------------

/// How much of the row's overview a leg's packet carries as background.
const ROW_SHARE: usize = 3_000;

/// How much of a finished leg's account a leg's packet carries.
const AFTER_SHARE: usize = 600;

/// The row as one leg's brief: its order first, then what the legs it follows
/// said they did, then the row's overview (or its packet, for a row with no
/// overview). Built as a task so the same generator reads it.
fn leg_task(t: &Task, leg: &crate::legs::Leg, all: &[crate::legs::Leg]) -> Task {
    let order = leg.order.lines().filter(|l| !l.trim_start().trim_start_matches("- ").starts_with(crate::packet::LEG_KEY)).collect::<Vec<_>>().join("\n");
    let order = order.trim();
    let mut text = format!("{order}\n");
    let waits = crate::legs::upstream(all, &leg.name);
    let said: Vec<String> = all
        .iter()
        .filter(|l| waits.contains(&l.name))
        .filter_map(|l| {
            let account = l.caption.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| head(l.baton.trim(), AFTER_SHARE));
            (!account.is_empty()).then(|| format!("- {}: {}", l.name, account))
        })
        .collect();
    if !said.is_empty() {
        text.push_str(&format!("\nLegs before this one:\n{}\n", said.join("\n")));
    }
    let row = Some(overview_of(t)).filter(|o| !o.is_empty()).or_else(|| crate::packet::of(t).map(|p| p.text)).unwrap_or_default();
    if !row.is_empty() {
        text.push_str(&format!("\nThe row, {} ({}):\n{}\n", t.id, t.title, head(&row, ROW_SHARE)));
    }
    let mut lt = t.clone();
    let first = order.lines().find(|l| !l.trim().is_empty()).unwrap_or(&leg.name);
    lt.title = head(first.trim(), 100);
    lt.body = format!("## Overview\n{text}");
    lt
}

/// Whether a leg is given a packet unasked: its order names none, does not
/// decline one, and reads as the kind of work a packet briefs well.
pub fn leg_eligible(leg: &crate::legs::Leg) -> Result<Kind, String> {
    if crate::packet::leg_path(&leg.order).is_some() {
        return Err("it names a packet".into());
    }
    if crate::packet::leg_declined(&leg.order) {
        return Err("it declined one".into());
    }
    let first = leg.order.lines().find(|l| !l.trim().is_empty()).unwrap_or(&leg.name);
    let (kind, reason) = kind_of(first, &leg.order);
    if kind.small() {
        Ok(kind)
    } else {
        Err(format!("it reads as {} work ({reason})", kind.name()))
    }
}

/// The section for one leg of `t`, read from the tree at `root`. Never writes.
pub fn generate_leg(store: &Store, t: &Task, leg: &crate::legs::Leg, root: &Path) -> Generated {
    let all = crate::legs::legs(t);
    generate_led(store, &leg_task(t, leg, &all), root, &leg.order)
}

/// Write a generated packet into the row. Returns what was written.
pub fn write(store: &Store, t: &mut Task, section: &str) -> Result<(), String> {
    crate::model::set_section_in(&mut t.body, SECTION, &format!("{}\n{section}", crate::packet::GENERATED));
    store.save_task(t).map_err(|e| e.to_string())?;
    store.git_commit(&format!("wsp: packet {} — generated", t.id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(title: &str, overview: &str) -> Kind {
        kind_of(title, overview).0
    }

    #[test]
    fn a_fix_and_a_mechanical_refactor_are_routine() {
        assert_eq!(kind("Fix the stale count on the dock", "The badge is wrong after a restart."), Kind::Routine);
        assert_eq!(kind("Split cmd_spawn.rs", "It is oversized."), Kind::Routine);
        assert_eq!(kind("Rename the verifier order", "x"), Kind::Routine);
    }

    #[test]
    fn a_capability_on_an_existing_seam_is_an_extension() {
        assert_eq!(kind("Show the packet size in wsp attempts", "Add a column to the attempts table."), Kind::Extension);
    }

    #[test]
    fn investigation_and_a_new_crate_are_not_small() {
        assert_eq!(kind("Measure why the daemon is slow", "x"), Kind::Discovery);
        assert_eq!(kind("Find when the claim is lost", "x"), Kind::Discovery);
        assert_eq!(kind("Design: a queue", "x"), Kind::Discovery);
        assert_eq!(kind("A wire protocol", "This is a new crate for the wire."), Kind::Novel);
        assert_eq!(kind("Add a flag", "The run hangs and costs a lot; nobody knows the hypothesis yet, so measure first."), Kind::Discovery);
        assert!(!kind("Measure why the daemon is slow", "x").small());
    }

    #[test]
    fn an_investigation_in_the_prose_is_overruled_by_a_fix() {
        assert_eq!(kind("Fix the run hanging", "It hangs and costs; the root cause is unknown."), Kind::Routine);
        assert_eq!(kind("Stop the run hanging", "It hangs and costs; the root cause is unknown."), Kind::Discovery);
    }

    #[test]
    fn the_answer_an_agent_appended_is_not_the_brief() {
        let o = "Add a column.\n\n# Answer\nI measured and investigated the root cause; it hangs and costs.";
        assert_eq!(kind("Add a column", o), Kind::Extension);
    }

    #[test]
    fn only_small_kinds_take_a_packet() {
        assert!(Kind::Routine.small() && Kind::Extension.small());
        assert!(!Kind::Novel.small() && !Kind::Discovery.small());
    }

    #[test]
    fn a_line_that_would_be_read_as_a_directive_stays_words() {
        let kept = as_words("Do it.\nfile: src/a.rs\n- attention: 2\npacket: off\nplain");
        let p = crate::packet::parse(&kept);
        assert!(p.files.is_empty() && !p.off && p.attention_cents.is_none(), "{kept}");
        assert!(p.text.contains("file: src/a.rs") && p.text.contains("plain"));
    }

    #[test]
    fn the_empty_verify_lines_of_a_foreign_layout_are_filled_from_the_path() {
        let dir = std::env::temp_dir().join(format!("wsp-packet-gen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        let pack = "CONTEXT PACK  compound@abc  built\nverify  \n        in src/cmd_attempts.rs: a_test\nsearch more: ixfind \"x\"\n(246 ms, ~809 tokens, cap 3000)\n";
        let out = tidy(pack, "wsp", &dir);
        assert!(out.starts_with("CONTEXT PACK  wsp@abc"), "{out}");
        assert!(out.contains("verify  cargo test cmd_attempts::\n        in src/cmd_attempts.rs: a_test"), "{out}");
        assert!(!out.contains("search more") && !out.contains("809 tokens"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pack_with_no_test_names_the_tests_in_its_first_file() {
        let dir = std::env::temp_dir().join(format!("wsp-packet-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        let likely = vec!["src/detail/run.rs".to_string()];
        let out = with_verify("CONTEXT PACK  wsp@abc\n".into(), &likely, &dir);
        assert!(out.contains("verify  cargo test detail::run::   (the tests in src/detail/run.rs"), "{out}");
        let own = with_verify("verify  cargo test -p x\n".into(), &likely, &dir);
        assert_eq!(own, "verify  cargo test -p x\n", "a pack's own verify line stands");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_row_with_a_packet_or_a_novel_brief_is_not_eligible() {
        let mut t = Task::default();
        t.title = "Add a column".into();
        t.body = "## Overview\nAdd a column.\n".into();
        assert!(eligible(&t).is_ok());
        t.body.push_str("\n## Packet\npacket: off\n");
        assert!(eligible(&t).unwrap_err().contains("already has a packet"));
        t.body = "## Overview\nx\n".into();
        t.title = "Measure why it is slow".into();
        assert!(eligible(&t).unwrap_err().contains("discovery"));
    }

    #[test]
    fn a_packet_without_a_pack_names_the_files_the_overview_names() {
        let dir = std::env::temp_dir().join(format!("wsp-packet-named-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        let got = named_files("Change `src/a.rs`, then src/missing.rs and https://x/y.rs and ../out.rs.", &dir);
        assert_eq!(got, ["src/a.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
