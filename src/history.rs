//! A list's history as data: `wsp-254` (compound `docs/ux-design.md` §19.3
//! and §19.9, for the graph's gate badges, its `+` marks, a dropped group's
//! tombstone and the inspector's timeline).
//!
//! # Read off the records the verbs already write
//!
//! Every hold, start again, join, follow-up, drop and stop amendment is
//! already a `## Log` line, written by one verb in one format, and every
//! barrier check is already a `checks:` entry on its group. A second record
//! of the same facts is one that can disagree with the first, and gpui-
//! migration's history — the list this was asked about — exists only in the
//! log. So this module reads the log once, here, and `worklist show --json`
//! hands the result to a client that would otherwise parse the prose itself.
//!
//! # Two readings, because the log names groups two ways
//!
//! **Membership is replayed by ordinal.** `→ group N`, `removed`, a
//! follow-up's `group N (new)` and `join group N`, and `group N dropped` each
//! name the ordinal the group had when the line was written, so the replay
//! keeps the groups as they were at each line and applies each edit where
//! it was made. Its last state is lined up with the groups the file holds,
//! by position where every group agrees and by members where one does not;
//! a group it cannot place has no history rather than somebody else's.
//!
//! **Events are placed by time.** `held`, `started again`, `passed` and a
//! stop amendment are about the group the run stood at, and the first two
//! name no group at all. A group's verdict opens with the instant its
//! barrier was passed, and nothing is ever put in ahead of the run, so the
//! group the run stood at when a line was written is the first whose
//! barrier had not been passed yet.
//!
//! # A group dropped while it ran takes its events with it
//!
//! It stood at the run's place from the moment the barrier before it was
//! passed until it was dropped, so what the log says about the running
//! group in that window is about it. gpui-migration's own case is the
//! reason this is a rule and not a nicety: `wsp-232`'s parser bug made a
//! group 11 of group 10's verdict, through no edit the log records, and the
//! replay never knew it. Its stop was amended at 20:09:44 and it was dropped
//! at 21:00:43; read by time alone, the amendment would land on the group
//! that took its place.
//!
//! # What a check calls a hold
//!
//! A check's `holds` means *hold the barrier*, and a member's verification
//! `holds` means *it passes*. The file keeps its word; the JSON says `hold`,
//! so one object never carries `holds` meaning both.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::barrier::{self, Check, Ending, State};
use crate::model::{Group, Task, Worklist};

/// Who put a group or a member on the list, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Maker {
    pub at: String,
    /// The check that filed it — its seat, or the Barrier row of a check from
    /// before `wsp-230` — or, for `add` and `mv`, whoever ran the verb, which
    /// a line written before this does not say.
    pub by: Option<String>,
    /// `add`, `mv` or `followup`.
    pub via: &'static str,
}

/// How a group came to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Made {
    pub maker: Maker,
    /// The group the run stood at when this one was made, by id: the barrier
    /// that is why it is here. Set for a follow-up group and for one somebody
    /// put in once the run had started, and not for one made of members that
    /// were already planned, which is the list rearranged, not added to.
    pub barrier: Option<u32>,
}

/// One line of a group's timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub at: String,
    /// `started`, `held`, `started_again`, `passed`, `stop_amended` or
    /// `followup`.
    pub kind: &'static str,
    pub why: String,
    /// The verdict's caption, where the line carries one (`wsp-241`): kept
    /// off `why`, which reads as it did before there were captions.
    pub caption: Option<String>,
    /// A follow-up's: the check that filed it, the rows, and whether they
    /// joined the group and held its barrier or became the next group.
    pub followup: Option<(String, Vec<String>, bool)>,
}

/// A group dropped while the run stood at it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dropped {
    /// The id of the group it came after; `None` when it was first, or when
    /// that group is no longer on the list either.
    pub after: Option<u32>,
    pub at: String,
    pub why: String,
    pub members: Vec<String>,
    pub events: Vec<Event>,
}

