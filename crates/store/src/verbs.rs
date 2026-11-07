//! The verb registry: what `wsp <word>` runs, and the help each word carries.
//!
//! Every `cmd_*` module exports `VERBS`, built from the types here; `registry.rs` lists the modules. `main`
//! looks the word up here and runs it; the help is assembled from the same
//! entries, so adding a verb is an edit to the module that owns it and one line
//! in `registry::ALL` — never to the dispatch or the help text in `main.rs`.

use crate::store::Store;
use crate::base::args::Args;

/// The part of the help a line sits under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Projects,
    Tasks,
    Agents,
    Machines,
    Worklists,
    Plumbing,
}

impl Section {
    pub const EVERY: [Section; 6] = [
        Section::Projects,
        Section::Tasks,
        Section::Agents,
        Section::Machines,
        Section::Worklists,
        Section::Plumbing,
    ];

    pub fn heading(self) -> &'static str {
        match self {
            Section::Projects => "PROJECTS",
            Section::Tasks => "TASKS",
            Section::Agents => "AGENTS",
            Section::Machines => "MACHINES",
            Section::Worklists => "WORKLISTS",
            Section::Plumbing => "PLUMBING",
        }
    }
}

/// One entry of the help: the usage line and the prose under it, as printed.
///
/// `order` places it inside its section, across all the modules; it is spaced by
/// ten so that a new entry goes between two old ones without touching them. The
/// text may hold `{classes}`, which is filled in with the watch signal classes.
pub struct Help {
    pub section: Section,
    pub order: u32,
    pub text: &'static str,
}

/// A word `wsp` answers to, with every alias it has.
pub struct Verb {
    pub names: &'static [&'static str],
    pub run: fn(&Store, &Args) -> i32,
    pub help: &'static [Help],
}
