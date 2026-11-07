//! The one writer of a task's tags. `wsp tag` calls [`adjust`]; no other module
//! edits the field.

use crate::model::Task;

/// Apply `+tag`, `-tag` and bare `tag` changes in order. Returns the tags a
/// removal named that the task did not carry.
pub(crate) fn adjust(t: &mut Task, changes: &[String]) -> Vec<String> {
    let mut absent: Vec<String> = Vec::new();
    for c in changes {
        if let Some(add) = c.strip_prefix('+') {
            if !add.is_empty() && !t.tags.iter().any(|x| x == add) {
                t.tags.push(add.to_string());
            }
        } else if let Some(rm) = c.strip_prefix('-') {
            if !t.tags.iter().any(|x| x == rm) {
                absent.push(rm.to_string());
            }
            t.tags.retain(|x| x != rm);
        } else if !t.tags.iter().any(|x| x == c) {
            t.tags.push(c.clone());
        }
    }
    absent
}