/// What the log says about one group the file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Of {
    pub made: Option<Made>,
    pub events: Vec<Event>,
    /// Each member that came onto the list after the run started, other than
    /// with the group itself.
    pub joined: BTreeMap<String, Maker>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct History {
    /// One per group of the list, in its order.
    pub groups: Vec<Of>,
    pub dropped: Vec<Dropped>,
    /// Every member a barrier check filed.
    pub filed: BTreeSet<String>,
}

/// One group as the replay holds it: a handle into what is known of its
/// making, and its members as they were at that line.
struct Slot {
    handle: usize,
    members: Vec<String>,
}

/// A group made while the replay ran, before it is known which group of the
/// file it became.
struct Making {
    maker: Maker,
    /// Made after the start of a member that was not planned then.
    new_work: bool,
    /// Made after the start, by the log's order: a list's first group and
    /// its start are often written in the same second.
    late: bool,
}

/// A drop, with the handle of the group before it still unresolved.
struct Drop {
    before: Option<usize>,
    at: String,
    why: String,
    members: Vec<String>,
}

/// The `## Log`'s entries, `(instant, text)`. A line under an entry is the
/// rest of it, and an entry with no instant is kept with an empty one, so it
/// is skipped rather than folded into the entry above.
fn entries(w: &Worklist) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in w.section("Log").unwrap_or_default().lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("- ") {
            let (at, text) = rest.split_once(' ').unwrap_or((rest, ""));
            match barrier::instant(at) {
                true => out.push((at.to_string(), text.trim().to_string())),
                false => out.push((String::new(), rest.to_string())),
            }
        } else if let Some(last) = out.last_mut().filter(|_| !t.is_empty()) {
            last.1.push(' ');
            last.1.push_str(t);
        }
    }
    out
}

/// Take a member out of whichever group holds it, and a group it empties
/// out of the list, as `rm` and `mv` both do.
fn take(slots: &mut Vec<Slot>, id: &str) -> bool {
    let Some(i) = slots.iter().position(|s| s.members.iter().any(|m| m == id)) else { return false };
    slots[i].members.retain(|m| m != id);
    if slots[i].members.is_empty() {
        slots.remove(i);
    }
    true
}

