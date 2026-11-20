//! Where a list's wall time went, by cause (`wsp-470`).
//!
//! The effort report's old figures are member-seconds: a member's model and
//! tool time, and the gaps *its own timeline* names. A stall inside a seat (a
//! row blocked on a person), a slow start, a member held behind the cap or an
//! `after`, and a barrier's full run were all booked as the seat's work or not
//! at all, so a 9 h run read "gaps 0.3 h".
//!
//! This reads the same timelines the other way round: **every second of the
//! list's wall belongs to one bucket**, whatever the members were doing at that
//! second together.
//!
//! - **working** — some member's seat is in a model call or a tool, or its own
//!   check or a verifier is running;
//! - **waiting** — nobody is working and some member is started but not moving:
//!   blocked (from the moment it blocks), an unclaimed start, a gap the logs
//!   name, a seat idle in its own phase, a leg whose seat has ended and whose
//!   line is still `running` (a leg is working only while its seat is alive);
//! - **serialised** — nobody is started but a member is yet to start: it waits
//!   behind the group's cap or an `after`;
//! - **barrier run** — the group's barrier, from the last verdict to the check;
//! - **check seat** — the barrier's check seat;
//! - **between groups** — the previous group's pass to this group's first start.
//!
//! A group is the window from the previous group's pass (the list's first
//! start for the first) to its own pass, so the windows tile the wall.

use std::collections::BTreeMap;

use crate::timeline::Phase;

/// What kind of time one stretch of a row's timeline is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Class {
    /// Spawn requested → claimed, seat up, warm-up: the seat is not at work yet.
    Start,
    /// The seat's own work (or a leg): working where it is in a call or a tool,
    /// idle otherwise.
    Seat,
    /// A leg of the row running (`wsp-218`): the member's seat has no call or
    /// tool of its own then, but the row's work is going on in the leg's seat.
    Leg,
    /// The row's own check, the verifier's start and read: working.
    Verify,
    /// Nothing was running; what was being waited for.
    Gap(String),
    /// The barrier's run (the group's), and the wait before it.
    Barrier,
    /// The barrier's check seat, and the wait before it.
    Check,
    /// The barrier held before it started, by a line of the cycle log naming
    /// what it waits on (an open question, a verifier, a member): the group is
    /// waiting on that, and no barrier is running (`compounding-468`).
    BarrierWait(String),
    /// Overlaps the barrier (clean-up, the seat released late): not counted.
    Alongside,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Span {
    pub start: i64,
    pub end: i64,
    pub class: Class,
}

impl Span {
    pub(crate) fn of(p: &Phase) -> Span {
        let l = p.label.as_str();
        let class = if p.shared {
            match p.waiting_for.as_deref() {
                Some(w) if p.gap && w.starts_with(BARRIER_HOLD) => Class::BarrierWait(w.to_string()),
                _ if l.contains("check seat") => Class::Check,
                _ => Class::Barrier,
            }
        } else if p.gap {
            Class::Gap(p.waiting_for.clone().unwrap_or_else(|| "unnamed".into()))
        } else if l.contains("alongside") {
            Class::Alongside
        } else if l.starts_with("spawn requested") || l.starts_with("seat up") || l.starts_with("warm-up") {
            Class::Start
        } else if l.starts_with("leg ") {
            Class::Leg
        } else if l.starts_with("the row's own check") || l.starts_with("verifier") {
            Class::Verify
        } else {
            Class::Seat
        };
        Span { start: p.start, end: p.end, class }
    }
}

/// What a barrier gap's cause starts with where the cycle log says the barrier
/// was held (`group N: barrier waits: …`): `timeline::wait_name` writes it, and
/// the wall books the stretch as waiting on that, not as the barrier's run.
pub(crate) const BARRIER_HOLD: &str = "the barrier waits: ";

