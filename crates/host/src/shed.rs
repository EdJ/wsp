//! What a spawned seat must not inherit from whoever spawned it. Moved down
//! from `wsp-wire`'s `place` (wsp-020), which re-exports it: the in-process
//! host strips these from every process it starts.

/// The marker that turns a spawned Claude Code into somebody else's child, and
/// the one variable measured to break a spawn outright.
///
/// A session that finds `CLAUDE_CODE_CHILD_SESSION` in its environment believes
/// it is a sub-session of the one that set it and **writes no transcript at
/// all**. The only evidence is one line in the pane, truncated where it matters:
/// `⚠ Transcript saving is off — inherited CLAUDE_CODE…`. Nothing fails, nothing
/// is logged, and an agent spawned this way works perfectly and leaves no
/// record. Claude Code knows the leak class — it probes `tmux show-environment`
/// to tell a real marker from one that is merely ambient in the multiplexer —
/// and backend is not tmux, so under backend the marker is always believed.
///
/// Measured 2026-08-17 against Claude Code 2.1.233, three panes of one sandbox,
/// each started with `agent.start` and prompted:
///
/// | pane | what it inherited | transcript |
/// |---|---|---|
/// | as spawned today | the caller's whole session | **none** |
/// | this one name emptied | — | 36,752 bytes |
/// | every name in [`shed`] emptied | — | 36,882 bytes |
///
/// So it is shed whether or not the caller has it, unlike the rest: the leak
/// does not need wsp to be the carrier. A backend server started by hand from
/// inside an agent's pane hands the marker to every seat it will ever open, and
/// a `wsp` run from a clean shell against that server can see nothing wrong with
/// its own environment to strip.
pub const CHILD_MARKER: &str = "CLAUDE_CODE_CHILD_SESSION";

/// What a seat's occupant must **not** find: a variable that names the Claude
/// Code session the caller is sitting in, rather than anything about the seat.
///
/// The other half of `SEAT_ENV`. That one is the handle a spawn owes the
/// child; this is the identity a spawn owes it *not* to pass on, and both are
/// facts about placing work rather than about any backend.
///
/// A prefix rather than the four names anybody could list today, for the reason
/// `cmd_sandbox::forgettable` gives about `WSP_BACKEND_`: a variable Claude Code adds
/// next month is a fact about the caller's session too, and this defect exists
/// precisely because [`CHILD_MARKER`] was a name nobody knew to unset. What the
/// prefix catches beyond the marker is worth naming, because none of it is
/// harmless: `CLAUDE_CODE_SESSION_ID` is the caller's own session id, and
/// `CLAUDE_CODE_MESSAGING_SOCKET` and `..._TOKEN` are the caller's control
/// channel and the credential for it, handed to an unrelated agent.
///
/// **Every `CLAUDE_` name is shed except the ones [`KEPT`] lists**, rather than
/// the prefix plus whatever names somebody has found. The named list was
/// always one release behind: `CLAUDE_PID` was added by name because the
/// control socket is `/tmp/cc-socks/<CLAUDE_PID>.sock`, and then
/// `CLAUDE_JOB_DIR` leaked for a day unnoticed (`wsp-130`). It names the
/// caller's background job, so every seat a job-spawned governor opened showed
/// that job's name in its status bar instead of its own `-n`, and recorded its
/// exit in the dead governor's job. `CLAUDE_EFFORT` went the same way: it is the
/// caller's live effort level, so a seat spawned without `--effort` ran at its
/// spawner's rather than its settings' default.
///
/// The inversion is argued from which failure is worse. Every `CLAUDE_` name
/// found in a spawning agent's environment (2026-09-30: `CLAUDECODE`,
/// `CLAUDE_PID`, `CLAUDE_JOB_DIR`, `CLAUDE_EFFORT`, seven `CLAUDE_CODE_*`) was
/// put there by the Claude Code that ran `wsp`, and describes that session. A
/// leak is silent — the marker costs the transcript, the job dir costs the
/// exit record, and nothing fails. Over-shedding a genuine preference empties
/// it on the seat, which shows on the first spawn and is repaired by one name
/// in [`KEPT`]. A setting like `CLAUDE_CONFIG_DIR` says where this machine
/// keeps its configuration and is *supposed* to be inherited; that is what the
/// list is for. Preferences are kept by name; everything else is identity.
pub fn shed(key: &str) -> bool {
    let claude = (key == "CLAUDECODE" || key.starts_with("CLAUDE_")) && !KEPT.contains(&key);
    claude || PI_SESSION.contains(&key)
}

/// What pi's bash tool hands every command it runs: the pi session that ran
/// it. Shed for the reason the `CLAUDE_` names are — it describes the caller,
/// not the seat — and for one more: `pi/wsp-session.ts` reads `PI_SESSION_ID`
/// as *this pi was started by another pi's shell* and stays silent, so a pi
/// governor's spawn that passed it on would start a member that never reports.
/// By name rather than by prefix, because pi's other `PI_` names
/// (`PI_CODING_AGENT_DIR`, `PI_OFFLINE`, …) are this machine's preferences and
/// are meant to be inherited.
pub const PI_SESSION: &[&str] =
    &["PI_SESSION_ID", "PI_SESSION_FILE", "PI_PROVIDER", "PI_MODEL", "PI_REASONING_LEVEL"];

/// The `CLAUDE_` names a seat inherits: preferences about this machine, not
/// facts about the session that spawned it. [`shed`] says why it is this way
/// round.
pub const KEPT: &[&str] = &["CLAUDE_CONFIG_DIR"];

/// The same rule against this process's environment: what a caller would have
/// to `unset` before spawning by hand, which is the workaround this replaces.
///
/// [`CHILD_MARKER`] is always in the list and the rest is only what is actually
/// here, so a spawn from a shell with none of this in it is one name long.
///
/// **It enumerates the live process environment, so no two of its results may
/// be compared to each other** — nor two of anything built on it, which is
/// every `shed_env` and every `crate::cmd_spawn::seat_env`. cargo runs
/// tests as threads in one process and several of them set `CLAUDE_CODE_*`
/// names (`place_super`, `cmd_sandbox`); one landing between two calls puts a
/// key in one result and not the other. `robustness-099` was such an assertion
/// in `cmd_spawn`, and what it cost was not itself: it was green in four debug
/// runs, red under `--release`, and passed alone — this project's signature for
/// a real ordering bug, spent on a test that had none, on the run an install is
/// gated on. Assert a property of one result, or the contribution of the thing
/// actually under test; never one snapshot against another.
pub fn shed_keys() -> Vec<String> {
    let mut keys: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| shed(k) && k != CHILD_MARKER)
        .collect();
    keys.sort();
    keys.insert(0, CHILD_MARKER.into());
    keys
}
