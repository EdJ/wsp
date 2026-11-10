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

/// The waiting cause for the stretch from a group's last barrier run (or, where
/// it has none, its last member) to its pass.
pub(crate) const PASS_WAIT: &str = "the barrier done, the pass not yet given";

pub(crate) const BUCKETS:[&str; 6] = ["working", "waiting", "serialised", "barrier_run", "check_seat", "between_groups"];

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
        _ => State::Waiting(3, format!("{SEAT_IDLE} — {}", idle_reason(m, t))),
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

    // The window instant by instant: where a barrier span lies it is the
    // barrier's (its run, or its check seat); after the last it is the barrier's
    // up to the pass; elsewhere the members', which includes the stretch
    // between a failed run and the next, a follow-up row's own time (`wsp-568`).
    let barrier = merge(&shared.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>());
    let last_barrier = barrier.last().map_or(barrier_from, |b| b.1);
    let mut cuts: Vec<i64> = vec![first, barrier_from, close, last_barrier];
    for s in &shared {
        cuts.extend([s.start, s.end]);
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
        if a >= last_barrier {
            // After the barrier's last run and its check seat, up to the pass:
            // nobody is running the barrier, the group is waiting for its pass
            // (a manual group's is given by a person), so it is not the
            // barrier's run (`wsp-568` booked it there).
            add(&mut g, "waiting", b - a);
            *g.waits.entry((String::new(), PASS_WAIT.into())).or_insert(0) += b - a;
            continue;
        }
        if within(&barrier, a) {
            let at = |s: &&&Span| a >= s.start && a < s.end;
            if shared.iter().filter(at).any(|s| s.class == Class::Check) {
                add(&mut g, "check_seat", b - a);
            } else {
                add(&mut g, "barrier_run", b - a);
                if shared.iter().filter(at).any(|s| s.class == Class::Barrier) {
                    g.barrier_run += b - a;
                }
            }
            continue;
        }
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
    fn a_group_with_no_started_member_has_no_wall() {
        assert!(group_wall(&[MemberIn::default()], None, None).is_none());
    }
}
