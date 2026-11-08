//! A row the run steps inside rather than seats whole: `docs/pieces.md`, *The run*,
//! "One predicate" (wsp-506).
//!
//! A row with `## Legs` or with `## Pieces` holds no claim between its parts by
//! design, so every repair that reads "doing, with no seat" as a start that never
//! took, or a member nobody is on, steps aside for either. This is the one place
//! that says which rows those are; what is about legs alone (their batons, a
//! skip that ends a row, a rework leg) still asks [`crate::legs::staged`].

use crate::model::Task;

/// Whether the run steps inside `t`: it has legs or pieces.
pub(crate) fn run_inside(t: &Task) -> bool {
    crate::legs::staged(t) || crate::pieces::staged(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_with_legs_or_pieces_is_run_inside_and_a_plain_one_is_not() {
        let row = |body: &str| {
            let mut t = Task::new("a row", "r-1");
            t.body = body.to_string();
            t
        };
        assert!(!run_inside(&row("## Overview\nx\n")));
        assert!(run_inside(&row("## Legs\n- a · pending\n")));
        assert!(run_inside(&row("## Pieces\ncap: 3\n")));
    }
}