fn ids(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

fn same(a: &[String], b: &[String]) -> bool {
    a.iter().collect::<BTreeSet<_>>() == b.iter().collect::<BTreeSet<_>>()
}

/// The ordinal at the head of `text`, and the rest.
fn ordinal(text: &str) -> Option<(usize, &str)> {
    let end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let n = text[..end].parse().ok().filter(|n| *n > 0)?;
    Some((n, &text[end..]))
}

pub(crate) fn of(w: &Worklist, groups: &[Group]) -> History {
    let mut slots: Vec<Slot> = Vec::new();
    let mut makings: Vec<Option<Making>> = Vec::new();
    let mut started: Option<String> = None;
    let mut planned: BTreeSet<String> = BTreeSet::new();
    let mut entered: BTreeMap<String, Maker> = BTreeMap::new();
    let mut filed: BTreeSet<String> = BTreeSet::new();
    let mut events: Vec<Event> = Vec::new();
    let mut drops: Vec<Drop> = Vec::new();

    // A group made by the replay: a handle for it, and what made it.
    let mut make = |slots: &mut Vec<Slot>, at: usize, members: Vec<String>, maker: Option<Maker>, fresh: bool, running: bool| {
        makings.push(maker.map(|maker| Making { maker, new_work: fresh, late: running }));
        slots.insert(at.min(slots.len()), Slot { handle: makings.len() - 1, members });
    };

    for (at, text) in entries(w) {
        if at.is_empty() {
            continue;
        }
        let event = |kind: &'static str, why: &str| {
            let (why, caption) = crate::cmd_worklist::uncaptioned(why);
            Event {
                at: at.clone(),
                kind,
                why: why.trim().to_string(),
                caption: caption.filter(|c| !c.is_empty()).map(str::to_string),
                followup: None,
            }
        };
        let running = started.as_deref().is_some_and(|s| at.as_str() >= s);
        if let Some(why) = text.strip_prefix("started again") {
            events.push(event("started_again", why.trim_start_matches(" ·")));
        } else if text == "started" || text.starts_with("started · ") {
            if started.is_none() {
                started = Some(at.clone());
                planned = slots.iter().flat_map(|s| s.members.iter().cloned()).collect();
            }
            events.push(event("started", text["started".len()..].trim_start_matches(" ·")));
        } else if let Some(why) = text.strip_prefix("held —").or_else(|| (text == "held").then_some("")) {
            events.push(event("held", why));
        } else if let Some(rest) = text.strip_prefix("passed ") {
            events.push(event("passed", rest.split_once(" · ").map(|(_, why)| why).unwrap_or("")));
        } else if let Some(rest) = text.strip_prefix("follow-up by ") {
            // `<row> passes: <ids> · group N (new), ahead of M planned`, or
            // `<row> holds: <ids> · join group N · <verdict>`.
            let Some((head, tail)) = rest.split_once(':') else { continue };
            let mut head = head.split_whitespace();
            let (Some(row), Some(how)) = (head.next(), head.next()) else { continue };
            let mut parts = tail.splitn(3, " · ");
            let rows = ids(parts.next().unwrap_or(""));
            let place = parts.next().unwrap_or("").trim();
            let why = parts.next().unwrap_or("");
            let maker = Maker { at: at.clone(), by: Some(row.to_string()), via: "followup" };
            let joins = how == "holds";
            match joins {
                true => {
                    if let Some((n, _)) = place.strip_prefix("join group ").and_then(ordinal) {
                        if let Some(s) = slots.get_mut(n - 1) {
                            s.members.extend(rows.iter().cloned());
                        }
                    }
                }
                false => {
                    if let Some((n, _)) = place.strip_prefix("group ").and_then(ordinal) {
                        make(&mut slots, n - 1, rows.clone(), Some(maker.clone()), true, running);
                    }
                }
            }
            for r in &rows {
                if running && !planned.contains(r) {
                    entered.entry(r.clone()).or_insert_with(|| maker.clone());
                }
            }
            filed.extend(rows.iter().cloned());
            events.push(Event { followup: Some((row.to_string(), rows, joins)), ..event("followup", why) });
        } else if let Some((n, rest)) = text.strip_prefix("group ").and_then(ordinal) {
            if let Some(why) = rest.strip_prefix(" stop amended while it runs — why: ") {
                events.push(event("stop_amended", why.split(" · now: ").next().unwrap_or(why)));
            } else if let Some(rest) = rest.strip_prefix(" dropped") {
                let live = rest.starts_with(" while it runs");
                let body = rest.trim_start_matches(" while it runs").trim_start_matches(" — ");
                let (members, why) = body.split_once(" — why: ").unwrap_or((body, ""));
                let members = if members.trim() == "no members" { Vec::new() } else { ids(members) };
                // The group the line names, if it holds what the line says it
                // held; any group that does, if one moved; and if none does, a
                // group the replay never saw made, which leaves the replay as
                // it was.
                let i = Some(n - 1)
                    .filter(|&i| slots.get(i).is_some_and(|s| same(&s.members, &members)))
                    .or_else(|| slots.iter().position(|s| same(&s.members, &members)));
                if let Some(i) = i {
                    slots.remove(i);
                }
                if live {
                    let before = n.checked_sub(2).and_then(|b| slots.get(b)).map(|s| s.handle);
                    drops.push(Drop { before, at: at.clone(), why: why.trim().to_string(), members });
                }
            }
        } else if let Some(rest) = text.strip_prefix("removed ") {
            for id in ids(rest) {
                take(&mut slots, &id);
            }
        } else if let Some((listed, rest)) = text.split_once(" → group ") {
            // `add` and `mv` write the same line, the ordinal the group has
            // once the members are out of wherever they were.
            let Some((n, rest)) = ordinal(rest) else { continue };
            let by = rest.split_once(" · by ").map(|(_, b)| b.trim().to_string());
            let named = ids(listed);
            let moved = named.iter().map(|id| take(&mut slots, id)).fold(false, |a, b| a | b);
            let maker = Maker { at: at.clone(), by, via: if moved { "mv" } else { "add" } };
            for id in &named {
                if running && !planned.contains(id) {
                    entered.entry(id.clone()).or_insert_with(|| maker.clone());
                }
            }
            if rest.trim_start().starts_with("(new)") {
                let fresh = running && named.iter().any(|id| !planned.contains(id));
                make(&mut slots, n - 1, named, Some(maker), fresh, running);
            } else if let Some(s) = slots.get_mut(n - 1) {
                s.members.extend(named);
            }
        }
    }

    // The replay's groups, lined up with the file's: by position when every
    // one agrees, which is a log that recorded every edit; otherwise each
    // group by the replayed group with its members, or the most of them.
    let mut at_of: BTreeMap<usize, usize> = BTreeMap::new();
    if slots.len() == groups.len() && slots.iter().zip(groups).all(|(s, g)| same(&s.members, &g.members)) {
        at_of.extend(slots.iter().enumerate().map(|(i, s)| (s.handle, i)));
    } else {
        let mut used = BTreeSet::new();
        for (i, g) in groups.iter().enumerate() {
            let overlap = |s: &Slot| s.members.iter().filter(|m| g.members.contains(m)).count();
            let best = slots
                .iter()
                .enumerate()
                .filter(|(j, s)| !used.contains(j) && overlap(s) > 0)
                .max_by_key(|(_, s)| (same(&s.members, &g.members), overlap(s)))
                .map(|(j, _)| j);
            if let Some(j) = best {
                used.insert(j);
                at_of.insert(slots[j].handle, i);
            }
        }
    }

    // When each group's barrier was passed: the instant its verdict opens
    // with. A verdict with none — written by hand — was passed before
    // anything this can place.
    let passed: Vec<Option<&str>> = groups
        .iter()
        .map(|g| match g.verdict.trim() {
            "" => None,
            v => Some(v.split_whitespace().next().filter(|t| barrier::instant(t)).unwrap_or("")),
        })
        .collect();
    let standing = |t: &str| passed.iter().position(|p| p.is_none_or(|p| p >= t));
    let id_of = |i: usize| groups.get(i).and_then(|g| g.id);

    let mut dropped: Vec<Dropped> = drops
        .into_iter()
        .map(|d| Dropped {
            after: d.before.and_then(|h| at_of.get(&h).copied()).and_then(id_of),
            at: d.at,
            why: d.why,
            members: d.members,
            events: Vec::new(),
        })
        .collect();
    // Where the run came to each dropped group: the pass of the group before
    // it, or the start for a first group.
    let arrived: Vec<String> = dropped
        .iter()
        .map(|d| {
            let before = d.after.and_then(|id| groups.iter().position(|g| g.id == Some(id)));
            match before {
                Some(i) => passed[i].unwrap_or("").to_string(),
                None => started.clone().unwrap_or_default(),
            }
        })
        .collect();

    let mut out: Vec<Of> = vec![Of::default(); groups.len()];
    let mut late: BTreeMap<usize, String> = BTreeMap::new();
    for e in events {
        let gone = dropped.iter().zip(&arrived).position(|(d, a)| a.as_str() < e.at.as_str() && e.at < d.at);
        match (gone, standing(&e.at)) {
            (Some(d), _) => dropped[d].events.push(e),
            (None, Some(i)) => out[i].events.push(e),
            (None, None) => {}
        }
    }
    for (handle, making) in makings.into_iter().enumerate() {
        let (Some(i), Some(making)) = (at_of.get(&handle).copied(), making) else { continue };
        let barrier = standing(&making.maker.at).filter(|_| making.new_work).and_then(id_of);
        if making.late {
            late.insert(i, making.maker.at.clone());
        }
        out[i].made = Some(Made { maker: making.maker, barrier });
    }
    for (i, (g, of)) in groups.iter().zip(out.iter_mut()).enumerate() {
        // Only a group made once the run had started: one planned before it
        // has no `+`, and a member that joined it in the second it was made
        // — which a test does in every run — joined all the same.
        let made_at = late.get(&i);
        for m in &g.members {
            // Put on with the group itself — in the line that made it, or in
            // the same second — is the group's `+`, not the member's.
            if let Some(e) = entered.get(m).filter(|e| made_at != Some(&e.at)) {
                of.joined.insert(m.clone(), e.clone());
            }
        }
    }
    History { groups: out, dropped, filed }
}