/// Sorted, overlapping and touching intervals joined.
pub(crate) fn merge(v: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut v: Vec<(i64, i64)> = v.iter().copied().filter(|(a, b)| b > a).collect();
    v.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Seconds a row's legs ran in which its own seat had no call or tool in
/// flight (`wsp-218`): the legs' union less the seat's active stretches.
pub(crate) fn legs_only_seconds(spans: &[Span], active: &[(i64, i64)]) -> i64 {
    let legs = merge(&spans.iter().filter(|s| s.class == Class::Leg).map(|s| (s.start, s.end)).collect::<Vec<_>>());
    let total: i64 = legs.iter().map(|(a, b)| b - a).sum();
    let covered: i64 = legs.iter().map(|(a, b)| active.iter().map(|(x, y)| ((*y).min(*b) - (*x).max(*a)).max(0)).sum::<i64>()).sum();
    total - covered
}

fn within(v: &[(i64, i64)], t: i64) -> bool {
    let n = v.partition_point(|(a, _)| *a <= t);
    n > 0 && t < v[n - 1].1
}

/// One member as the sweep reads it.
#[derive(Debug, Clone, Default)]
pub(crate) struct MemberIn {
    pub id: String,
    pub spans: Vec<Span>,
    pub active: Vec<(i64, i64)>,
    pub blocked: Vec<(i64, i64)>,
    pub measured: bool,
    /// `(epoch, reason)`, sorted: what the row's log says it was waiting on from
    /// that moment (`wsp-559`). The seat idle is named by the latest one.
    pub markers: Vec<(i64, String)>,
}

/// The cause a seat idle is booked under, before the reason after ` — `.
pub(crate) const SEAT_IDLE: &str = "the seat idle (in neither a model call nor a tool)";
/// The reason where the log says nothing at that moment.
pub(crate) const NONE_RECORDED: &str = "none recorded";

/// The reason the row's log gives for the seat idle at `t`: the latest marker
/// at or before it.
fn idle_reason(m: &MemberIn, t: i64) -> &str {
    let n = m.markers.partition_point(|(a, _)| *a <= t);
    if n == 0 { NONE_RECORDED } else { &m.markers[n - 1].1 }
}

/// Seat-idle seconds by reason, from a group's `waits`.
pub(crate) fn seat_idle_reasons<'a>(waits: impl Iterator<Item = (&'a str, i64)>) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for (cause, s) in waits {
        if let Some(rest) = cause.strip_prefix(SEAT_IDLE) {
            let why = rest.strip_prefix(" — ").unwrap_or(NONE_RECORDED);
            *out.entry(why.to_string()).or_insert(0) += s;
        }
    }
    out
}

/// The waiting cause for the stretch from a group's last barrier run and check
/// seat to its pass, where a barrier ran.
pub(crate) const PASS_WAIT: &str = "waiting for the pass (the barrier done, the pass not yet given)";
/// The same where the group has no barrier span (a manual group): none ran, the
/// pass is given by a person.
pub(crate) const PASS_WAIT_MANUAL: &str = "waiting for the pass (manual group: no barrier ran, the pass not yet given)";

/// The waiting cause for the stretch from the previous group's pass to a manual
/// group's first start: the pass is given, and nobody has put a hand to the
/// group (a manual group is started by a person, not the run).
pub(crate) const START_WAIT_MANUAL: &str = "waiting for a hand to start (manual group: the previous pass is given, no member started yet)";

pub(crate) const BUCKETS: [&str; 7] = ["working", "waiting", "serialised", "barrier_run", "check_seat", "between_groups", "parked"];

/// One group's wall by bucket, and what the waiting and the barrier were.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct GroupWall {
    pub open: i64,
    pub close: i64,
    pub by: BTreeMap<&'static str, i64>,
    /// Waiting seconds by `(row, cause)`.
    pub waits: BTreeMap<(String, String), i64>,
    /// Seconds of the barrier's run alone (not the wait before it).
    pub barrier_run: i64,
    /// Of `working`, the seconds where only a member's legs were running: no
    /// seat was in a call or a tool (`wsp-218`).
    pub legs: i64,
    /// Parked seconds by what parked the list (who, when, why): the whole list
    /// stood still, so nobody's working, waiting or barrier run is in them.
    pub parked: BTreeMap<String, i64>,
}

impl GroupWall {
    pub(crate) fn seconds(&self, bucket: &str) -> i64 {
        self.by.get(bucket).copied().unwrap_or(0)
    }
}

enum State {
    NotStarted,
    Done,
    Working,
    /// Only a leg of the row is running.
    Legs,
    Waiting(u8, String),
}

fn state(m: &MemberIn, t: i64, first: i64, last: i64) -> State {
    if t < first {
        return State::NotStarted;
    }
    if t >= last {
        return State::Done;
    }
    // Blocked first: a row blocked on a person waits from the moment it blocks,
    // even when its seat is sat in one long tool call.
    if m.blocked.iter().any(|(a, b)| t >= *a && t < *b) {
        return State::Waiting(0, "blocked (the row stood blocked, on a person or a fix)".into());
    }
    if within(&m.active, t) {
        return State::Working;
    }
    // A leg running is the row's work even while the member's own seat has
    // nothing in flight; any other span a leg overlaps is read as it was.
    if m.spans.iter().any(|s| s.class == Class::Leg && t >= s.start && t < s.end) {
        return State::Legs;
    }
    match m.spans.iter().filter(|s| t >= s.start && t < s.end && s.class != Class::Alongside).map(|s| &s.class).next() {
        Some(Class::Verify) => State::Working,
        Some(Class::Seat) if !m.measured => State::Working,
        Some(Class::Start) => State::Waiting(1, "an unclaimed or slow start (spawn requested → the seat's first work)".into()),
        // A leg whose seat ended with the leg not closed (`wsp-016`): waiting,
        // under its own name, with the seat and when it ended.
        Some(Class::Gap(why)) if why.starts_with(crate::timeline::LEG_SEAT_ENDED) => State::Waiting(2, why.clone()),
        Some(Class::Gap(why)) => State::Waiting(2, format!("a gap — {why}")),
        _ => State::Waiting(3, format!("{SEAT_IDLE} — {}", idle_reason(m, t))),
    }
}

