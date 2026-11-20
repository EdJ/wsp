//! What `sh` itself says of a line, asked where a step's command is written
//! (`wsp check --set`, `wsp worklist stop-checks`) and before a step runs: no
//! tree, no working directory, nothing executed. A step's run is a job
//! (`Host::run_job`, wsp-057); these two only read the line.

use std::process::Stdio;

use crate::calls::Command;

/// The first line of `sh -n`'s complaint about `command`, or `None` when it
/// parses (or `sh` would not run, which says nothing against the line).
pub fn parse_error(command: &str) -> Option<String> {
    let out = Command::new("sh").arg("-n").arg("-c").arg(command).stdin(Stdio::null()).output().ok()?;
    if out.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.lines().next().unwrap_or("it does not parse").trim();
    Some(said.trim_start_matches("sh: ").trim_start_matches("-c: ").to_string())
}

/// Whether `word` is a shell keyword or builtin, or a program on `PATH`:
/// `command -v`.
pub fn knows(word: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg("command -v \"$1\" >/dev/null 2>&1")
        .arg("sh")
        .arg(word)
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sh_says_what_does_not_parse_and_knows_its_builtins() {
        assert_eq!(parse_error("echo a; true"), None);
        assert!(parse_error("if then fi (").is_some());
        assert!(knows("cd") && knows("sh"));
        assert!(!knows("no-such-program-wsp-057"));
    }
}
