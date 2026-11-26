//! The daemon starts the first planned list that may start, and says why then
//! (`wsp-384`).
//!
//! Third of three pieces of `wsp-381`. The first gave a list a status, the
//! second ([`crate::waits`]) made "what a planned list waits on" one answer in
//! queue order, and this acts on it. **Setting a list `planned` is Ed's
//! approval to run it**; what is left for wsp is to notice that there is room
//! and to say it did.
//!
//! # Where and how often
//!
//! On the repair tick ([`crate::repair::tick`]), once a minute, **one list per
//! tick**. One because the room a start takes is not visible until its agents
//! are seated: a second start in the same pass would be judged against room the
//! first has already spent, and the next tick reads the machine as it is.
//!
//! # What may be started, and where that is guarded
//!
//! Only a list whose status is `planned` — not `init`, `executing`, `complete`,
//! `closed` or `cancelled`. **That is asked three times, on purpose.** The
//! queue only ever contains planned lists. The list is re-read from the store
//! just before the start, because the queue was computed from a listing taken
//! a moment before. And the start itself is `go --if-planned`, which asks again
//! at the write, under the store's lock ([`crate::cmd_worklist::start_planned`]),
//! so a person who set the list back to `init` while this was deciding wins and
//! nothing is started. The daemon never calls a start that does not carry the
//! guard.
//!
//! # What it tells, and where
//!
//! The reason is a sentence — when the list was planned, what it had been
//! waiting on (as [`note_waits`] last saw it), how many places were free and
//! what its first group takes, whether the load held builds back. It goes to
//! three places and they are different readers:
//!
//! - **the list's own log**, as the sentence of its `started` line (it is the
//!   verdict `go` records, and the caption is its short form);
//! - **`cycle.log`**, which is how a governor reads what the run has been doing;
//! - **`wsp wip`**, for a day afterwards ([`wip_lines`]), because Ed is the one
//!   who set `planned` and is not watching a log when it happens.
//!
//! # What it remembers
//!
//! `pickup.json` in the state directory, and not the daemon's memory, because
//! the daemon `exec`s itself when an install lands. `waiting` is what each
//! planned list was last held by; `started` is the last few starts, for `wip`;
//! `refused` is a list `go` turned away, so the same refusal is not made, and
//! not said, every minute.

use serde_json::{json, Value};

use crate::model::{Worklist, WorklistStatus};
use crate::store::Store;
use crate::util;
use crate::waits::{self, Machine, Prio, Row};
use crate::worklist;

/// How long a list `go` refused is left alone before it is tried again. The
/// refusals are about the list's own members (one is in another run), which a
/// person fixes in minutes or hours, and trying every minute would print the
/// same complaint on the daemon's stderr that often.
const REFUSED_FOR: i64 = 15 * 60;

/// How every sentence the pickup writes begins; `wip` leaves it off, where the
/// line already says a planned list was started.
const STARTED_BY: &str = "started by wsp, the first planned list that may start: ";

/// How many starts are kept for `wip`.
const KEEP: usize = 20;

/// How long a start stays on `wip`'s page: a day, which is the longest Ed is
/// likely to be away from a machine that keeps working.
const SHOWN_FOR: i64 = 24 * 3600;

/// `go`'s code for "this is not a planned list any more", from the guard at the
/// write.
const NOT_PLANNED: i32 = 3;

/// Whether a list is one the pickup may start: stored as `planned`, and not yet
/// started. The one place the question is asked in this module.
fn still_planned(w: &Worklist) -> bool {
    w.is_planned() && w.status() == WorklistStatus::Draft
}

/// The daemon's pass. Returns the list it started, if it did.
pub fn tick(store: &Store) -> Option<String> {
    tick_on(store, None)
}