/// A group's wall. `open` is the previous group's pass (`None` for the first or
/// where it is not found: the group's first start), `pass` its own barrier's
/// pass (`None`: the end of the last stretch the members record).
#[cfg(test)]
pub(crate) fn group_wall(members: &[MemberIn], open: Option<i64>, pass: Option<i64>) -> Option<GroupWall> {
    group_wall_parked(members, open, pass, &[])
}

/// [`group_wall`] with the stretches a person parked the whole list, `(start,
/// end, words)`: a second inside one is `parked` and nothing else, whatever the
/// members or the barrier were doing or owed there (the barrier's wait is not
/// its run, `compounding-464`).
pub(crate) fn group_wall_parked(members: &[MemberIn], open: Option<i64>, pass: Option<i64>, parks: &[(i64, i64, String)]) -> Option<GroupWall> {
    group_wall_for(members, open, pass, parks, false)
}

/// [`group_wall_parked`] for a group that is `manual` or not: a manual group's
/// stretch from the previous pass to its first start is a person's wait for a
/// hand to start it, `waiting` and named, not `between_groups` (where the run
/// itself is the one starting the next group).
pub(crate) fn group_wall_for(members: &[MemberIn], open: Option<i64>, pass: Option<i64>, parks: &[(i64, i64, String)], manual: bool) -> Option<GroupWall> {
    let started: Vec<(&MemberIn, i64, i64)> = members
        .iter()
        .filter_map(|m| {
            let own: Vec<&Span> = m.spans.iter().filter(|s| !matches!(s.class, Class::Barrier | Class::Check | Class::BarrierWait(_) | Class::Alongside)).collect();
            Some((m, own.iter().map(|s| s.start).min()?, own.iter().map(|s| s.end).max()?))
        })
        .collect();
    let first_start = started.iter().map(|x| x.1).min()?;
    let shared: Vec<&Span> = members.iter().flat_map(|m| m.spans.iter()).filter(|s| matches!(s.class, Class::Barrier | Class::Check | Class::BarrierWait(_))).collect();
    let last_member = started.iter().map(|x| x.2).max()?;
    // The window opens at the previous group's pass, whether or not a member
    // started before it (a row cut from the previous group and started again, a
    // seat claimed ahead): what a member did before the window opened is the
    // previous window's, so the windows tile and no second is in two
    // (`compounding-217`: the buckets summed 1531 s past the wall).
    let open = open.unwrap_or(first_start);
    let first = first_start.max(open);
    let barrier_from = shared.iter().map(|s| s.start).min().unwrap_or(last_member).max(first);
    let close = pass.unwrap_or_else(|| shared.iter().map(|s| s.end).max().unwrap_or(last_member)).max(barrier_from);
    let mut g = GroupWall { open, close, ..Default::default() };
    let add = |g: &mut GroupWall, k: &'static str, secs: i64| *g.by.entry(k).or_insert(0) += secs;

    // A park before the group's first start is the list's, not a pass waiting
    // for a start.
    let mut between = first - open;
    for (a, b, why) in parks {
        let got = ((*b).min(first) - (*a).max(open)).max(0);
        if got > 0 {
            add(&mut g, "parked", got);
            *g.parked.entry(why.clone()).or_insert(0) += got;
            between -= got;
        }
    }
    if manual && between > 0 {
        add(&mut g, "waiting", between);
        *g.waits.entry((String::new(), START_WAIT_MANUAL.into())).or_insert(0) += between;
    } else {
        add(&mut g, "between_groups", between.max(0));
    }

    // The window instant by instant: where a barrier span lies it is the
    // barrier's (its run, or its check seat); after the last it is the barrier's
    // up to the pass; elsewhere the members', which includes the stretch
    // between a failed run and the next, a follow-up row's own time (`wsp-568`).
    let barrier = merge(&shared.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>());
    let pass_wait = if shared.iter().any(|s| s.class == Class::Barrier) { PASS_WAIT } else { PASS_WAIT_MANUAL };
    let last_barrier = barrier.last().map_or(barrier_from, |b| b.1);
    let mut cuts: Vec<i64> = vec![first, barrier_from, close, last_barrier];
    for s in &shared {
        cuts.extend([s.start, s.end]);
    }
    for (a, b, _) in parks {
        cuts.extend([*a, *b]);
    }
    for (m, ..) in &started {
        for s in &m.spans {
            cuts.extend([s.start, s.end]);
        }
        for (a, b) in m.active.iter().chain(m.blocked.iter()) {
            cuts.extend([*a, *b]);
        }
        cuts.extend(m.markers.iter().map(|(t, _)| *t));
    }
    cuts.retain(|t| *t >= first && *t <= close);
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if let Some((_, _, why)) = parks.iter().find(|(x, y, _)| a >= *x && a < *y) {
            add(&mut g, "parked", b - a);
            *g.parked.entry(why.clone()).or_insert(0) += b - a;
            continue;
        }
        if a >= last_barrier {
            // After the barrier's last run and its check seat, up to the pass:
            // nobody is running the barrier, the group is waiting for its pass
            // (a manual group's is given by a person), so it is not the
            // barrier's run (`wsp-568` booked it there).
            add(&mut g, "waiting", b - a);
            *g.waits.entry((String::new(), pass_wait.into())).or_insert(0) += b - a;
            continue;
        }
        if within(&barrier, a) {
            let at = |s: &&&Span| a >= s.start && a < s.end;
            let held = shared.iter().filter(at).find_map(|s| if let Class::BarrierWait(c) = &s.class { Some(c.clone()) } else { None });
            let running = shared.iter().filter(at).any(|s| matches!(s.class, Class::Barrier | Class::Check));
            if let (Some(cause), false) = (held, running) {
                // Held by a named wait: nobody is running the barrier.
                add(&mut g, "waiting", b - a);
                *g.waits.entry((String::new(), cause)).or_insert(0) += b - a;
            } else if shared.iter().filter(at).any(|s| s.class == Class::Check) {
                add(&mut g, "check_seat", b - a);
            } else {
                add(&mut g, "barrier_run", b - a);
                if shared.iter().filter(at).any(|s| s.class == Class::Barrier) {
                    g.barrier_run += b - a;
                }
            }
            continue;
        }
        let (mut working, mut legs) = (false, false);
        let mut waiting: Option<(u8, &str, String)> = None;
        let mut not_started = false;
        for (m, f, l) in &started {
            match state(m, a, *f, *l) {
                State::Working => working = true,
                State::Legs => legs = true,
                State::Waiting(rank, cause) => {
                    if waiting.as_ref().is_none_or(|w| rank < w.0) {
                        waiting = Some((rank, &m.id, cause));
                    }
                }
                State::NotStarted => not_started = true,
                State::Done => {}
            }
        }
        if working || legs {
            add(&mut g, "working", b - a);
            if !working {
                g.legs += b - a;
            }
        } else if let Some((_, row, cause)) = waiting {
            add(&mut g, "waiting", b - a);
            *g.waits.entry((row.to_string(), cause)).or_insert(0) += b - a;
        } else if not_started {
            add(&mut g, "serialised", b - a);
        } else {
            add(&mut g, "waiting", b - a);
            *g.waits.entry((String::new(), "every member done, the barrier not yet started".into())).or_insert(0) += b - a;
        }
    }
    Some(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(id: &str, a: i64, b: i64, active: &[(i64, i64)], blocked: &[(i64, i64)]) -> MemberIn {
        MemberIn {
            id: id.into(),
            spans: vec![Span { start: a, end: a + 10, class: Class::Start }, Span { start: a + 10, end: b, class: Class::Seat }],
            active: active.to_vec(),
            blocked: blocked.to_vec(),
            measured: true,
            markers: Vec::new(),
        }
    }

    fn barrier(ms: &mut [MemberIn], run: (i64, i64), check: (i64, i64)) {
        for m in ms.iter_mut() {
            m.spans.push(Span { start: run.0, end: run.1, class: Class::Barrier });
            m.spans.push(Span { start: check.0, end: check.1, class: Class::Check });
        }
    }

    #[test]
    fn a_stall_in_a_seat_is_waiting_from_the_moment_it_blocks_not_the_seats_work() {
        // r-1 works 0-100; r-2 starts at 0, is blocked from 50 to 900 and then works to 1000.
        let mut ms = vec![seat("r-1", 0, 100, &[(10, 100)], &[]), seat("r-2", 0, 1000, &[(10, 50), (900, 1000)], &[(50, 900)])];
        barrier(&mut ms, (1000, 1600), (1600, 1700));
        let g = group_wall(&ms, None, Some(1700)).unwrap();
        assert_eq!(g.seconds("working"), 90 + 100, "10-100 (r-1, then r-2 to 50) and 900-1000");
        assert_eq!(g.seconds("waiting"), 10 + 800, "the two starts, then 100-900 with r-2 blocked");
        assert_eq!(g.by.get("serialised"), None);
        assert_eq!(g.seconds("barrier_run"), 600);
        assert_eq!(g.seconds("check_seat"), 100);
        assert_eq!(g.barrier_run, 600);
        assert_eq!(g.waits.iter().find(|((r, c), _)| r == "r-2" && c.starts_with("blocked")).map(|(_, s)| *s), Some(800));
        assert_eq!(g.by.values().sum::<i64>(), 1700, "every second of 0-1700 is in a bucket");
    }

    #[test]
    fn a_failed_barrier_run_then_a_follow_up_row_then_the_run_again_reads_as_the_barrier_and_tiles_the_wall() {
        // r-1 works 0-100; r-2 is held to 300 and works to 400. The barrier's
        // first run fails (400-1000); the follow-up r-3 starts at 1010 and works
        // to 1100, and nothing runs to 1500; the run again is 1500-1515, its
        // check seat to 1600. The stretch between the runs is the members', not
        // the barrier's.
        let mut ms = vec![seat("r-1", 0, 100, &[(10, 100)], &[]), seat("r-2", 300, 400, &[(310, 400)], &[]), seat("r-3", 1010, 1100, &[(1020, 1100)], &[])];
        for m in ms.iter_mut() {
            m.spans.push(Span { start: 400, end: 1000, class: Class::Barrier });
            m.spans.push(Span { start: 1500, end: 1515, class: Class::Barrier });
            m.spans.push(Span { start: 1515, end: 1600, class: Class::Check });
        }
        let g = group_wall(&ms, None, Some(1600)).unwrap();
        assert_eq!(g.seconds("barrier_run"), 600 + 15, "the failed run and the run again, not the stretch between");
        assert_eq!(g.barrier_run, 615);
        assert_eq!(g.seconds("check_seat"), 85);
        assert_eq!(g.seconds("serialised"), 200 + 10, "100-300 r-2 held, and 1000-1010 before the follow-up starts");
        assert_eq!(g.seconds("working"), 90 + 90 + 80, "r-1, r-2 and the follow-up r-3");
        assert_eq!(g.seconds("waiting"), 20 + 10 + 400, "the starts, r-3's, and 1100-1500 with every member done");
        assert_eq!(g.by.values().sum::<i64>(), 1600, "the buckets sum to the wall");
    }

    #[test]
    fn a_barrier_held_by_a_named_wait_is_waiting_on_it_and_only_the_run_is_barrier_run() {
        // r-1 works 0-100. The barrier is held from 100 to 2650 (a member's open
        // question, from the cycle log), runs 2650-2873 and its check seat to 2900.
        let mut ms = vec![seat("r-1", 0, 100, &[(10, 100)], &[])];
        let cause = format!("{BARRIER_HOLD}cpd-1 (r-9) is waiting on the governor to answer its question m-1");
        for m in ms.iter_mut() {
            m.spans.push(Span { start: 100, end: 2650, class: Class::BarrierWait(cause.clone()) });
            m.spans.push(Span { start: 2650, end: 2873, class: Class::Barrier });
            m.spans.push(Span { start: 2873, end: 2900, class: Class::Check });
        }
        let g = group_wall(&ms, None, Some(2900)).unwrap();
        assert_eq!(g.seconds("barrier_run"), 223, "the run, not the hold");
        assert_eq!(g.barrier_run, 223);
        assert_eq!(g.seconds("check_seat"), 27);
        assert_eq!(g.seconds("waiting"), 10 + 2550, "the start, then the hold");
        assert_eq!(g.waits.get(&(String::new(), cause)), Some(&2550));
        assert_eq!(g.by.values().sum::<i64>(), 2900, "the buckets tile the window");
    }

    #[test]
    fn a_barrier_hold_phase_is_a_wait_class_and_an_unheld_gap_stays_the_barriers() {
        let gap = |why: String| Phase { label: "last verdict → barrier 2 run started".into(), start: 0, end: 10, gap: true, waiting_for: Some(why), shared: true };
        let held = gap(format!("{BARRIER_HOLD}x"));
        let hand_off = gap("the hand-off (a poke and a spawn)".into());
        assert_eq!(Span::of(&held).class, Class::BarrierWait(format!("{BARRIER_HOLD}x")));
        assert_eq!(Span::of(&hand_off).class, Class::Barrier);
    }

    #[test]
    fn a_block_inside_one_long_tool_call_is_still_waiting() {
        // r-2's seat is in one tool call from 10 to 1000, but the row is blocked from 50 to 900.
        let mut ms = vec![seat("r-2", 0, 1000, &[(10, 1000)], &[(50, 900)])];
        barrier(&mut ms, (1000, 1600), (1600, 1700));
        let g = group_wall(&ms, None, Some(1700)).unwrap();
        assert_eq!(g.seconds("working"), 40 + 100, "10-50 and 900-1000 only");
        assert_eq!(g.seconds("waiting"), 10 + 850, "the start, then 50-900 blocked");
        assert_eq!(g.waits.iter().find(|((r, c), _)| r == "r-2" && c.starts_with("blocked")).map(|(_, s)| *s), Some(850));
        assert_eq!(g.by.values().sum::<i64>(), 1700);
    }

    #[test]
    fn a_member_not_yet_started_with_nobody_else_moving_is_serialised() {
        let mut ms = vec![seat("r-1", 0, 100, &[(10, 100)], &[]), seat("r-2", 300, 400, &[(310, 400)], &[])];
        barrier(&mut ms, (400, 450), (450, 460));
        let g = group_wall(&ms, None, Some(460)).unwrap();
        assert_eq!(g.seconds("serialised"), 200, "100-300: r-1 done, r-2 behind the cap or an after");
        assert_eq!(g.seconds("working"), 90 + 90);
        assert_eq!(g.seconds("waiting"), 10 + 10, "the two starts that nobody else was covering");
    }

    #[test]
    fn the_wall_between_a_pass_and_the_groups_first_start_is_between_groups() {
        let mut ms = vec![seat("r-1", 500, 600, &[(510, 600)], &[])];
        barrier(&mut ms, (600, 700), (700, 720));
        let g = group_wall(&ms, Some(400), Some(720)).unwrap();
        assert_eq!(g.seconds("between_groups"), 100);
        assert_eq!((g.open, g.close), (400, 720));
        assert_eq!(g.by.values().sum::<i64>(), 320);
    }

    #[test]
    fn a_manual_groups_wait_for_a_hand_to_start_is_waiting_and_named_not_between_groups() {
        let mk = || {
            let mut ms = vec![seat("r-1", 500, 600, &[(510, 600)], &[])];
            barrier(&mut ms, (600, 700), (700, 720));
            ms
        };
        let g = group_wall_for(&mk(), Some(400), Some(720), &[], true).unwrap();
        assert_eq!(g.seconds("between_groups"), 0);
        assert_eq!(g.waits.get(&(String::new(), START_WAIT_MANUAL.to_string())), Some(&100));
        assert_eq!(g.by.values().sum::<i64>(), 320, "the window still tiles");
        let run = group_wall_for(&mk(), Some(400), Some(720), &[], false).unwrap();
        assert_eq!(run.seconds("between_groups"), 100, "a run-started group's stretch is unchanged");
    }

    #[test]
    fn a_members_legs_running_are_working_not_the_seat_idle() {
        // r-1's seat works 10-100, then runs legs 100-900 (its own calls: none),
        // then reviews 900-1000. Two legs overlap in the middle.
        let mut m = seat("r-1", 0, 100, &[(10, 100)], &[]);
        m.spans.push(Span { start: 100, end: 600, class: Class::Leg });
        m.spans.push(Span { start: 400, end: 900, class: Class::Leg });
        m.spans.push(Span { start: 900, end: 1000, class: Class::Verify });
        m.spans.push(Span { start: 1000, end: 1100, class: Class::Barrier });
        let g = group_wall(&[m], None, Some(1100)).unwrap();
        assert_eq!(g.seconds("working"), 90 + 800 + 100, "the seat's call, the legs, the check");
        assert_eq!(g.legs, 800, "the legs alone, overlap counted once");
        assert_eq!(g.seconds("waiting"), 10, "the start only");
        assert!(seat_idle_reasons(g.waits.iter().map(|((_, c), s)| (c.as_str(), *s))).is_empty(), "{:?}", g.waits);
    }

    /// wsp-001's `finish` (`wsp-016`): the leg's seat came up, worked and ended
    /// at T at its Stop, and the leg's line read `running` to T+60 min. The hour
    /// is waiting, named for the seat and when it ended, and none of it is
    /// working nor legs_running_seconds.
    #[test]
    fn a_leg_whose_seat_ended_is_waiting_from_the_end_not_working_nor_legs_running() {
        use crate::timeline::{build, Inputs, LegIn, LEG_SEAT_ENDED};
        use serde_json::json;
        let b = 1_763_350_000i64;
        let t = b + 1000;
        let ev = |at: i64, kind: &str, data: serde_json::Value| (b + at, kind.to_string(), data);
        let i = Inputs {
            id: "r-1".into(),
            events: vec![
                ev(110, "task-claimed", json!({"id": "r-1"})),
                ev(112, "session-learned", json!({"id": "r-1", "pane": "cpd-120"})),
                ev(160, "leg-added", json!({"id": "r-1", "leg": "finish"})),
                ev(165, "task-released", json!({"id": "r-1"})),
                ev(200, "session-learned", json!({"id": "r-1", "pane": "cpd-121"})),
                ev(1000 + 3600, "leg-done", json!({"id": "r-1", "leg": "finish"})),
                ev(1000 + 3605, "task-review", json!({"id": "r-1"})),
            ],
            cycle: vec![
                (b + 100, "starting r-1 on claude (new)".into()),
                (b + 190, "list group 1: starting leg finish of r-1".into()),
                (t + 10, format!("r-1: {} {} (member, cpd-121), no pid of it running and its tree kept", crate::ending::EXIT_ENDED, crate::util::iso_at(t))),
            ],
            legs: vec![LegIn { name: "finish".into(), ..Default::default() }],
            first_request: Some(b + 118),
            ..Default::default()
        };
        let spans: Vec<Span> = build(&i).iter().map(Span::of).collect();
        let m = MemberIn { id: "r-1".into(), spans, active: vec![(b + 118, b + 165)], measured: true, ..Default::default() };
        let g = group_wall(&[m], None, Some(t + 3605)).unwrap();
        let ended: Vec<(&String, &i64)> = g.waits.iter().filter(|((_, c), _)| c.starts_with(LEG_SEAT_ENDED)).map(|((_, c), s)| (c, s)).collect();
        assert_eq!(ended.len(), 1, "{:?}", g.waits);
        let (cause, secs) = ended[0];
        assert_eq!(*secs, 3600, "T to T+60 min: {:?}", g.waits);
        assert!(cause.contains("cpd-121") && cause.contains(&crate::util::iso_at(t)), "the seat and when it ended: {cause}");
        assert_eq!(g.legs, 1000 - 190, "the leg's run up to its seat's end, and no more");
        assert_eq!(g.seconds("working"), (165 - 118) + (1000 - 190), "the member's own calls and the leg while its seat lived");
        assert_eq!(g.by.values().sum::<i64>(), g.close - g.open, "the window tiles");
    }

    #[test]
    fn a_blocked_row_is_waiting_even_with_a_leg_span_over_it() {
        let mut m = seat("r-1", 0, 100, &[(10, 100)], &[(200, 300)]);
        m.spans.push(Span { start: 100, end: 400, class: Class::Leg });
        m.spans.push(Span { start: 400, end: 500, class: Class::Barrier });
        let g = group_wall(&[m], None, Some(500)).unwrap();
        assert_eq!(g.seconds("waiting"), 10 + 100);
        assert_eq!(g.legs, 200);
    }

    #[test]
    fn a_seat_with_no_transcript_is_not_called_idle() {
        let mut m = seat("r-1", 0, 100, &[], &[]);
        m.measured = false;
        let mut ms = vec![m];
        barrier(&mut ms, (100, 150), (150, 160));
        let g = group_wall(&ms, None, Some(160)).unwrap();
        assert_eq!(g.seconds("working"), 90);
        assert_eq!(g.seconds("waiting"), 10, "the start only");
    }

    #[test]
    fn a_seat_idle_is_named_by_the_latest_line_the_log_wrote_before_it() {
        // r-1 works 10-100, then sits idle to 990 with no phase: sent back at 200, told at 500, reopened at 600.
        let mut m = seat("r-1", 0, 100, &[(10, 100)], &[]);
        m.spans[1].end = 100;
        m.spans.push(Span { start: 990, end: 1000, class: Class::Verify });
        m.spans.push(Span { start: 1000, end: 1100, class: Class::Barrier });
        m.markers = vec![(200, "sent back: the verdict given".into()), (500, "told: the seat has the sentence".into()), (600, "reopened: the seat is on the send-back".into())];
        let g = group_wall(&[m], None, Some(1100)).unwrap();
        let by = seat_idle_reasons(g.waits.iter().map(|((_, c), s)| (c.as_str(), *s)));
        assert_eq!(by.get(NONE_RECORDED), Some(&100), "100-200, before any line");
        assert_eq!(by.get("sent back: the verdict given"), Some(&300));
        assert_eq!(by.get("told: the seat has the sentence"), Some(&100));
        assert_eq!(by.get("reopened: the seat is on the send-back"), Some(&390));
        assert_eq!(by.values().sum::<i64>(), 890, "every idle second of 100-990 carries a reason");
    }

    #[test]
    fn a_manual_groups_wait_for_its_pass_is_waiting_not_the_barriers_run() {
        // The barrier runs 100-107 and its check seat to 110; the person gives
        // the pass at 2600.
        let mut ms = vec![seat("r-1", 0, 100, &[(10, 100)], &[])];
        barrier(&mut ms, (100, 107), (107, 110));
        let g = group_wall(&ms, None, Some(2600)).unwrap();
        assert_eq!(g.seconds("barrier_run"), 7);
        assert_eq!(g.barrier_run, 7);
        assert_eq!(g.seconds("check_seat"), 3);
        assert_eq!(g.seconds("waiting"), 10 + 2490, "the start, then 110-2600 for the pass");
        assert_eq!(g.waits.get(&(String::new(), PASS_WAIT.to_string())), Some(&2490));
        assert_eq!(g.by.values().sum::<i64>(), 2600);
    }

    #[test]
    fn a_group_with_no_barrier_run_waits_for_its_pass_from_its_last_member() {
        let ms = vec![seat("r-1", 0, 100, &[(10, 100)], &[])];
        let g = group_wall(&ms, None, Some(500)).unwrap();
        assert_eq!(g.seconds("barrier_run"), 0);
        assert_eq!(g.seconds("waiting"), 10 + 400);
        assert_eq!(g.by.values().sum::<i64>(), 500);
    }

    #[test]
    fn a_member_started_before_its_windows_pass_opened_is_read_from_the_pass_on_and_the_windows_tile() {
        // Group 1 passes at 600. r-2 was in group 1, cut, started again for group
        // 2: its first stretch (50-120) lies in group 1's window, its second (700-800)
        // in group 2's, and the days between are a named gap. Group 2's window
        // opens at 600, not at r-2's first start of 50.
        let mut m = seat("r-2", 50, 800, &[(60, 120), (710, 800)], &[]);
        m.spans = vec![
            Span { start: 50, end: 60, class: Class::Start },
            Span { start: 60, end: 120, class: Class::Seat },
            Span { start: 120, end: 700, class: Class::Gap("parked between attempts".into()) },
            Span { start: 700, end: 710, class: Class::Start },
            Span { start: 710, end: 800, class: Class::Seat },
            Span { start: 800, end: 900, class: Class::Barrier },
        ];
        let g = group_wall(&[m], Some(600), Some(900)).unwrap();
        assert_eq!((g.open, g.close), (600, 900));
        assert_eq!(g.by.values().sum::<i64>(), 300, "the window and no more: nothing before the pass");
        assert_eq!(g.seconds("between_groups"), 0);
        assert_eq!(g.seconds("working"), 90);
        assert_eq!(g.waits.get(&("r-2".to_string(), "a gap — parked between attempts".to_string())), Some(&100), "120-700 clipped to 600-700");
    }

    #[test]
    fn a_group_with_no_started_member_has_no_wall() {
        assert!(group_wall(&[MemberIn::default()], None, None).is_none());
    }

    /// exit-packet's group 1 (`compounding-464`): the last verdict at 100, the
    /// barrier's span 100-1100 (its wait, then its run), the list parked 150-1000.
    /// The park is the list's, not the barrier's.
    #[test]
    fn a_park_inside_the_barriers_wait_is_parked_and_the_barrier_run_is_not_the_park() {
        let mut m = seat("r-1", 0, 100, &[(10, 100)], &[]);
        m.spans.push(Span { start: 100, end: 1100, class: Class::Barrier });
        let parks = vec![(150, 1000, "the list was parked by somebody".to_string())];
        let g = group_wall_parked(&[m.clone()], None, Some(1100), &parks).unwrap();
        assert_eq!(g.seconds("parked"), 850);
        assert_eq!(g.parked.get("the list was parked by somebody"), Some(&850));
        assert_eq!(g.seconds("barrier_run"), 50 + 100, "100-150 and 1000-1100: the wait left and the run, none of the park");
        assert_eq!(g.by.values().sum::<i64>(), g.close - g.open, "the window tiles");
        let plain = group_wall(&[m], None, Some(1100)).unwrap();
        assert_eq!(plain.seconds("barrier_run"), 1000, "without the park record the wait is the barrier's");
    }

    #[test]
    fn a_park_before_the_groups_first_start_is_parked_and_not_between_groups() {
        let m = seat("r-2", 700, 800, &[(710, 800)], &[]);
        let parks = vec![(100, 650, "parked".to_string())];
        let g = group_wall_parked(&[m], Some(0), Some(900), &parks).unwrap();
        assert_eq!((g.seconds("parked"), g.seconds("between_groups")), (550, 150), "{g:?}");
        assert_eq!(g.by.values().sum::<i64>(), 900);
    }

    #[test]
    fn a_park_after_the_barrier_and_before_the_pass_is_parked_and_not_waiting_for_the_pass() {
        let mut ms = vec![seat("r-1", 0, 100, &[(10, 100)], &[])];
        barrier(&mut ms, (100, 200), (200, 220));
        let g = group_wall_parked(&ms, None, Some(1000), &[(300, 900, "p".to_string())]).unwrap();
        assert_eq!((g.seconds("parked"), g.seconds("waiting")), (600, 10 + 80 + 100), "{g:?}");
        assert_eq!(g.by.values().sum::<i64>(), 1000);
    }
}
