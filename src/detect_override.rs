//! The agent-detection override, and the thing that notices it is there.
//!
//! herdr resolves each agent's detection rules from a manifest, and takes the
//! first of three: a local file at `~/.config/herdr/agent-detection/<agent>.toml`,
//! then the remote manifest it keeps refreshed under
//! `~/.local/state/herdr/agent-detection/remote/`, then the one built into the
//! binary. The local file wins, and a fresh process picks it up with no reload,
//! no rebuild and no restart — measured 2026-08-19 against herdr 0.8.0 by
//! putting a copy of the bundled `kilo` manifest in that directory and asking
//! `herdr agent explain --file /dev/null --agent kilo --json`, which answered
//! with the override as its source, immediately.
//!
//! That matters here because this fleet forks herdr, and every fork line is
//! paid again at every rebase. A detection fix through this path costs none:
//! it is a file herdr already reads, in a schema herdr already defines. No
//! restart is the expensive half — restarting herdr kills every agent on the
//! machine.
//!
//! # And the catch, which is the only reason this module exists
//!
//! **The override shadows upstream's remote updates for that agent, silently.**
//! herdr goes on fetching the remote manifest — `manifest_update` knows nothing
//! about overrides — so the version on disk keeps advancing while the rules
//! actually in force stop. Nothing about a stale detection rule looks like a
//! stale config file: it looks like an agent behaving oddly.
//!
//! The cost is not hypothetical, and one month is enough to pay it. herdr
//! 0.8.0 ships claude `2026.07.13.1`; the remote on this machine on 2026-08-19
//! was `2026.08.13.1`, and the difference is that Claude Code 2.1.228 changed
//! its busy spinner from braille to half-circles. An override written against
//! the bundled manifest on 13 July would, by 13 August, have reported every
//! working claude agent as idle — on the machine whose whole panel is built on
//! that word.
//!
//! So an override is a patch with a removal condition, not a home, and the
//! removal condition has to be written where somebody will meet it:
//!
//! - **In the file**, as three comment lines herdr's TOML parser ignores:
//!   `# fault:` what misdetects without it, `# base:` the manifest version it
//!   was written against, `# remove-when:` the command that decides and the
//!   answer that means delete it.
//! - **Here**, because a comment in a file nobody opens is invisible and a task
//!   is invisible at 2am. `wsp doctor` names the override every run, and turns
//!   it into a *problem* — exit 1 — once herdr's remote has moved past the
//!   version it was written against. Silencing it honestly means re-running the
//!   check and either deleting the file or recording the version you re-checked
//!   against, which is the loop we want.
//!
//! # What the check can and cannot be
//!
//! `herdr agent explain --file <screen> --agent <label>` runs the whole
//! resolution chain offline: no pane, no live agent, and — since it resolves in
//! the CLI's own process — no effect on the running server until somebody runs
//! `herdr server reload-agent-manifests`. That makes a captured screen the
//! natural removal check: move the override aside, ask, put it back.
//!
//! It only reaches the *screen* rules. `--file` passes screen content and
//! nothing else, so the OSC-title rules — `osc_title_working`,
//! `osc_title_idle`, `osc_progress_idle`, which is where claude's real faults
//! have been — need a live pane and `herdr agent explain <pane>`. `remove-when`
//! is therefore free text carrying a command, rather than a fixture path this
//! module runs itself.
//!
//! # Why the file list is scanned and herdr is then asked
//!
//! The scan finds candidates; herdr adjudicates. An override that does not
//! parse is *ignored* — herdr falls back to remote and reports the reason in
//! `warning` — and an override under `~/.config/herdr-dev/` is read by debug
//! builds only, so a fork agent who writes one there has a file that the
//! installed binary never opens. Both are silent, both look exactly like a file
//! that is working, and both are worth an exit 1.

use crate::util;
use std::path::{Path, PathBuf};

/// The three lines an override has to carry, as parsed off the top of the file.
#[derive(Debug, Default, PartialEq)]
pub struct Head {
    pub fault: Option<String>,
    pub base: Option<String>,
    pub remove_when: Option<String>,
}

