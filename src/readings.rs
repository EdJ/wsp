//! The readings the daemon writes (wsp-743): each census read's input that the
//! CLI works out here in `src/` and no contract entity carries, taken with the
//! CLI's own builder and written through the store's `Readings` as the JSON
//! of `wsp-view`'s input struct, under the name `wsp_view::reading` gives it.
//! The API's `readings` verb serves them, so a reader (web-chrome's census)
//! draws what `ls`, `wip`, `worklist ls`, `machine ls`, `throttle`, `spend`,
//! `flag --person` and `yours` print, with the same `wsp-view` function and no `wsp` child.
//!
//! One thread, beside the mirror's: each reading is taken again on its own
//! period ([`PERIODS`]); a builder that panics is one `cycle.log` line and is
//! tried again next period. `WSP_READINGS=off` writes none.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wsp_server::SharedStore;
use wsp_store::store::Store;
use wsp_view::reading as r;

/// How often each reading is taken again. `ls --all`'s stages and `spend`'s
/// ledger walk are the dear ones.
pub const PERIODS: [(&str, Duration); 7] = [
    (r::THROTTLE, Duration::from_secs(10)),
    (r::MACHINES, Duration::from_secs(10)),
    (r::WIP, Duration::from_secs(10)),
    (r::WORKLISTS, Duration::from_secs(10)),
    (r::FLAGS, Duration::from_secs(10)),
    (r::TASKS, Duration::from_secs(30)),
    (r::SPEND, Duration::from_secs(60)),
];