/// A check, for `--json`.
pub(crate) fn check_json(c: &Check) -> serde_json::Value {
    json!({
        "at": c.at,
        // The one word renamed: see the module's last section.
        "verdict": match c.state {
            State::Holds => "hold",
            s => s.as_str(),
        },
        "read": c.read,
        "pane": c.pane,
        "on": c.on,
        "ended": c.ending == Ending::Ended,
        "end_failed": c.ending == Ending::Failed,
        "was": c.was,
    })
}

pub(crate) fn maker_json(m: &Maker) -> serde_json::Value {
    json!({ "at": m.at, "by": m.by, "via": m.via })
}

pub(crate) fn event_json(e: &Event) -> serde_json::Value {
    let mut v = json!({
        "at": e.at,
        "kind": e.kind,
        "why": (!e.why.is_empty()).then_some(&e.why),
        "caption": e.caption,
    });
    if let Some((by, rows, joins)) = &e.followup {
        v["by"] = json!(by);
        v["members"] = json!(rows);
        v["blocking"] = json!(joins);
    }
    v
}

/// A group's share of the history, as keys to put beside its own in
/// `worklist show --json`.
pub(crate) fn group_json(g: &Group, of: &Of, filed: &BTreeSet<String>, tasks: &[Task]) -> serde_json::Value {
    let follow_up_of: BTreeMap<&String, &String> = g
        .members
        .iter()
        .filter(|m| filed.contains(*m))
        .filter_map(|m| Some((m, tasks.iter().find(|t| &t.id == m)?.parent.as_ref()?)))
        .collect();
    json!({
        "made": of.made.as_ref().map(|m| {
            let mut v = maker_json(&m.maker);
            v["barrier"] = json!(m.barrier);
            v
        }),
        "checks": g.checks.iter().map(check_json).collect::<Vec<_>>(),
        "events": of.events.iter().map(event_json).collect::<Vec<_>>(),
        "joined": of.joined.iter().map(|(m, j)| (m.clone(), maker_json(j))).collect::<serde_json::Map<_, _>>(),
        "follow_up_of": follow_up_of,
    })
}

