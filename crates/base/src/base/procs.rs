//! The processes on this machine, as `ps` lists them: the reading of its
//! output. The `ps` itself is the host's (`wsp_wire::host::Host::table`, and
//! its `processes` and `parents`), so this module runs nothing.

/// A `ps -A -E -o pid=,ppid=,command=` table as `(pid, ppid, the rest of the
/// line)` — where "the rest" includes the environment.
///
/// Shared by everything that asks the same output the same two questions —
/// what is running, and which store its environment points it at — which would
/// otherwise keep second copies of the parsing that was already silently wrong
/// once (see [`ps_line`]).
pub fn read_processes(table: &str) -> Vec<(u32, u32, String)> {
    table.lines().filter_map(ps_line).collect()
}

/// A `ps -A -o pid=,ppid=` table as `(pid, ppid, "")`: the parent links alone,
/// for a caller that only walks [`ancestors`].
pub fn read_parents(table: &str) -> Vec<(u32, u32, String)> {
    table
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(|x| x.parse::<u32>().ok());
            Some((it.next()??, it.next()??, String::new()))
        })
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

/// What a finished `ps -o lstart= -p <pid>` says. It says there is no such
/// process by exiting 1 with nothing on stdout (stderr is free text, and a pid
/// too large for the system gets a complaint there with the same exit). Any
/// other end — killed by a signal, or another exit status — says nothing about
/// the pid, so it is `Err` and a live run is not read as dead on it. A `ps` that
/// itself fails with exit 1 and prints nothing cannot be told from a missing
/// pid, and reads as gone: the lock (`open_unless_held`) is what covers that.
pub fn ps_says(code: Option<i32>, stdout: &[u8]) -> Result<Option<String>, ()> {
    if code != Some(0) && !(code == Some(1) && stdout.iter().all(u8::is_ascii_whitespace)) {
        return Err(());
    }
    let s = String::from_utf8_lossy(stdout);
    let s = s.split_whitespace().collect::<Vec<_>>().join("_");
    Ok((!s.is_empty()).then_some(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `backend session stop` ends the server, not what it started: the daemon it
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
            ps_line("  1628     1 /Users/e/wsp daemon WSP_BACKEND_SESSION=wsp-w24"),
            Some((
                1628,
                1,
                "/Users/e/wsp daemon WSP_BACKEND_SESSION=wsp-w24".to_string()
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