impl Head {
    /// Missing keys, in the order the header wants them written.
    fn missing(&self) -> Vec<&'static str> {
        [
            ("fault", self.fault.is_none()),
            ("base", self.base.is_none()),
            ("remove-when", self.remove_when.is_none()),
        ]
        .into_iter()
        .filter(|(_, absent)| *absent)
        .map(|(key, _)| key)
        .collect()
    }
}

/// What herdr says about the agent this override is for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Live {
    /// `manifest_source`: the path or `remote:`/`bundled` label actually in force.
    pub source: String,
    /// `cached_remote_version`: what upstream has, override or no override.
    pub remote: Option<String>,
    /// `warning`: set when herdr read the override and refused it.
    pub warning: Option<String>,
}

/// The comment header, read from the top of the file.
///
/// Comments only, and only before the first line of TOML: a `# base:` written
/// halfway down is a note to a reader, not a declaration, and reading it as one
/// would let a header drift away from the top of the file where the next person
/// looks for it.
pub fn head(text: &str) -> Head {
    let mut out = Head::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(comment) = line.strip_prefix('#') else {
            break;
        };
        let Some((key, value)) = comment.trim().split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "fault" => out.fault = Some(value),
            "base" => out.base = Some(value),
            "remove-when" | "remove when" => out.remove_when = Some(value),
            _ => {}
        }
    }
    out
}

/// The directories the installed herdr and a debug build read overrides from.
///
/// Two, because this fleet runs both: `herdr` for the release binary every pane
/// is in, `herdr-dev` for anything built with `cargo run`. herdr picks between
/// them on `debug_assertions`, so the same file in the wrong one is read by
/// nobody.
pub fn dirs() -> Vec<PathBuf> {
    let root = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => util::home().join(".config"),
    };
    vec![
        root.join("herdr").join("agent-detection"),
        root.join("herdr-dev").join("agent-detection"),
    ]
}

/// Every override file present, as `(agent label, path)`, in a stable order.
pub fn scan(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut here: Vec<(String, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .filter_map(|p| {
                let label = p.file_stem()?.to_str()?.to_string();
                Some((label, p))
            })
            .collect();
        here.sort();
        found.extend(here);
    }
    found
}

