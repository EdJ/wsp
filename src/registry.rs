//! Every module that has verbs, in one list. `main` looks a word up here and
//! the help is assembled from the same entries.

/// Every module that has verbs.
pub const ALL: &[&[crate::verbs::Verb]] = &[
    crate::after::VERBS,
    crate::artefact::VERBS,
    crate::check::VERBS,
    crate::cmd_agent::VERBS,
    crate::cmd_allocate::VERBS,
    crate::cmd_whoami::VERBS,
    crate::cmd_attempts::VERBS,
    crate::cmd_attend::VERBS,
    crate::cmd_brief::VERBS,
    crate::cmd_burn::VERBS,
    crate::spend::VERBS,
    crate::cmd_calls::VERBS,
    crate::cmd_checkout::VERBS,
    crate::cmd_govern::VERBS,
    crate::cmd_install::VERBS,
    crate::cmd_login::VERBS,
    crate::cmd_machine::VERBS,
    crate::cmd_mandate::VERBS,
    crate::cmd_message::VERBS,
    crate::cmd_migrate::VERBS,
    crate::cmd_project::VERBS,
    crate::cmd_publish::VERBS,
    crate::cmd_initiative::VERBS,
    crate::cmd_resume::VERBS,
    crate::cmd_role::VERBS,
    crate::cmd_sandbox::VERBS,
    crate::cmd_spawn::VERBS,
    crate::cmd_stamp::VERBS,
    crate::cmd_task::VERBS,
    crate::cmd_submit::VERBS,
    crate::cmd_verifications::VERBS,
    crate::cmd_verify::VERBS,
    crate::cmd_watch::VERBS,
    crate::cmd_worklist::VERBS,
    crate::cold::VERBS,
    crate::daemon::VERBS,
    crate::detail::VERBS,
    crate::kanban::VERBS,
    crate::legs::VERBS,
    crate::packet::VERBS,
    crate::cmd_piece::VERBS,
    crate::panel::VERBS,
    crate::place_super::VERBS,
    crate::retro::VERBS,
    crate::scratch::VERBS,
    crate::stuck::VERBS,
    crate::throttle::VERBS,
    crate::timeline::VERBS,
    crate::verification::VERBS,
];

pub fn find(word: &str) -> Option<&'static crate::verbs::Verb> {
    ALL.iter().flat_map(|vs| vs.iter()).find(|v| v.names.contains(&word))
}

/// Every word, alias or not, that dispatches.
pub fn words() -> Vec<&'static str> {
    ALL.iter().flat_map(|vs| vs.iter()).flat_map(|v| v.names.iter().copied()).collect()
}

/// The help between the opening line and the closing notes: each section's
/// heading (through `heading`), then its entries in `order`.
pub fn help_sections(heading: &dyn Fn(&str) -> String, classes: &str) -> String {
    let mut all: Vec<&crate::verbs::Help> = ALL.iter().flat_map(|vs| vs.iter()).flat_map(|v| v.help.iter()).collect();
    all.sort_by_key(|h| (h.section, h.order));
    let mut out: Vec<String> = Vec::new();
    for section in crate::verbs::Section::EVERY {
        let mut part = vec![heading(section.heading())];
        part.extend(all.iter().filter(|h| h.section == section).map(|h| h.text.replace("{classes}", classes)));
        out.push(part.join("\n"));
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use crate::agent_commands::of;
    use crate::cmd_spawn::Reach;

    /// The exit is `wsp submit` (docs/exit-packet.md §3), and every packet ends
    /// by telling the seat to run it. A seat refused it could not leave: its
    /// turn would stop with no exit accepted, and closing for the seat would be
    /// the only way out. So the verb, and each of its aliases, is in the
    /// registry `wsp help` is built from, and no deny a spawned opencode is
    /// configured with reaches any of them.
    #[test]
    fn a_spawned_opencode_may_submit_its_exit_and_submit_is_in_the_help() {
        let exits = ["submit", "review", "verified", "leg done", "worklist go", "worklist hold"];
        let registered = |word: &str| super::ALL.iter().flat_map(|m| m.iter()).any(|v| v.names.contains(&word));
        for exit in exits {
            let head = exit.split_whitespace().next().unwrap();
            assert!(registered(head), "`wsp {head}` is a registered verb");
        }
        let submit = crate::cmd_submit::VERBS.iter().find(|v| v.names.contains(&"submit")).expect("submit is registered");
        assert!(
            submit.help.iter().any(|h| h.section == crate::verbs::Section::Tasks && h.text.trim_start().starts_with("wsp submit <id>")),
            "`wsp help` lists submit under TASKS"
        );

        let env = of("opencode").env(None, &Reach::default());
        let v: Value = serde_json::from_str(env.get("OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        let denies: Vec<String> = v["permission"]["bash"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, a)| *a == "deny")
            .map(|(p, _)| p.trim_end_matches('*').to_string())
            .collect();
        assert!(!denies.is_empty(), "the denies are still there to check against");
        for exit in exits {
            let line = format!("wsp {exit} wsp-618 --dry-run");
            assert!(!denies.iter().any(|d| line.starts_with(d.as_str())), "`{line}` meets a deny in {denies:?}");
        }
    }
}
