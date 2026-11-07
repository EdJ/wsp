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
//!   name, a seat idle in its own phase;
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
    /// The row's own check, the verifier's start and read: working.
    Verify,
    /// Nothing was running; what was being waited for.
    Gap(String),
    /// The barrier's run (the group's), and the wait before it.
    Barrier,
    /// The barrier's check seat, and the wait before it.
    Check,
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
            if l.contains("check seat") { Class::Check } else { Class::Barrier }
        } else if p.gap {
            Class::Gap(p.waiting_for.clone().unwrap_or_else(|| "unnamed".into()))
        } else if l.contains("alongside") {
            Class::Alongside
        } else if l.starts_with("spawn requested") || l.starts_with("seat up") || l.starts_with("warm-up") {
            Class::Start
        } else if l.starts_with("the row's own check") || l.starts_with("verifier") {
            Class::Verify
        } else {
            Class::Seat
        };
        Span { start: p.start, end: p.end, class }
    }
}

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
}

pub(crate) const BUCKETS: [&str; 6] = ["working", "waiting", "serialised", "barrier_run", "check_seat", "between_groups"];

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
    match m.spans.iter().filter(|s| t >= s.start && t < s.end && s.class != Class::Alongside).map(|s| &s.class).next() {
        Some(Class::Verify) => State::Working,
        Some(Class::Seat) if !m.measured => State::Working,
        Some(Class::Start) => State::Waiting(1, "an unclaimed or slow start (spawn requested → the seat's first work)".into()),
        Some(Class::Gap(why)) => State::Waiting(2, format!("a gap — {why}")),
        _ => State::Waiting(3, "the seat idle (in neither a model call nor a tool)".into()),
    }
}

/// A group's wall. `open` is the previous group's pass (`None` for the first or
/// where it is not found: the group's first start), `pass` its own barrier's
/// pass (`None`: the end of the last stretch the members record).
pub(crate) fn group_wall(members: &[MemberIn], open: Option<i64>, pass: Option<i64>) -> Option<GroupWall> {
    let started: Vec<(&MemberIn, i64, i64)> = members
        .iter()
        .filter_map(|m| {
            let own: Vec<&Span> = m.spans.iter().filter(|s| !matches!(s.class, Class::Barrier | Class::Check | Class::Alongside)).collect();
            Some((m, own.iter().map(|s| s.start).min()?, own.iter().map(|s| s.end).max()?))
        })
        .collect();
    let first = started.iter().map(|x| x.1).min()?;
    let shared: Vec<&Span> = members.iter().flat_map(|m| m.spans.iter()).filter(|s| matches!(s.class, Class::Barrier | Class::Check)).collect();
    let last_member = started.iter().map(|x| x.2).max()?;
    let barrier_from = shared.iter().map(|s| s.start).min().unwrap_or(last_member).max(first);
    let close = pass.unwrap_or_else(|| shared.iter().map(|s| s.end).max().unwrap_or(last_member)).max(barrier_from);
    let open = open.unwrap_or(first).min(first);
    let mut g = GroupWall { open, close, ..Default::default() };
    let add = |g: &mut GroupWall, k: &'static str, secs: i64| *g.by.entry(k).or_insert(0) += secs;

    add(&mut g, "between_groups", first - open);

    // The members' stretch, instant by instant.
    let mut cuts: Vec<i64> = vec![first, barrier_from];
    for (m, ..) in &started {
        for s in &m.spans {
            cuts.extend([s.start, s.end]);
        }
        for (a, b) in m.active.iter().chain(m.blocked.iter()) {
            cuts.extend([*a, *b]);
        }
    }
    cuts.retain(|t| *t >= first && *t <= barrier_from);
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut working = false;
        let mut waiting: Option<(u8, &str, String)> = None;
        let mut not_started = false;
        for (m, f, l) in &started {
            match state(m, a, *f, *l) {
                State::Working => working = true,
                State::Waiting(rank, cause) => {
                    if waiting.as_ref().is_none_or(|w| rank < w.0) {
                        waiting = Some((rank, &m.id, cause));
                    }
                }
                State::NotStarted => not_started = true,
                State::Done => {}
            }
        }
        if working {
            add(&mut g, "working", b - a);
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

    // The barrier: from the last verdict to the pass.
    let mut cuts: Vec<i64> = vec![barrier_from, close];
    for s in &shared {
        cuts.extend([s.start, s.end]);
    }
    cuts.retain(|t| *t >= barrier_from && *t <= close);
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let at = |s: &&&Span| a >= s.start && a < s.end;
        if shared.iter().filter(at).any(|s| s.class == Class::Check) {
            add(&mut g, "check_seat", b - a);
        } else {
            add(&mut g, "barrier_run", b - a);
            if shared.iter().filter(at).any(|s| s.class == Class::Barrier) {
                g.barrier_run += b - a;
            }
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
    fn a_group_with_no_started_member_has_no_wall() {
        assert!(group_wall(&[MemberIn::default()], None, None).is_none());
    }
}