/// Ask the installed herdr what it resolves for an agent, with no pane and no
/// live agent of that kind: `--file /dev/null` is a screen that matches nothing,
/// and every field this module wants is about the manifest rather than the
/// screen. ~130ms, and only ever run for an agent an override was found for.
pub fn ask(agent: &str) -> Option<Live> {
    let out = std::process::Command::new(crate::cmd_sandbox::herdr_bin())
        .args(["agent", "explain", "--file", "/dev/null", "--agent", agent, "--json"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Some(Live {
        source: s("manifest_source").unwrap_or_default(),
        remote: s("cached_remote_version"),
        warning: s("warning"),
    })
}

/// Days between two date-shaped manifest versions (`2026.08.13.1`).
///
/// `None` when either is not one: the scheme is upstream's convention rather
/// than a promise, and a number invented from an unparseable string would be
/// the one part of this report a reader could not check.
fn days_between(base: &str, now: &str) -> Option<i64> {
    let day = |v: &str| -> Option<i64> {
        let mut parts = v.split('.');
        let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
        if y.len() != 4 || m.len() != 2 || d.len() != 2 {
            return None;
        }
        match util::epoch_of(&format!("{y}-{m}-{d}")) {
            0 => None,
            secs => Some(secs / 86_400),
        }
    };
    Some(day(now)? - day(base)?)
}

/// What `doctor` says about every agent-detection override on this machine.
///
/// Nothing at all when there are none, which is the normal machine and the
/// state this was written in: the shadowing cost is paid from the moment a file
/// exists, so an override arrives with a fault it fixes or it does not arrive.
///
/// `live` is passed in so the tests can state herdr's answer instead of needing
/// a herdr.
pub fn health(
    dirs: &[PathBuf],
    live: impl Fn(&str) -> Option<Live>,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    for (agent, path) in scan(dirs) {
        let shown = util::contract(&path);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let head = head(&text);
        let answer = live(&agent);

        // Read and refused, or in the directory the other build reads. Either
        // way the rules in force are not the ones in this file, and nothing on
        // the machine says so out loud.
        match &answer {
            Some(a) if a.warning.is_some() => {
                problems.push(format!(
                    "agent-detection override {shown} is being ignored by herdr — the rules in force are {}, and the file is still shadowing upstream's",
                    a.source
                ));
                problems.push(format!("  {}", a.warning.clone().unwrap_or_default().trim().replace('\n', " ")));
            }
            Some(a) if !same_file(&a.source, &path) => problems.push(format!(
                "agent-detection override {shown} is read by no herdr on this machine — the binary in PATH resolves {agent} from {}. A file under `herdr-dev/` is for debug builds only",
                a.source
            )),
            _ => {}
        }

        let missing = head.missing();
        if !missing.is_empty() {
            problems.push(format!(
                "agent-detection override {shown} carries no removal condition ({}) — it shadows every upstream {agent} detection fix and nothing records why, or when it goes",
                missing.join(", ")
            ));
            problems.push(
                "  head it with `# fault:`, `# base:` and `# remove-when:`, or delete it and let herdr's own manifest through".into(),
            );
            continue;
        }

        let fault = head.fault.unwrap_or_default();
        let base = head.base.unwrap_or_default();
        let remove_when = head.remove_when.unwrap_or_default();
        let remote = answer.and_then(|a| a.remote);
        let moved = remote.as_deref().is_some_and(|r| r != base);

        // Behind upstream is a problem and not a note. The file may still be
        // right — but nobody can say so without re-running the check, and a
        // note is exactly what this whole mechanism exists because nobody
        // reads. Exit 1 asks for the one thing that settles it.
        let line = match (&remote, moved) {
            (Some(remote), true) => {
                let since = days_between(&base, remote)
                    .map(|d| format!(", {d} days on"))
                    .unwrap_or_default();
                format!(
                    "agent-detection override {shown} is behind upstream — written against {base}, herdr's remote is now {remote}{since}, and every {agent} fix in between is shadowed"
                )
            }
            (Some(_), false) => format!(
                "agent-detection override {shown} in force, level with upstream's {base}"
            ),
            (None, _) => format!(
                "agent-detection override {shown} in force, written against {base} — herdr has no remote manifest for {agent}, so nothing here can tell you whether upstream has moved"
            ),
        };
        let target = match moved {
            true => &mut *problems,
            false => &mut *notes,
        };
        target.push(line);
        target.push(format!("  for: {fault}"));
        target.push(format!("  remove when: {remove_when}"));
        if moved {
            problems.push(
                "  run that; then delete the file, or record the version you re-checked against in `# base:`".into(),
            );
        }
    }
}

/// Whether herdr's `manifest_source` names this file.
///
/// The label is the path for an override and `remote:<path>` or `bundled` for
/// the other two, so a plain comparison is enough and a mismatch is the answer
/// this wants: whatever is in force, it is not this.
fn same_file(source: &str, path: &Path) -> bool {
    source == path.to_string_lossy()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(source: &str, remote: &str) -> Live {
        Live {
            source: source.into(),
            remote: Some(remote.into()),
            warning: None,
        }
    }

    #[test]
    fn a_machine_with_no_override_is_a_machine_doctor_says_nothing_about() {
        let (mut p, mut n) = (vec![], vec![]);
        let dir = std::env::temp_dir().join("wsp-detect-override-absent");
        health(&[dir], |_| None, &mut p, &mut n);
        assert!(p.is_empty() && n.is_empty(), "{p:?} {n:?}");
    }

    #[test]
    fn the_header_is_read_off_the_top_and_stops_at_the_first_line_of_toml() {
        let h = head(
            "# fault: working reads as idle\n\
             # base: 2026.08.13.1\n\
             # remove-when: herdr agent explain <pane> says working\n\
             id = \"claude\"\n\
             # base: 1999.01.01.1\n",
        );
        assert_eq!(h.base.as_deref(), Some("2026.08.13.1"));
        assert_eq!(h.fault.as_deref(), Some("working reads as idle"));
        assert!(h.remove_when.is_some());
    }

    #[test]
    fn a_header_with_a_key_missing_names_the_key() {
        let h = head("# fault: x\n# base: 2026.08.13.1\n");
        assert_eq!(h.missing(), vec!["remove-when"]);
    }

    #[test]
    fn an_override_with_no_removal_condition_is_a_problem() {
        let dir = write(&[("claude.toml", "id = \"claude\"\n")]);
        let (mut p, mut n) = (vec![], vec![]);
        health(
            &[dir],
            |_| Some(live("/x/claude.toml", "2026.08.13.1")),
            &mut p,
            &mut n,
        );
        assert!(p.iter().any(|x| x.contains("no removal condition")), "{p:?}");
        assert!(n.is_empty(), "{n:?}");
    }

    #[test]
    fn an_override_level_with_upstream_is_a_note_and_says_what_it_is_for() {
        let dir = write(&[(
            "claude.toml",
            "# fault: the spinner\n# base: 2026.08.13.1\n# remove-when: ask a live pane\nid = \"claude\"\n",
        )]);
        let path = dir.join("claude.toml");
        let (mut p, mut n) = (vec![], vec![]);
        health(
            &[dir],
            |_| Some(live(&path.to_string_lossy(), "2026.08.13.1")),
            &mut p,
            &mut n,
        );
        assert!(p.is_empty(), "{p:?}");
        assert!(n.iter().any(|x| x.contains("in force")), "{n:?}");
        assert!(n.iter().any(|x| x.contains("for: the spinner")), "{n:?}");
    }

    #[test]
    fn an_override_upstream_has_moved_past_is_a_problem_that_counts_the_days() {
        let dir = write(&[(
            "claude.toml",
            "# fault: the spinner\n# base: 2026.07.13.1\n# remove-when: ask a live pane\nid = \"claude\"\n",
        )]);
        let path = dir.join("claude.toml");
        let (mut p, mut n) = (vec![], vec![]);
        health(
            &[dir],
            |_| Some(live(&path.to_string_lossy(), "2026.08.13.1")),
            &mut p,
            &mut n,
        );
        assert!(n.is_empty(), "{n:?}");
        assert!(p.iter().any(|x| x.contains("31 days on")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("record the version")), "{p:?}");
    }

    #[test]
    fn an_override_herdr_refused_is_a_problem_even_though_it_reads_as_installed() {
        let dir = write(&[(
            "claude.toml",
            "# fault: x\n# base: 2026.08.13.1\n# remove-when: y\nnot toml {{{\n",
        )]);
        let (mut p, mut n) = (vec![], vec![]);
        health(
            &[dir],
            |_| {
                Some(Live {
                    source: "remote:/s/claude.toml".into(),
                    remote: Some("2026.08.13.1".into()),
                    warning: Some("ignored override … because it could not be loaded".into()),
                })
            },
            &mut p,
            &mut n,
        );
        assert!(p.iter().any(|x| x.contains("being ignored by herdr")), "{p:?}");
    }

    #[test]
    fn an_override_the_installed_binary_never_opens_is_a_problem() {
        let dir = write(&[(
            "claude.toml",
            "# fault: x\n# base: 2026.08.13.1\n# remove-when: y\nid = \"claude\"\n",
        )]);
        let (mut p, mut n) = (vec![], vec![]);
        health(
            &[dir],
            |_| Some(live("remote:/s/claude.toml", "2026.08.13.1")),
            &mut p,
            &mut n,
        );
        assert!(p.iter().any(|x| x.contains("read by no herdr")), "{p:?}");
    }

    /// A directory of override files, named for the test that wrote it.
    fn write(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-detect-override-{}", util::epoch_nanos()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        for (name, body) in files {
            std::fs::write(dir.join(name), body).expect("write override");
        }
        dir
    }
}