/// [`tick`] with the machine given, which is how the tests say what the
/// machine says. `None` reads it.
pub fn tick_on(store: &Store, machine: Option<Machine>) -> Option<String> {
    let lists = store.worklists();
    if !lists.iter().any(still_planned) {
        // The cheap, common case: no list is planned, and nothing is read but
        // the lists. A record kept for a list that is no longer planned goes.
        forget_except(store, &[]);
        return None;
    }
    let machine = machine.unwrap_or_else(|| Machine::now(store));
    let listed = worklist::listing(store, lists);
    let rows = waits::read_on(store, &listed, Some(machine));
    note_waits(store, &rows);

    for row in rows.iter().filter(|r| r.wait.is_none()) {
        if refused_lately(store, &row.id) {
            continue;
        }
        // Re-read, not taken from the listing: see the module's guard.
        let Some(w) = store.worklist(&row.id).filter(still_planned) else { continue };
        let seen = seen_of(store, &w.id);
        let why = why_started(&w, row, &machine, &seen);
        let caption = caption_for(&seen);
        match crate::cmd_worklist::start_planned(store, &w.id, &why, &caption) {
            0 => {
                told(store, &w.id, &why);
                return Some(w.id);
            }
            NOT_PLANNED => continue,
            code => refuse(store, &w.id, code),
        }
    }
    None
}

// ---- what a planned list was held by -----------------------------------

/// What a planned list was last seen waiting on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Seen {
    /// Each kind of wait it has had, in the order first seen: `room`,
    /// `after pre-prep`.
    kinds: Vec<String>,
    /// The latest, whole: `room: 2 slots short`.
    last: String,
}

