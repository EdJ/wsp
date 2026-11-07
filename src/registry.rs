//! Every module that has verbs, in one list. `main` looks a word up here and
//! the help is assembled from the same entries.

/// Every module that has verbs.
pub const ALL: &[&[crate::verbs::Verb]] = &[
    crate::after::VERBS,
    crate::artefact::VERBS,
    crate::check::VERBS,
    crate::cmd_agent::VERBS,
    crate::cmd_attempts::VERBS,
    crate::cmd_attend::VERBS,
    crate::cmd_brief::VERBS,
    crate::cmd_burn::VERBS,
    crate::cmd_checkout::VERBS,
    crate::cmd_govern::VERBS,
    crate::cmd_install::VERBS,
    crate::cmd_machine::VERBS,
    crate::cmd_mandate::VERBS,
    crate::cmd_message::VERBS,
    crate::cmd_migrate::VERBS,
    crate::cmd_project::VERBS,
    crate::cmd_resume::VERBS,
    crate::cmd_role::VERBS,
    crate::cmd_sandbox::VERBS,
    crate::cmd_spawn::VERBS,
    crate::cmd_stamp::VERBS,
    crate::cmd_task::VERBS,
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
