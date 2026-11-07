//! The processes on this machine, as `ps` lists them.

use std::process::Command;

/// Every process this machine is running, as `(pid, ppid, the rest of the
/// line)` — where "the rest" includes the environment.
///
/// `ps -E` is how a process's environment is readable without a dependency or a
/// `/proc`, and the environment is the only place a plugin's child says which
/// herdr started it: its command line is `wsp daemon`, identical to the live
/// one's. `ppid` comes back in the same call so the ancestors of this process
/// can be ruled out of anything we are about to kill — a shell whose command
/// line happens to mention the session is not a child of it.
///
/// Shared with `daemon`, which asks the same output the same two
/// questions — what is running, and which store its environment points it at —
/// and would otherwise keep a second copy of the parsing that was already
/// silently wrong once (see [`ps_line`]).
pub fn processes() -> Vec<(u32, u32, String)> {
    let Ok(out) = Command::new("ps")
        .args(["-A", "-E", "-o", "pid=,ppid=,command="])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(ps_line)
        .collect()
}

/// One line of that, as `(pid, ppid, the rest)`.
///
/// Its own function because it is the part that was wrong, and wrong silently:
/// `ps` right-aligns the numeric columns, so a `splitn` on whitespace found an
/// empty second field, failed to parse it and dropped **every** line. The
/// teardown then reaped nothing and said nothing, which is precisely the shape
/// of the defect it was written to fix.
fn ps_line(line: &str) -> Option<(u32, u32, String)> {
    let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
    let (ppid, rest) = rest.trim_start().split_once(char::is_whitespace)?;
    Some((
        pid.parse().ok()?,
        ppid.parse().ok()?,
        rest.trim_start().to_string(),
    ))
}

/// This process and everything above it, so nothing we are standing on can end
/// up in a list of things to kill.
pub fn ancestors(all: &[(u32, u32, String)]) -> Vec<u32> {
    let mut out: Vec<u32> = vec![std::process::id()];
    // Bounded by the list itself, so a cycle cannot spin.
    for _ in 0..all.len() {
        let Some(&(_, ppid, _)) = out
            .last()
            .and_then(|p| all.iter().find(|(pid, _, _)| pid == p))
        else {
            break;
        };
        if ppid == 0 || out.contains(&ppid) {
            break;
        }
        out.push(ppid);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `herdr session stop` ends the server, not what it started: the daemon it
    /// left was reparented to launchd and went on holding the live store. The
    /// environment is the only handle — the command line is `wsp daemon`, the
    /// same as the real one's — so the match has to be exact at the end of the
    /// name, and it must never include a process we are standing on.
    /// `ps` right-aligns its numeric columns, and the first version of this
    /// split on every run of whitespace — so the second field came back empty,
    /// the parse failed, and every line was dropped. A teardown that reaps
    /// nothing looks exactly like a teardown with nothing to reap: it printed
    /// "torn down" over a live daemon, which is the defect verbatim. Pinned
    /// against real output rather than a tidy invention.
    #[test]
    fn a_padded_ps_line_is_still_a_process() {
        assert_eq!(
            ps_line("  1628     1 /Users/e/wsp daemon HERDR_SESSION=wsp-w24"),
            Some((
                1628,
                1,
                "/Users/e/wsp daemon HERDR_SESSION=wsp-w24".to_string()
            ))
        );
        assert_eq!(
            ps_line("22121 38749 /Users/e/.local/bin/wsp daemon -v"),
            Some((
                22121,
                38749,
                "/Users/e/.local/bin/wsp daemon -v".to_string()
            ))
        );
        assert_eq!(
            ps_line("  PID  PPID COMMAND"),
            None,
            "a header parsed as a process"
        );
        assert_eq!(ps_line(""), None);
    }
}