/// `WSP_READINGS=off`: the daemon writes no readings.
fn off() -> bool {
    std::env::var("WSP_READINGS").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// The reading `name`'s JSON text, taken now with the CLI's builder; `None`
/// for a name this build does not take.
pub fn take(store: &Store, name: &str) -> Option<String> {
    Some(match name {
        r::TASKS => r::text(&crate::cmd_task::tasks_reading(store)),
        r::WIP => r::text(&crate::cmd_agent::wip_reading(store)),
        r::WORKLISTS => r::text(&crate::cmd_worklist::listings(store)),
        r::MACHINES => r::text(&crate::cmd_machine::view(&crate::cmd_machine::Fleet::live(store))),
        r::THROTTLE => r::text(&crate::throttle::view(store)),
        r::SPEND => r::text(&crate::spend::reading(store, wsp_base::util::epoch_secs())),
        r::FLAGS => r::text(&crate::cmd_message::flags_reading(store)),
        _ => return None,
    })
}

/// Start the readings thread: every second it takes each reading whose period
/// has passed (all of them at start) and writes it to `db`.
pub fn start(store: &Store, db: SharedStore) -> Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)> {
    if off() {
        return None;
    }
    let (root, state) = (store.root.clone(), store.state.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::Builder::new()
        .name("readings".into())
        .spawn(move || {
            let store = Store::at(root, state);
            let say = |line: String| wsp_store::store::log_line(&store, &format!("readings: {line}"));
            let mut last: [Option<Instant>; PERIODS.len()] = [None; PERIODS.len()];
            while !flag.load(Ordering::SeqCst) {
                for (i, (name, period)) in PERIODS.iter().enumerate() {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    if last[i].is_some_and(|t| t.elapsed() < *period) {
                        continue;
                    }
                    last[i] = Some(Instant::now());
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| take(&store, name))) {
                        Ok(Some(json)) => {
                            if let Err(e) = db.set_reading(name, json) {
                                say(format!("{name}: not written: {e:?}"));
                            }
                        }
                        Ok(None) => {}
                        Err(_) => say(format!("{name}: its builder panicked; tried again in {}s", period.as_secs())),
                    }
                }
                for _ in 0..10 {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
        .expect("the readings thread starts");
    Some((handle, stop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use wsp_store::model::Task;
    use wsp_view::{flag, machine, spend, throttle, wip, worklist};

    /// The `tasks` reading beside the mirror's rows cut to heads (what `rows {heads: true}`
    /// serves) gives `ls --json --all` row for row: each is `ls_json_row` of the head's
    /// record, its stage and its legs, in the reading's order.
    #[test]
    fn the_tasks_reading_and_the_heads_give_ls_json_all() {
        let store = crate::testkit::store("readings-tasks");
        let mut staged = Task::new("A staged row", "s-2");
        staged.status_raw = "doing".into();
        crate::legs::write(&mut staged, &[crate::legs::Leg::pending("scout", "look"), crate::legs::Leg::pending("finish", "land")]);
        store.save_task(&staged).unwrap();
        let mut done = Task::new("A done row", "s-1");
        done.status_raw = "done".into();
        done.tags = vec!["verify".into()];
        store.save_task(&done).unwrap();
        let mut plain = Task::new("A plain row", "s-3");
        plain.status_raw = "todo".into();
        plain.priority_raw = "high".into();
        crate::model::set_section_in(&mut plain.body, "Log", "- 2026-10-08T08:00:00Z claimed by pane w1:p1\n");
        store.save_task(&plain).unwrap();

        let reading: r::Tasks = r::of(&take(&store, r::TASKS).unwrap()).expect("the reading decodes as its shape");
        let tasks = store.tasks();
        let heads: std::collections::BTreeMap<String, wsp_store_contract::Row> =
            crate::mirror::rows(&tasks, &store.claims(), &store.worklists(), &crate::mirror::texts_of(&store, &tasks), &Default::default())
                .into_iter()
                .filter_map(|e| match e {
                    wsp_store_contract::Entity::Row(mut row) => {
                        wsp_store_contract::heads_only(&mut row.fields);
                        Some((row.id.0.clone(), row))
                    }
                    _ => None,
                })
                .collect();
        let drawn: Vec<Value> = reading
            .order
            .iter()
            .map(|id| {
                let record = wsp_view::common::task_json(&heads[id]);
                wsp_view::task::ls_json_row(record, reading.stages.get(id), reading.legs.get(id).map(Vec::as_slice))
            })
            .collect();

        let cli: Vec<Value> = serde_json::from_str(&crate::cmd_task::rows_json(&store, &crate::cmd_task::ls_all(&store))).unwrap();
        assert_eq!(reading.order, ["s-2", "s-3", "s-1"], "ls's order: doing, then todo, then done");
        assert_eq!(drawn, cli);
        assert!(drawn[0].get("legs").is_some() && drawn[1].get("legs").is_none(), "legs only where it has them");
    }

    /// Each other reading's shape keeps what its projection draws: decoded from its text, it
    /// draws the JSON the value it was written from draws (fractions, tuples, nulls and maps
    /// included).
    #[test]
    fn every_reading_shape_draws_the_same_after_its_text() {
        // A macro, as the binary names no `serde`: the shape is the one the projection takes.
        macro_rules! again {
            ($v:expr) => {
                r::of(&r::text(&$v)).expect("decodes")
            };
        }

        let t = throttle::Throttle {
            mode: "auto".into(),
            in_force: true,
            idle_secs: Some(12),
            cores: 8,
            slots: 2,
            jobs: 4,
            load: Some((3.25, 2.5, 0.125)),
            load_warning: true,
            memory: json!({"free_percent": 41, "held": false}),
            cpu_speed_limit: Some(70),
            thermal_below: 80,
            thermally_limited: true,
            hot_slots: 1,
            hot_jobs: 2,
            suggested_agents: Some(3),
            agents: None,
            builds: vec![(json!({"pane": "cpd-1"}), Some(0)), (json!("x"), None)],
        };
        assert_eq!(throttle::json(&again!(t)), throttle::json(&t));

        let s = r::Spend {
            lines: vec![spend::Line { seat: "cpd-1".into(), ended: 1_763_400_000, model: "claude-opus-5-5".into(), micros: 1_250_000, input: 10, output: 20, cache_read: 30, cache_write: 40, turns: 5 }],
            places: [("cpd-1".to_string(), spend::Place { list: Some("agents-fast".into()), project: Some("wsp".into()) })].into(),
            landed: 2,
        };
        let back: r::Spend = again!(s);
        let now = 1_763_450_000;
        assert_eq!(spend::report(&back.lines, &back.places(), back.landed, now), spend::report(&s.lines, &s.places(), s.landed, now));

        let l = vec![worklist::Listing {
            head: worklist::Head {
                id: "w".into(),
                title: "W".into(),
                status: "executing".into(),
                held: true,
                planned: Some("2026-11-20".into()),
                groups: 3,
                at: Some(2),
                outputs: vec![worklist::Output { name: "o".into(), kind: "md".into(), file: "o.md".into(), present: true }],
                ..Default::default()
            },
            segment: "running".into(),
            open: 4,
            gone: vec!["x-1".into()],
            unwritten: vec![3],
            activity: 1_763_400_000,
            prio: "high".into(),
            after: None,
            queue: Some(worklist::Queue { n: 1, waits: Some("wsp-1".into()) }),
        }];
        let back_l: Vec<worklist::Listing> = again!(l);
        assert_eq!(worklist::list_json(&back_l), worklist::list_json(&l));

        let f = machine::Fleet {
            machines: vec![
                (machine::Record { name: "i5".into(), agents: Some(2), lease: 180, ..Default::default() }, Some(machine::Live { reachable: true, wire: Some(3), leases: [("a".to_string(), "b".to_string())].into(), tick_secs: 5, ..Default::default() })),
                (machine::Record { name: "pi".into(), ..Default::default() }, None),
            ],
            agents: vec![machine::Standing { machine: "i5".into(), pane: "p1".into(), ..Default::default() }],
            seat: "mac".into(),
            seat_cap: Some(6),
            seat_working: vec!["cpd-1".into()],
        };
        assert_eq!(machine::list_json(&again!(f)), machine::list_json(&f));

        let c = wip::Census {
            agents: vec![wip::Agent {
                task_id: "wsp-743".into(),
                pane: "cpd-1".into(),
                turning: true,
                seat: Some("cpd-1".into()),
                waiting: Some(json!({"on": "person"})),
                role: json!({"kind": "leg", "of": "wsp-743"}),
                queued: 2,
                owed: true,
                leg: Some(json!({"name": "contract-store"})),
                ..Default::default()
            }],
            head: wip::Head { attention: json!({"needs_you": [], "follow_up": []}), daemon: Some("up".into()), idle_memory: json!(null), inbox: 3, ..Default::default() },
            waiting_at_cap: vec!["wsp-1 waits".into()],
            planned_started: Some(vec![json!({"worklist": "w"})]),
        };
        assert_eq!(wip::census_json(&again!(c)), wip::census_json(&c));
        let item = flag::Item {
            record: json!({"id": "m-1", "caption": {"from": "derived", "text": "Ship?"}, "text": "ship?", "about": {"task": "wsp-1"}, "at": "2026-11-23T00:00:00Z", "waiting": null, "for": "person", "choices": {"kind": "ask", "questions": []}}),
            asked: "ship?".into(),
            from: "cpd-1".into(),
            project: Some("wsp".into()),
            unblocks: Some("w group 2".into()),
        };
        let fl = flag::Flags {
            person: vec![item.record.clone(), json!({"id": "m-0", "shape": "notification", "closed": {"how": "answered"}})],
            project: None,
            items: vec![item.clone()],
            review: 4,
            closed_by_the_run: 1,
            retro: vec!["w cleared".into()],
            closed: vec![flag::Closed { item, closed: json!({"how": "answered", "by": "person", "answer": {"text": "yes"}}) }],
        };
        let back_f: flag::Flags = again!(fl);
        assert_eq!(flag::person_json(&back_f.person), flag::person_json(&fl.person));
        assert_eq!(flag::yours_json(&back_f), flag::yours_json(&fl));

        let bare = wip::Census::default();
        let v = wip::census_json(&again!(bare));
        assert!(v.get("waiting_at_cap").is_none() && v.get("planned_started").is_none(), "the CLI's keys stay absent: {v}");
    }

    /// wsp-743's measure: each reply the census takes, built in-process on the
    /// live store and sized as the frame the server writes, against `MAX_LINE`.
    /// The SQL replies come from a read-only open of `state.db`; each reading is
    /// [`take`]n with the CLI's own builder over the live files (the read paths
    /// `wsp ls`, `wip`, `worklist ls`, `machine ls`, `throttle` and `spend`
    /// run), and one `readings` reply carries all seven. It writes nothing. Not
    /// run with the suite; the `wip` builder opens the store by its env, which
    /// a test must name, so on that one command:
    /// `WSP_HOME=~/wsp WSP_STATE=~/.local/state/wsp cargo test --bin wsp readings::tests::the_census_replies_on_the_live_store -- --ignored --nocapture`
    #[test]
    #[ignore = "reads the live store, to measure"]
    fn the_census_replies_on_the_live_store() {
        use wsp_api::{Answer, Frame, Outcome, Reply, ReqId, MAX_LINE};
        use wsp_store_contract::{AskState, Reading, Reads, ListFilter, RowFilter};

        // `Store::open` refuses the live store in a test, on purpose; this reads it and writes nothing.
        let store = Store::at(wsp_base::util::home().join("wsp"), Store::default_state());
        let db = wsp_store_sqlite::SqliteStore::open_read_only(store.state.join("state.db")).expect("state.db opens read-only");
        let size = |answer: Answer| {
            let frame = Frame::Reply(Reply { id: ReqId(1), outcome: Outcome::Ok(answer), events: vec![] });
            serde_json::to_string(&frame).unwrap().len() + 1
        };
        let line = |what: &str, bytes: usize, took: Duration| {
            eprintln!("{what:<34} {bytes:>12} B  {:>6.2}% of MAX_LINE  {:>6} ms", bytes as f64 * 100.0 / MAX_LINE as f64, took.as_millis());
            assert!(bytes < MAX_LINE, "{what} is {bytes} B, past MAX_LINE");
        };
        let machine = crate::throttle::view(&store);
        eprintln!("load (1m 5m 15m) {:?} on {} cores", machine.load, machine.cores);
        eprintln!("MAX_LINE {MAX_LINE} B");

        let timed = |f: &dyn Fn() -> Answer| {
            let began = Instant::now();
            let a = f();
            (size(a), began.elapsed())
        };
        let (b, t) = timed(&|| Answer::Rows(db.rows(&RowFilter { heads: true, ..Default::default() }).unwrap()));
        line("rows {heads: true}", b, t);
        let (b, t) = timed(&|| Answer::Lists(db.lists(&ListFilter { heads: true }).unwrap()));
        line("lists {heads: true}", b, t);
        let (b, t) = timed(&|| Answer::Lists(db.lists(&ListFilter::default()).unwrap()));
        line("lists {} (unchanged)", b, t);
        let asks = |states: Vec<AskState>| {
            let (st, _) = db.snapshot().unwrap();
            Answer::Asks(st.asks.into_values().filter(|a| states.is_empty() || states.contains(&a.state)).collect())
        };
        let (b, t) = timed(&|| asks(vec![AskState::Raised]));
        line("asks {states: [raised]} (flags)", b, t);
        let (b, t) = timed(&|| asks(vec![]));
        line("asks {}", b, t);
        let (b, t) = timed(&|| {
            let (state, at) = db.wip_snapshot().unwrap();
            Answer::Wip { state, at }
        });
        line("wip (unchanged)", b, t);

        let mut all = std::collections::BTreeMap::new();
        for name in r::ALL {
            let began = Instant::now();
            let json = take(&store, name).expect("every name is taken");
            let took = began.elapsed();
            let one = Reading { json, at: wsp_store_contract::At(wsp_base::util::epoch_secs() as u64) };
            line(&format!("readings {{names: [{name}]}}"), size(Answer::Readings([(name.to_string(), one.clone())].into())), took);
            all.insert(name.to_string(), one);
        }
        line("readings {} (all seven)", size(Answer::Readings(all)), Duration::ZERO);
    }

    /// Every name `wsp-view` lists is one this module takes, with a period.
    #[test]
    fn every_reading_is_taken_on_a_period() {
        let named: Vec<&str> = PERIODS.iter().map(|(n, _)| *n).collect();
        let mut all = r::ALL.to_vec();
        all.sort();
        let mut periods = named.clone();
        periods.sort();
        assert_eq!(periods, all);
        let store = crate::testkit::store("readings-names");
        assert_eq!(take(&store, "nothing"), None);
    }
}