fn seen_of(store: &Store, id: &str) -> Seen {
    let rec = store.pickup();
    let Some(v) = rec.get("waiting").and_then(|w| w.get(id)) else { return Seen::default() };
    Seen {
        kinds: v
            .get("kinds")
            .and_then(|k| k.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        last: v.get("last").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
    }
}

/// Write down what each planned list waits on, and drop the record of any list
/// that is no longer planned. Written only when it changed, so a list held by
/// the same thing for a night costs nothing after the first minute.
fn note_waits(store: &Store, rows: &[Row]) {
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    store.update_pickup(|m| {
        let mut waiting = m.get("waiting").and_then(|w| w.as_object()).cloned().unwrap_or_default();
        waiting.retain(|id, _| ids.contains(&id.as_str()));
        for r in rows {
            let Some(wait) = &r.wait else { continue };
            let mut kinds: Vec<Value> =
                waiting.get(&r.id).and_then(|v| v.get("kinds")).and_then(|k| k.as_array()).cloned().unwrap_or_default();
            let kind = wait.kind();
            if !kinds.iter().any(|k| k.as_str() == Some(kind.as_str())) {
                kinds.push(json!(kind));
            }
            waiting.insert(r.id.clone(), json!({ "kinds": kinds, "last": wait.text() }));
        }
        set_or_drop(m, "waiting", waiting);
    });
}

/// Drop the record of every list not in `keep`.
fn forget_except(store: &Store, keep: &[&str]) {
    let rec = store.pickup();
    let held = |k: &str| rec.get(k).and_then(|w| w.as_object()).is_some_and(|o| o.keys().any(|id| !keep.contains(&id.as_str())));
    if !held("waiting") && !held("refused") {
        return;
    }
    store.update_pickup(|m| {
        for key in ["waiting", "refused"] {
            let mut o = m.get(key).and_then(|w| w.as_object()).cloned().unwrap_or_default();
            o.retain(|id, _| keep.contains(&id.as_str()));
            set_or_drop(m, key, o);
        }
    });
}

fn set_or_drop(m: &mut serde_json::Map<String, Value>, key: &str, o: serde_json::Map<String, Value>) {
    if o.is_empty() {
        m.remove(key);
    } else {
        m.insert(key.into(), Value::Object(o));
    }
}

// ---- the sentence -------------------------------------------------------

/// The reason, as the list's `started` line carries it: one line, no markup a
/// shell would read, and what Ed wants to know — why *then*.
fn why_started(w: &Worklist, row: &Row, machine: &Machine, seen: &Seen) -> String {
    let mut s = format!("{STARTED_BY}planned ");
    s.push_str(&match w.planned.as_deref() {
        Some(at) => format!("{} ago", util::duration_human(util::since(at))),
        None => "by hand (no time recorded)".into(),
    });
    let prio = Prio::of(w);
    if prio != Prio::Normal {
        s.push_str(&format!(", priority {}", prio.word()));
    }
    if seen.kinds.is_empty() {
        s.push_str("; nothing was in its way");
    } else {
        s.push_str(&format!("; held by {} (last: {})", seen.kinds.join(", "), seen.last));
    }
    match (machine.free, machine.cap) {
        (Some(free), Some(cap)) => {
            if row.together {
                s.push_str(&format!("; {free} of {cap} places free, its first group takes {}", row.need))
            } else {
                s.push_str(&format!("; {free} of {cap} places free, its first group takes 1 to start (x{} as places free)", row.width))
            }
        }
        _ => s.push_str("; this seat has no cap on agents"),
    }
    if machine.throttled.is_none() {
        s.push_str("; load is fine");
    }
    s
}

/// The short form, under the caption's limit: what changed, from the last
/// thing the list was held by.
fn caption_for(seen: &Seen) -> String {
    let last = seen.kinds.last().map(String::as_str).unwrap_or("");
    match last {
        "" => "planned, nothing was in its way",
        "room" => "planned, and room came free",
        "throttle" => "planned, and the load eased",
        "manual group" | "no groups" => "planned, and its groups are ready",
        k if k.starts_with("after ") => "planned, and the list it follows is complete",
        k if k.starts_with("alone with ") => "planned, and the project's other list ended",
        _ => "planned, and nothing holds it now",
    }
    .to_string()
}

// ---- telling -------------------------------------------------------------

/// A start, said where it is read: `cycle.log`, and the record `wip` draws.
/// (The list's own log has it already, as `go`'s `started` line.)
fn told(store: &Store, id: &str, why: &str) {
    crate::store::log_line(store, &format!("{id}: {why}"));
    let at = util::now_iso();
    store.update_pickup(|m| {
        let mut started: Vec<Value> = m.get("started").and_then(|s| s.as_array()).cloned().unwrap_or_default();
        started.push(json!({ "at": at, "id": id, "why": why }));
        let over = started.len().saturating_sub(KEEP);
        started.drain(..over);
        m.insert("started".into(), Value::Array(started));
        for key in ["waiting", "refused"] {
            let mut o = m.get(key).and_then(|w| w.as_object()).cloned().unwrap_or_default();
            o.remove(id);
            set_or_drop(m, key, o);
        }
    });
}

/// `go` turned the list away: said once, and not tried again for a while.
fn refuse(store: &Store, id: &str, code: i32) {
    crate::store::log_line(
        store,
        &format!("{id}: planned and first in the queue, but go refused it (exit {code}) — daemon.log has the reason; tried again in {}", util::duration_human(REFUSED_FOR)),
    );
    let at = util::now_iso();
    store.update_pickup(|m| {
        let mut o = m.get("refused").and_then(|w| w.as_object()).cloned().unwrap_or_default();
        o.insert(id.into(), json!({ "at": at, "code": code }));
        m.insert("refused".into(), Value::Object(o));
    });
}

fn refused_lately(store: &Store, id: &str) -> bool {
    store
        .pickup()
        .get("refused")
        .and_then(|r| r.get(id))
        .and_then(|r| r.get("at"))
        .and_then(|a| a.as_str())
        .is_some_and(|at| util::since(at) < REFUSED_FOR)
}

/// The starts `wip` draws: what the pickup did in the last day, newest last.
pub fn wip_lines(store: &Store) -> Vec<String> {
    started_lately(store)
        .into_iter()
        .map(|(at, id, why)| {
            let why = why.strip_prefix(STARTED_BY).unwrap_or(&why);
            format!("started {} ago, planned list {id}: {why}", util::duration_human(util::since(&at)))
        })
        .collect()
}

/// `(at, list, why)` for each start inside [`SHOWN_FOR`].
pub fn started_lately(store: &Store) -> Vec<(String, String, String)> {
    let rec = store.pickup();
    let Some(all) = rec.get("started").and_then(|s| s.as_array()) else { return Vec::new() };
    all.iter()
        .filter_map(|v| {
            let at = v.get("at")?.as_str()?.to_string();
            let id = v.get("id")?.as_str()?.to_string();
            let why = v.get("why").and_then(|w| w.as_str()).unwrap_or_default().to_string();
            (util::since(&at) < SHOWN_FOR).then_some((at, id, why))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;
    use crate::waits::{Machine, Wait};
    use crate::base::args::Args;

    fn setup(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("pickup-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        // Nothing the pickup starts may run: the step `go` pokes is a child
        // process, and a test has no business starting agents.
        std::env::set_var("WSP_NO_CYCLE", "1");
        (env, store)
    }

    fn wl(store: &Store, argv: &[&str]) -> i32 {
        crate::cmd_worklist::dispatch(store, &Args::synth("worklist", argv, &[]))
    }

    fn flagged(store: &Store, argv: &[&str], flags: &[(&str, &str)]) -> i32 {
        crate::cmd_worklist::dispatch(store, &Args::synth("worklist", argv, flags))
    }

    fn member(store: &Store, id: &str) {
        let t = Task::new(id, id);
        store.save_task(&t).unwrap();
    }

    /// A list of one member, planned at `when` (the queue's tie-break).
    fn planned(store: &Store, slug: &str, member_id: &str, when: &str) {
        member(store, member_id);
        assert_eq!(wl(store, &["new", slug, slug]), 0);
        assert_eq!(wl(store, &["add", slug, member_id]), 0);
        assert_eq!(wl(store, &["status", slug, "planned"]), 0);
        let mut w = store.worklist(slug).unwrap();
        w.planned = Some(when.into());
        store.save_worklist(&w).unwrap();
    }

    fn word(store: &Store, slug: &str) -> String {
        let w = store.worklist(slug).unwrap();
        let l = worklist::listing(store, vec![w]);
        l[0].phase(store).status.word().to_string()
    }

    fn room(free: usize) -> Option<Machine> {
        Some(Machine { free: Some(free), cap: Some(8), throttled: None })
    }

    /// Two members and a verifier is three places for a list of two; a list of
    /// one takes two.
    #[test]
    fn two_planned_lists_start_one_after_the_other_as_slots_free() {
        let (_env, store) = setup("two");
        planned(&store, "first", "p-1", "2026-11-05T09:00:00Z");
        planned(&store, "second", "p-2", "2026-11-05T10:00:00Z");
        store.set_seat_cap(Some(8)).unwrap();

        // Room for both, and still only one a tick: the first in queue order.
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("first"));
        assert_eq!(word(&store, "first"), "executing");
        assert_eq!(word(&store, "second"), "planned", "one per tick");

        // The first has taken its places; none free is a place short.
        assert_eq!(tick_on(&store, room(0)), None);
        assert_eq!(word(&store, "second"), "planned");
        let seen = seen_of(&store, "second");
        assert_eq!(seen.kinds, ["room"]);
        assert_eq!(seen.last, "room: 1 slot short");

        // Slots free, and the second goes, and says what it had waited on.
        assert_eq!(tick_on(&store, room(6)).as_deref(), Some("second"));
        assert_eq!(word(&store, "second"), "executing");
        let log = store.worklist("second").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("started · started by wsp, the first planned list that may start"), "{log}");
        assert!(log.contains("held by room (last: room: 1 slot short)"), "{log}");
        assert!(log.contains("6 of 8 places free, its first group takes 1 to start (x1 as places free)"), "{log}");
        assert!(log.contains("planned, and room came free"), "the caption: {log}");

        // Told in wip, and nothing is left to start.
        let lines = wip_lines(&store);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains("planned list second") && lines[1].contains("held by room"), "{lines:?}");
        assert_eq!(tick_on(&store, room(8)), None);
        assert!(seen_of(&store, "second").kinds.is_empty(), "the record of a started list goes");
    }

    /// A list of three members in one x3 group, `together` or not.
    fn x3_list(store: &Store, slug: &str, together: bool) {
        planned(store, slug, &format!("{slug}-1"), "2026-11-05T09:00:00Z");
        for i in 2..=3 {
            let id = format!("{slug}-{i}");
            member(store, &id);
            assert_eq!(flagged(store, &["add", slug, &id], &[("group", "1")]), 0);
        }
        assert_eq!(flagged(store, &["group", slug, "1"], &[("parallel", "3")]), 0);
        if together {
            assert_eq!(flagged(store, &["group", slug, "1"], &[("together", "true")]), 0);
        }
        assert_eq!(store.worklist(slug).unwrap().groups()[0].together, together);
    }

    /// `wsp-584`: with one place of four free, an x3 first group not marked
    /// together starts its list, and the sentence says what it took.
    #[test]
    fn one_free_place_starts_an_x3_list() {
        let (_env, store) = setup("loose");
        store.set_seat_cap(Some(4)).unwrap();
        x3_list(&store, "loose", false);
        let one = Some(Machine { free: Some(1), cap: Some(4), throttled: None });
        assert_eq!(tick_on(&store, one).as_deref(), Some("loose"));
        assert_eq!(word(&store, "loose"), "executing");
        let log = store.worklist("loose").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("1 of 4 places free, its first group takes 1 to start (x3 as places free)"), "{log}");
    }

    /// The same list marked together is three short for the group and its
    /// verifier, and nothing starts.
    #[test]
    fn one_free_place_does_not_start_an_x3_list_marked_together() {
        let (_env, store) = setup("tied");
        store.set_seat_cap(Some(4)).unwrap();
        x3_list(&store, "tied", true);
        let one = Some(Machine { free: Some(1), cap: Some(4), throttled: None });
        assert_eq!(tick_on(&store, one), None);
        assert_eq!(word(&store, "tied"), "planned");
        let rows = waits::read_on(&store, &worklist::listing(&store, store.worklists()), one);
        assert_eq!((rows[0].need, rows[0].together), (4, true));
        assert_eq!(waits::line(&rows[0].wait), "room: 3 slots short");
        let all = Some(Machine { free: Some(4), cap: Some(4), throttled: None });
        assert_eq!(tick_on(&store, all).as_deref(), Some("tied"));
        let log = store.worklist("tied").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("4 of 4 places free, its first group takes 4"), "{log}");
    }

    #[test]
    fn a_list_after_another_waits_until_that_is_complete_and_the_one_behind_it_goes() {
        let (_env, store) = setup("after");
        planned(&store, "pre", "a-1", "2026-11-05T09:00:00Z");
        planned(&store, "late", "a-2", "2026-11-05T09:30:00Z");
        planned(&store, "free", "a-3", "2026-11-05T10:00:00Z");
        assert_eq!(wl(&store, &["after", "late", "pre"]), 0);

        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("pre"));
        // `late` waits for `pre` to be complete (it is executing), so the
        // next in queue order that may start goes in its place.
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("free"));
        assert_eq!(word(&store, "late"), "planned", "held by after pre");
        assert_eq!(tick_on(&store, room(8)), None);
        assert_eq!(seen_of(&store, "late").kinds, ["after pre"]);
    }

    #[test]
    fn a_list_of_the_next_increment_is_not_picked_while_the_one_before_stands_at_its_align() {
        let (_env, store) = setup("align");
        let mut prod = crate::model::Project::new("compound");
        prod.kind = "product".into();
        store.save_project(&prod).unwrap();
        let init = |rest: &[&str], flags: &[(&str, &str)]| crate::cmd_initiative::initiative(&store, &Args::synth("initiative", rest, flags));
        assert_eq!(init(&["add", "Plan"], &[("product", "compound")]), 0);
        planned(&store, "n1", "g-1", "2026-11-05T09:00:00Z");
        planned(&store, "n2", "g-2", "2026-11-05T09:30:00Z");
        assert_eq!(init(&["increment", "add", "init-1", "One"], &[("list", "n1")]), 0);
        assert_eq!(init(&["increment", "add", "init-1", "Two"], &[("list", "n2")]), 0);
        // n1 has finished: n2 is after nothing that is open, but n stands at its align.
        let mut n1 = store.worklist("n1").unwrap();
        n1.status_raw = "done".into();
        store.save_worklist(&n1).unwrap();

        assert_eq!(tick_on(&store, room(8)), None, "n+1 is held at the align");
        assert_eq!(word(&store, "n2"), "planned");
        let listed = worklist::listing(&store, store.worklists());
        let wait = crate::waits::read_on(&store, &listed, room(8)).into_iter().find(|r| r.id == "n2").unwrap().wait;
        assert_eq!(wait, Some(Wait::Align("align I1 of init-1".into())));
        assert_eq!(crate::waits::line(&wait), "waiting on align I1 of init-1");

        assert_eq!(init(&["align", "pass", "init-1", "I1", "good"], &[]), 0);
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("n2"), "the pass starts it");
    }

    #[test]
    fn alone_holds_the_right_list_back_while_another_on_its_project_executes() {
        let (_env, store) = setup("alone");
        let mut p = crate::model::Project::new("proj");
        p.name = "proj".into();
        store.save_project(&p).unwrap();
        planned(&store, "one", "l-1", "2026-11-05T09:00:00Z");
        planned(&store, "two", "l-2", "2026-11-05T10:00:00Z");
        planned(&store, "other", "l-3", "2026-11-05T11:00:00Z");
        for slug in ["one", "two"] {
            let mut w = store.worklist(slug).unwrap();
            w.project = Some("proj".into());
            store.save_worklist(&w).unwrap();
        }
        assert_eq!(flagged(&store, &["edit", "two"], &[("alone", "true")]), 0);

        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("one"));
        // `two` runs alone and `one` is executing on its project: held, and
        // `other` (another project) is not held back by it.
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("other"));
        assert_eq!(tick_on(&store, room(8)), None);
        assert_eq!(word(&store, "two"), "planned");
        assert_eq!(seen_of(&store, "two").kinds, ["alone with one"]);
    }

    /// `wsp-435`: an executing list parked at a manual group, with no seat of its
    /// own working, does not hold `alone`; once its group is one wsp runs again,
    /// it does.
    #[test]
    fn a_list_waiting_on_ed_does_not_hold_alone_until_it_resumes() {
        let (_env, store) = setup("alone-ed");
        let mut p = crate::model::Project::new("proj");
        p.name = "proj".into();
        store.save_project(&p).unwrap();
        planned(&store, "one", "e-1", "2026-11-05T09:00:00Z");
        planned(&store, "two", "e-2", "2026-11-05T10:00:00Z");
        planned(&store, "three", "e-3", "2026-11-05T11:00:00Z");
        for slug in ["one", "two", "three"] {
            let mut w = store.worklist(slug).unwrap();
            w.project = Some("proj".into());
            store.save_worklist(&w).unwrap();
            assert_eq!(flagged(&store, &["edit", slug], &[("alone", "true")]), 0);
        }
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("one"));
        assert_eq!(tick_on(&store, room(8)), None, "one is executing: two and three are held");
        assert_eq!(seen_of(&store, "two").kinds, ["alone with one"]);

        // `one` comes to a group only a person can do, nobody of it working.
        let set_agent = |agent: &str| {
            let mut w = store.worklist("one").unwrap();
            let mut gs = w.groups();
            gs[0].agent = agent.into();
            w.set_groups(&gs);
            store.save_worklist(&w).unwrap();
        };
        let was = store.worklist("one").unwrap().groups()[0].agent.clone();
        set_agent(crate::model::MANUAL);
        let listed = crate::worklist::listing(&store, store.worklists());
        let one = listed.iter().find(|l| l.list.id == "one").unwrap();
        assert!(one.phase(&store).flags.waiting_on_ed);
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("two"), "one waits on Ed and holds nothing");

        // `two` is running now, and holds `three` as `one` did.
        assert_eq!(tick_on(&store, room(8)), None);
        assert_eq!(seen_of(&store, "three").last, "alone: two executing");

        // Resumed, `one` holds the queue again: close `two` out of the way.
        set_agent(&was);
        let mut w = store.worklist("two").unwrap();
        w.set_status(crate::model::WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        assert_eq!(tick_on(&store, room(8)), None, "one is executing again, and three is held by it");
        assert_eq!(seen_of(&store, "three").last, "alone: one executing");
    }

    #[test]
    fn a_manual_group_stops_pickup_and_so_does_the_load() {
        let (_env, store) = setup("manual");
        planned(&store, "by-hand", "m-1", "2026-11-05T09:00:00Z");
        let mut w = store.worklist("by-hand").unwrap();
        let mut gs = w.groups();
        gs[0].agent = crate::model::MANUAL.into();
        w.set_groups(&gs);
        store.save_worklist(&w).unwrap();
        assert_eq!(tick_on(&store, room(8)), None);
        assert_eq!(word(&store, "by-hand"), "planned");
        assert_eq!(seen_of(&store, "by-hand").kinds, ["manual group"]);

        planned(&store, "hot", "m-2", "2026-11-05T10:00:00Z");
        let hot = Some(Machine { free: Some(8), cap: Some(8), throttled: Some(crate::throttle::sample_pause()) });
        assert_eq!(tick_on(&store, hot), None);
        assert_eq!(word(&store, "hot"), "planned");
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("hot"), "the load eased");
        let log = store.worklist("hot").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("planned, and the load eased"), "{log}");
    }

    /// Nothing but a planned list is ever started, whatever the queue says.
    #[test]
    fn an_init_executing_complete_closed_or_cancelled_list_is_never_started() {
        let (_env, store) = setup("never");
        for (slug, m) in [("i", "n-1"), ("e", "n-2"), ("c", "n-3"), ("x", "n-4")] {
            member(&store, m);
            assert_eq!(wl(&store, &["new", slug, slug]), 0);
            assert_eq!(wl(&store, &["add", slug, m]), 0);
        }
        // `i` stays an init list; `e` is executing; `c` is cancelled; `x` is closed.
        let mut e = store.worklist("e").unwrap();
        e.status_raw = "running".into();
        store.save_worklist(&e).unwrap();
        assert_eq!(wl(&store, &["status", "c", "planned"]), 0);
        assert_eq!(flagged(&store, &["status", "c", "cancelled"], &[("why", "no")]), 0);
        let mut x = store.worklist("x").unwrap();
        x.status_raw = "done".into();
        store.save_worklist(&x).unwrap();

        assert_eq!(tick_on(&store, room(8)), None);
        assert_eq!(word(&store, "i"), "init");
        assert_eq!(word(&store, "c"), "cancelled");
        assert_eq!(store.worklist("e").unwrap().status_raw, "running");

        // And the guard at the start itself, for a list that is not planned.
        assert_eq!(crate::cmd_worklist::start_planned(&store, "i", "why", "caption"), NOT_PLANNED);
        assert_eq!(word(&store, "i"), "init", "the guard refused the write");
        assert_eq!(crate::cmd_worklist::start_planned(&store, "c", "why", "caption"), 1, "a cancelled list is done");
        assert_eq!(word(&store, "c"), "cancelled");
    }

    #[test]
    fn a_list_set_back_to_init_is_not_started_and_go_by_hand_still_starts_an_init_list() {
        let (_env, store) = setup("init");
        planned(&store, "back", "b-1", "2026-11-05T09:00:00Z");
        assert_eq!(wl(&store, &["status", "back", "init"]), 0);
        assert_eq!(tick_on(&store, room(8)), None);
        assert_eq!(word(&store, "back"), "init");
        // `go` stays "plan and start now, whatever the queue".
        assert_eq!(wl(&store, &["go", "back", "go"]), 0);
        assert_eq!(word(&store, "back"), "executing");
    }

    #[test]
    fn the_priority_decides_which_of_two_ready_lists_goes_first() {
        let (_env, store) = setup("prio");
        planned(&store, "early", "r-1", "2026-11-05T09:00:00Z");
        planned(&store, "urgent", "r-2", "2026-11-05T12:00:00Z");
        assert_eq!(flagged(&store, &["edit", "urgent"], &[("prio", "high")]), 0);
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("urgent"));
        let log = store.worklist("urgent").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("priority high"), "{log}");
        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("early"));
    }

    #[test]
    fn a_list_go_refuses_is_said_once_and_the_one_behind_it_still_goes() {
        let (_env, store) = setup("refused");
        // `t-1` is a member of an executing list and also of the planned `clash`.
        member(&store, "t-1");
        assert_eq!(wl(&store, &["new", "runs", "runs"]), 0);
        assert_eq!(wl(&store, &["add", "runs", "t-1"]), 0);
        let mut r = store.worklist("runs").unwrap();
        r.status_raw = "running".into();
        store.save_worklist(&r).unwrap();
        planned(&store, "clash", "t-3", "2026-11-05T09:00:00Z");
        let mut c = store.worklist("clash").unwrap();
        let mut gs = c.groups();
        gs[0].members = vec!["t-1".into()];
        c.set_groups(&gs);
        store.save_worklist(&c).unwrap();
        planned(&store, "behind", "t-2", "2026-11-05T10:00:00Z");

        assert_eq!(tick_on(&store, room(8)).as_deref(), Some("behind"), "clash is refused, behind goes");
        assert_eq!(word(&store, "clash"), "planned");
        assert!(refused_lately(&store, "clash"));
        assert!(!refused_lately(&store, "behind"));
    }

    #[test]
    fn the_sentence_and_the_caption_say_what_the_list_waited_on() {
        let mut w = Worklist::new("l", "L");
        w.planned = Some(util::now_iso());
        let row = Row { id: "l".into(), wait: None, need: 3, width: 2, together: true };
        let m = Machine { free: Some(5), cap: Some(8), throttled: None };
        let none = why_started(&w, &row, &m, &Seen::default());
        assert!(none.contains("nothing was in its way") && none.contains("5 of 8 places free"), "{none}");
        assert_eq!(caption_for(&Seen::default()), "planned, nothing was in its way");
        let seen = Seen { kinds: vec!["room".into(), "after pre-prep".into()], last: "after pre-prep".into() };
        let s = why_started(&w, &row, &m, &seen);
        assert!(s.contains("held by room, after pre-prep (last: after pre-prep)"), "{s}");
        assert_eq!(caption_for(&seen), "planned, and the list it follows is complete");
        let capless = why_started(&w, &row, &Machine::default(), &Seen::default());
        assert!(capless.contains("no cap on agents"), "{capless}");
        assert!(!s.contains('`') && !s.contains('\n'), "a sentence a shell can carry: {s}");
        for kind in ["room", "throttle", "manual group", "after x", "alone with y"] {
            let c = caption_for(&Seen { kinds: vec![kind.into()], last: String::new() });
            assert!(c.chars().count() <= crate::caption::LIMIT, "{c}");
        }
        let _ = Wait::Throttle(crate::throttle::sample_pause());
    }
}
