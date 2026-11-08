//! A member's or a leg's start that did not take, and when it is tried again:
//! `wsp-335`.
//!
//! `compound-956` (2026-11-02, load about 60 on 8 cores): `claude started but
//! never became ready for input`, and the leg stayed `running` on a pane that
//! was gone. Nothing reset it, the seat cap went on counting it, and a person
//! edited the store by hand. `wsp-328` had already taught the barrier check to
//! start again on a backoff and then flag the person; this is the same rule for
//! the two other things a run starts and waits on.
//!
//! **The count lives in `starts.json` and not on the row.** A member's row is
//! handed to every spawn and a leg's entry is the order its agent reads; how
//! many times the run missed is the run's own business. Keyed by the member's id,
//! the leg's label, or the piece's id (`<row>-<n>`, wsp-506), and local to the
//! machine as the seats are.
//!
//! **The backoff is the check's** ([`crate::barrier::wait_after`]): thirty
//! seconds after the first miss, two minutes after the second, and the third is the
//! last. A miss older than [`FORGOTTEN_AFTER`] starts the count again, so a
//! start that fails once a day is never "the third".
//!
//! A start whose tries are spent is [`held`] until a person asks for it again —
//! `wsp worklist advance <list>` or `wsp leg reset` — which is [`forgive`].

use serde_json::json;

use crate::store::Store;
use crate::util;

/// How long after its last miss a count is still the same trouble. The tries and
/// their waits are about ten minutes; an hour is well clear of them.
const FORGOTTEN_AFTER: i64 = 60 * 60;

/// What a miss left the start with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Missed {
    /// Starts that have not taken, this one included.
    pub tries: u32,
    /// This was the last try: the person is to be flagged, and only now —
    /// a count already spent says nothing again.
    pub last: bool,
    /// Seconds until the next start, while there is one.
    pub again_in: Option<i64>,
}

/// A start of `label` did not take. Counts it under the lock.
pub(crate) fn missed(store: &Store, label: &str) -> Missed {
    let now = util::epoch_secs();
    let mut out = Missed { tries: 1, last: false, again_in: None };
    store.update_starts(|m| {
        let before = m
            .get(label)
            .filter(|e| now - e["at"].as_i64().unwrap_or(0) <= FORGOTTEN_AFTER || e["spent"].as_bool() == Some(true))
            .map_or(0, |e| e["tries"].as_u64().unwrap_or(0) as u32);
        let tries = before + 1;
        let spent = tries >= crate::barrier::TRIES;
        out = Missed {
            tries,
            last: spent && before < crate::barrier::TRIES,
            again_in: crate::barrier::wait_after(tries),
        };
        m.insert(label.to_string(), json!({ "tries": tries, "at": now, "spent": spent }));
        m.retain(|_, e| e["spent"].as_bool() == Some(true) || now - e["at"].as_i64().unwrap_or(0) <= 24 * FORGOTTEN_AFTER);
    });
    out
}

/// Whether the run is to leave `label` alone now: its next try is not yet due,
/// or its tries are spent.
pub(crate) fn held(store: &Store, label: &str) -> bool {
    let starts = store.starts();
    let Some(e) = starts.get(label) else { return false };
    if e["spent"].as_bool() == Some(true) {
        return true;
    }
    let tries = e["tries"].as_u64().unwrap_or(0) as u32;
    let wait = crate::barrier::wait_after(tries).unwrap_or(0);
    util::epoch_secs() - e["at"].as_i64().unwrap_or(0) <= wait
}

/// Forget `label`'s misses, so its next start is the first. Whether it had any.
pub(crate) fn forgive(store: &Store, label: &str) -> bool {
    let mut had = false;
    store.update_starts(|m| had = m.remove(label).is_some());
    had
}

/// Forget the misses of those of `labels` whose tries are spent: what
/// `wsp worklist advance` does for a list it is asked to try again.
pub(crate) fn forgive_spent(store: &Store, labels: &[String]) -> usize {
    let mut n = 0;
    store.update_starts(|m| {
        for l in labels {
            if m.get(l).is_some_and(|e| e["spent"].as_bool() == Some(true)) {
                m.remove(l);
                n += 1;
            }
        }
    });
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        crate::testkit::isolated_store(tag)
    }

    fn age(store: &Store, label: &str, secs: i64) {
        store.update_starts(|m| {
            let e = m.get_mut(label).unwrap();
            e["at"] = json!(util::epoch_secs() - secs);
        });
    }

    #[test]
    fn a_start_that_missed_is_held_for_its_backoff_then_free_and_the_third_miss_is_the_last() {
        let (_env, store) = scratch("starts-backoff");
        assert!(!held(&store, "m-1"), "a start never missed is nobody's to hold");

        let one = missed(&store, "m-1");
        assert_eq!(one, Missed { tries: 1, last: false, again_in: Some(30) });
        assert!(held(&store, "m-1"));
        age(&store, "m-1", 4 * 60);
        assert!(!held(&store, "m-1"), "free once the wait is over");

        let two = missed(&store, "m-1");
        assert_eq!(two, Missed { tries: 2, last: false, again_in: Some(120) });
        age(&store, "m-1", 6 * 60);
        assert!(!held(&store, "m-1"));

        let three = missed(&store, "m-1");
        assert_eq!((three.tries, three.last, three.again_in), (3, true, None));
        age(&store, "m-1", 24 * 60 * 60);
        assert!(held(&store, "m-1"), "spent is held however long it waits");
        assert!(!missed(&store, "m-1").last, "and says nothing a second time");
    }

    #[test]
    fn a_miss_from_long_ago_is_not_the_same_trouble() {
        let (_env, store) = scratch("starts-forgotten");
        missed(&store, "m-1");
        missed(&store, "m-1");
        age(&store, "m-1", 2 * 60 * 60);
        assert_eq!(missed(&store, "m-1").tries, 1);
    }

    #[test]
    fn a_person_asking_again_clears_what_is_spent_and_only_that() {
        let (_env, store) = scratch("starts-forgive");
        for _ in 0..3 {
            missed(&store, "m-1");
        }
        missed(&store, "m-2");
        let cleared = forgive_spent(&store, &["m-1".into(), "m-2".into(), "m-3".into()]);
        assert_eq!(cleared, 1);
        assert!(!held(&store, "m-1") && held(&store, "m-2"), "m-2 is still inside its wait");
        assert!(forgive(&store, "m-2") && !forgive(&store, "m-2"));
    }
}
