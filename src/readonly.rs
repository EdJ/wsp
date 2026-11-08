//! Which verbs write nothing (`wsp-488`): the one place that says so.
//!
//! `wsp calls` keeps polls apart from actions (`docs/calls.md`). A call is a poll
//! when its caller says so (`WSP_POLL=1`), or else when it is one of the
//! read-only verbs below *and* run by no seat. A seat reading `wsp show` is an
//! agent working, and its call is worth reading one by one; compound's host
//! reading `wsp wip --json` every half minute is a rate.
//!
//! "Read-only" means no write to the store (`~/wsp`) or to the state directory
//! beyond the call log itself. A verb that only *sometimes* writes is an action:
//! the table errs toward the action log, whose cost is a poll that shows up in
//! the one-by-one view, rather than an act that is folded into a rate.
//!
//! Every word the registry dispatches must be in exactly one of the lists;
//! `every_verb_is_classified` fails on a new verb until it is.

/// Verbs that never write.
pub const READ_ONLY: &[&str] = &[
    "wip",      // what every seat is doing: the host's main poll
    "ls",       // the task list
    "find",     // search the tasks
    "show",     // one task
    "inbox",    // what is waiting for the person
    "stamp",    // has anything changed
    "where",    // which seat am I
    "peek",     // another seat's screen
    "overlap",  // files two tasks both touch
    "timeline", // the record, drawn
    "tree",     // the project tree
    "projects", // the project list
    "burn",     // token cost
    "calls",    // this log
];

/// Verbs that write, or that write for some of their forms.
#[allow(dead_code)] // read by the test; the list is the record
pub const ACTIONS: &[&str] = &[
    "add", "decide", "note", "start", "block", "park", "review", "reopen", "todo", "mv", "tag", "ref", "prio", "next", "edit",
    "rename", "caption", "rm", "archive", "ask", "answer", "yours", "ack", "watch", "verified", "check", "spawn", "despawn",
    "attend", "claim", "release", "pin", "unpin", "agent", "sync", "hook", "doctor", "adopt", "say", "tell", "flag", "reconcile",
    "attempts", "leg", "install", "daemon", "brief", "commit-help", "machine", "migrate", "code", "retro", "packet", "sandbox",
    "mandate", "cold", "after", "scratch", "artefact", "done", "reap", "stuck", "verifications", "panel", "surface", "init",
    "project", "resume", "govern", "view", "kanban", "checkout", "land", "verify", "report", "throttle",
];

/// Verbs that write except for the listed sub-verbs.
pub const MIXED: &[(&str, &[&str])] = &[("worklist", &["ls", "show"])];

/// The name the registry lists first for a word (`wl` → `worklist`).
fn canonical(word: &str) -> &str {
    crate::registry::find(word).and_then(|v| v.names.first().copied()).unwrap_or(word)
}

/// Whether `wsp <verb> <sub>` is read-only. `sub` is the documented sub-verb
/// `sub_of` reduced the command line to, or empty.
pub fn is_read_only(verb: &str, sub: &str) -> bool {
    let v = canonical(verb);
    if READ_ONLY.contains(&v) {
        return true;
    }
    MIXED.iter().any(|(m, subs)| *m == v && subs.contains(&sub))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_is_classified() {
        let mut unclassified = Vec::new();
        for vs in crate::registry::ALL {
            for v in vs.iter() {
                let first = v.names[0];
                let n = READ_ONLY.contains(&first) as u8 + ACTIONS.contains(&first) as u8 + MIXED.iter().any(|(m, _)| *m == first) as u8;
                if n != 1 {
                    unclassified.push(format!("{first} (listed {n} times)"));
                }
            }
        }
        assert!(unclassified.is_empty(), "classify these in src/readonly.rs: {unclassified:?}");
    }

    #[test]
    fn nothing_listed_is_not_a_verb() {
        let firsts: Vec<&str> = crate::registry::ALL.iter().flat_map(|vs| vs.iter()).map(|v| v.names[0]).collect();
        for w in READ_ONLY.iter().chain(ACTIONS.iter()).chain(MIXED.iter().map(|(m, _)| m)) {
            assert!(firsts.contains(w), "`{w}` is listed in src/readonly.rs but is no verb's first name");
        }
    }

    #[test]
    fn aliases_and_sub_verbs_follow_their_verb() {
        assert!(is_read_only("status", ""));
        assert!(is_read_only("list", ""));
        assert!(is_read_only("wl", "ls"));
        assert!(is_read_only("worklist", "show"));
        assert!(!is_read_only("worklist", "go"));
        assert!(!is_read_only("worklist", ""));
        assert!(!is_read_only("tell", ""));
        assert!(!is_read_only("spawn", ""));
        assert!(!is_read_only("no-such", ""));
    }

    /// The sub-verbs named here are ones the help documents, which is what
    /// `sub_of` reduces a command line to; an undocumented one would never match.
    #[test]
    fn the_read_only_sub_verbs_are_documented() {
        let help: String = crate::registry::ALL
            .iter()
            .flat_map(|vs| vs.iter())
            .flat_map(|v| v.help.iter())
            .map(|h| h.text)
            .collect::<Vec<_>>()
            .join("\n");
        for (m, subs) in MIXED {
            for s in *subs {
                let want = format!("wsp {m} {s}");
                assert!(help.lines().any(|l| l.trim_start().starts_with(&want)), "`{want}` is not in the help");
            }
        }
    }
}
