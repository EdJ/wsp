//! The one writer of `## Decisions`.
//!
//! Every edit a caller makes to the section goes through here: a decision is
//! appended to a task's or a project's body with an identity, and the id it was
//! given comes back. The format itself stays in `model` (`append_decision`).

use crate::model::{Project, Task};

/// Append a decision to a task's body; returns the id it was given.
pub fn decide_task(t: &mut Task, text: &str, supersedes: &[String]) -> String {
    crate::model::append_decision(&mut t.body, text, supersedes)
}

/// Append a decision to a project's body; returns the id it was given.
pub fn decide_project(p: &mut Project, text: &str, supersedes: &[String]) -> String {
    crate::model::append_decision(&mut p.body, text, supersedes)
}