pub(crate) fn dropped_json(d: &Dropped) -> serde_json::Value {
    json!({
        "after": d.after,
        "at": d.at,
        "why": d.why,
        "members": d.members,
        "events": d.events.iter().map(event_json).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gpui-migration's shapes, at its own instants, cut to the lines that
    /// make each one: a follow-up group, a join before the run reached its
    /// group, the phantom group `wsp-232` made and the drop of it, a blocking
    /// follow-up and two holds on one barrier, and the group Ed put in at a
    /// hold beside a planned member moved to a group of its own.
    fn gpui_shaped() -> Worklist {
        let mut w = Worklist::new("gm", "gm");
        w.body = "\
## Groups
- 1  c-1
  verdict: 2026-10-07T09:37:36Z Go.
- 2  c-2  c-7
  verdict: 2026-10-07T12:04:11Z passes, with follow-ups
- 3  c-8
  verdict: 2026-10-07T12:32:58Z Barrier passes.
- 4  c-3  c-4  c-9
  verdict: 2026-10-08T04:31:10Z Group passes.
  checks: 2026-10-07T22:13:03Z blocking read:d99ce1fe68cb pane:cpd-559
          was:c-388 ended · 2026-10-07T22:41:27Z holds pane:cpd-562
- 5  c-5  c-10
  verdict: 2026-10-08T08:48:01Z Group 5 passes.
- 6  c-11  c-12  c-13
- 7  c-6

## Log
- 2026-10-05T12:04:35Z c-1 → group 1 (new)
- 2026-10-05T12:04:35Z c-2 → group 2 (new)
- 2026-10-05T12:04:35Z c-3 c-4 → group 3 (new)
- 2026-10-05T12:04:35Z c-5 → group 4 (new)
- 2026-10-05T12:04:35Z c-6 → group 5 (new)
- 2026-10-07T08:12:26Z started · Ed: start.
- 2026-10-07T09:37:36Z passed c-1 · Go.
- 2026-10-07T09:53:44Z c-7 → group 2
- 2026-10-07T12:04:10Z follow-up by c-369 passes: c-8 · group 3 (new), ahead of 3 planned
- 2026-10-07T12:04:11Z passed c-2 c-7 · passes, with follow-ups
- 2026-10-07T12:32:58Z passed c-8 · Barrier passes.
- 2026-10-07T20:09:44Z group 4 stop amended while it runs — why: made by a wsp defect · now: c-3 alone
- 2026-10-07T21:00:43Z group 4 dropped while it runs — Keystrokes in a field — why: Made by wsp-232's parser bug.
- 2026-10-07T22:21:23Z follow-up by c-388 holds: c-9 · join group 4 · Group HOLDS, pending one follow-up.
- 2026-10-07T22:21:59Z group 4 stop amended while it runs — why: the re-check is told both · now: the whole stop
- 2026-10-07T22:42:04Z held — HOLDS, on one clause only. · caption: a person's keyboard check
- 2026-10-08T04:30:50Z c-10 → group 5 · by wsp-for-ux
- 2026-10-08T04:31:10Z started again · Group passes.
- 2026-10-08T08:10:46Z held — Group 5 barrier: HOLD.
- 2026-10-08T08:47:14Z removed c-6
- 2026-10-08T08:47:14Z c-11 c-12 → group 6 (new) · by person
- 2026-10-08T08:47:14Z c-13 → group 6 · by person
- 2026-10-08T08:47:14Z c-6 → group 7 (new) · by person
- 2026-10-08T08:48:01Z started again · Group 5 passes.
"
        .to_string();
        w
    }

    fn timeline(of: &[Event]) -> Vec<(&str, &str)> {
        of.iter().map(|e| (e.at.as_str(), e.kind)).collect()
    }

    /// The four things wsp-254's done-when names on gpui-migration, in its
    /// shapes: a barrier's checks and its hold-and-start-again, a member
    /// joined by a check, a follow-up group's maker, and the dropped group.
    #[test]
    fn the_log_gives_each_group_its_timeline_its_maker_and_its_joins() {
        let w = gpui_shaped();
        let groups = w.groups();
        let h = of(&w, &groups);

        assert_eq!(
            timeline(&h.groups[3].events),
            vec![
                ("2026-10-07T22:21:23Z", "followup"),
                ("2026-10-07T22:21:59Z", "stop_amended"),
                ("2026-10-07T22:42:04Z", "held"),
                ("2026-10-08T04:31:10Z", "started_again"),
            ],
            "every line about the barrier the run stood at, by time, since `held` names no group"
        );
        let held = &h.groups[3].events[2];
        assert_eq!((held.why.as_str(), held.caption.as_deref()), ("HOLDS, on one clause only.", Some("a person's keyboard check")));
        let j = &h.groups[3].joined["c-9"];
        assert_eq!((j.at.as_str(), j.by.as_deref(), j.via), ("2026-10-07T22:21:23Z", Some("c-388"), "followup"));
        assert!(h.filed.contains("c-9") && h.filed.contains("c-8"), "both kinds of follow-up are a check's filing");

        let made = h.groups[2].made.as_ref().unwrap();
        assert_eq!(
            (made.maker.at.as_str(), made.maker.by.as_deref(), made.maker.via, made.barrier),
            ("2026-10-07T12:04:10Z", Some("c-369"), "followup", Some(2)),
        );
        assert_eq!(timeline(&h.groups[1].events), vec![("2026-10-07T12:04:10Z", "followup"), ("2026-10-07T12:04:11Z", "passed")]);
        assert_eq!(h.groups[1].joined["c-7"].via, "add");
        assert_eq!(h.groups[1].joined["c-7"].by, None, "a line from before `by` does not say");
        assert_eq!(timeline(&h.groups[0].events), vec![("2026-10-07T08:12:26Z", "started"), ("2026-10-07T09:37:36Z", "passed")]);
        assert_eq!(h.groups[0].made.as_ref().unwrap().barrier, None, "planned before the start");
    }

    /// The group `wsp-232` made was never written by an edit, so the replay
    /// never had it; its drop names what it held, which is how it is told
    /// from the group that took its place, and the amendment made while it
    /// ran goes with it rather than onto that group.
    #[test]
    fn a_group_dropped_while_it_ran_is_kept_with_what_happened_to_it() {
        let w = gpui_shaped();
        let h = of(&w, &w.groups());
        assert_eq!(h.dropped.len(), 1);
        let d = &h.dropped[0];
        assert_eq!((d.after, d.at.as_str()), (Some(3), "2026-10-07T21:00:43Z"));
        assert_eq!(d.why, "Made by wsp-232's parser bug.");
        assert_eq!(d.members, ["Keystrokes", "in", "a", "field"]);
        assert_eq!(timeline(&d.events), vec![("2026-10-07T20:09:44Z", "stop_amended")]);
        assert_eq!(h.groups.iter().map(|g| g.events.len()).sum::<usize>(), 11, "and the other eleven placed once each");
    }

    /// Ed's Q-6: a group put in at a hold carries its maker and the barrier,
    /// like a follow-up group. A member put in in the same second is the
    /// group's, and a planned member moved to a group of its own is the list
    /// rearranged — no barrier, and no join.
    #[test]
    fn a_group_put_in_at_a_hold_names_who_and_which_barrier() {
        let w = gpui_shaped();
        let h = of(&w, &w.groups());
        let made = h.groups[5].made.as_ref().unwrap();
        assert_eq!((made.maker.by.as_deref(), made.maker.via, made.barrier), (Some("person"), "add", Some(5)));
        assert!(h.groups[5].joined.is_empty(), "{:?}", h.groups[5].joined);
        let moved = h.groups[6].made.as_ref().unwrap();
        assert_eq!(moved.barrier, None);
        assert!(h.groups[6].joined.is_empty());
        let j = &h.groups[4].joined["c-10"];
        assert_eq!((j.by.as_deref(), j.via), (Some("wsp-for-ux"), "add"));
        assert_eq!(timeline(&h.groups[4].events), vec![("2026-10-08T08:10:46Z", "held"), ("2026-10-08T08:48:01Z", "started_again")]);
    }

    /// The check's word for holding the barrier is `hold` in the JSON, so the
    /// object never says `holds` for both a hold and a pass.
    #[test]
    fn a_check_that_held_says_hold_and_never_holds() {
        let w = gpui_shaped();
        let g = &w.groups()[3];
        let v: Vec<_> = g.checks.iter().map(check_json).collect();
        assert_eq!((v[0]["verdict"].as_str(), v[1]["verdict"].as_str()), (Some("blocking"), Some("hold")));
        assert_eq!(v[0]["read"], "d99ce1fe68cb");
        assert_eq!(v[0]["pane"], "cpd-559");
        assert_eq!((v[0]["ended"].as_bool(), v[1]["ended"].as_bool()), (Some(true), Some(false)));
        assert_eq!(v[0]["was"], "c-388");
    }

    /// A group the log never made — typed into the file — has no history,
    /// rather than somebody else's, and the groups it can place keep theirs.
    #[test]
    fn a_group_written_by_hand_has_no_history_and_takes_none_from_its_neighbours() {
        let mut w = gpui_shaped();
        w.body = w.body.replace("- 7  c-6\n", "- 7  c-6\n- 8  c-99\n");
        let h = of(&w, &w.groups());
        assert_eq!(h.groups[7], Of::default());
        assert_eq!(h.groups[2].made.as_ref().unwrap().barrier, Some(2));
        assert!(h.groups[3].joined.contains_key("c-9"));
    }
}
