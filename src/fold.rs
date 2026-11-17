//! The daemon folds the manager's events (`docs/agent-manager.md` 3.1, 4.1
//! and 4.3; M1's group 5, wsp-028).
//!
//! Under `WSP_HOST=manager` the daemon holds one [`Subscription`] per manager
//! (today: the one the local machine record names) on a thread of its own,
//! not paced by the tick (3.4). Each event it hears is written into the files
//! the rest of wsp already reads, by the store's own writers for each:
//!
//! | event | file |
//! |---|---|
//! | `seat.started` | `spawned.json` (`started`), `bindings.json` and `claims.json` (the row the spawn's key named, where none is held) |
//! | `seat.refused` | `spawned.json` (`refused`) |
//! | `turn` | the seat's `said.json` (`working`/`idle`), `seat-since.json`, `burn.json`'s `turns` |
//! | `seat.said` | the seat's `said.json` (`said`, `said_at`) |
//! | `seat.spent` | the seat's `burn.json` (tokens and cost) |
//! | `seat.stopped` | `spent.jsonl` (the burn banked), `said.json` and `seat-since.json` (`gone`), `manager/<host>.ended.json` |
//! | `seat.did` | `manager/<host>.did.jsonl`, one line per digest |
//! | `seat.stuck` | `manager/<host>.stuck.json`, the newest per seat |
//! | `order.*` | `manager/<host>.orders.json`, the newest per order |
//! | `beat` (with `HostFacts`) | `manager/<host>.beat.json` |
//!
//! Nothing here asks the OS: no `ps`, no renderer, no `df`. The facts are the
//! event's.
//!
//! **At least once, folded once.** The subscription keeps the last id it
//! handed out only once the next is asked for, so an event can be heard
//! twice (a daemon that died between the two, a replay that overlaps). The
//! last id folded is kept per host beside the files
//! ([`folded_path`], `<state>/manager/<host>.folded`) and an id at or below it
//! is skipped, so a resubscribe folds nothing twice.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use wsp_host_api::{EventId, Frame, Message, TurnEdge};

use crate::host::{Host as _, HostError, Subscription, LOCAL};
use crate::place::Seat;
use crate::place_compound::Compound;
use crate::store::{write_atomic, Store};
use crate::util;

/// How long one ask of the subscription waits. Short enough that a stop is
/// heard within it; the subscription beats on its own clock regardless.
const ASK: Duration = Duration::from_secs(2);
/// The longest wait between attempts to reach a manager that did not answer.
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Where `seat-since.json` says a seat's state came from.
const FROM: &str = "manager";

// ---------------------------------------------------------------- where it is kept

fn beside(state: &Path, host: &str, ext: &str) -> PathBuf {
    // `last_path` names the host safely; everything of one host sits beside it.
    wsp_wire::host_tcp::last_path(state, host).with_extension(ext)
}

/// The last event id folded from `host`.
pub fn folded_path(state: &Path, host: &str) -> PathBuf {
    beside(state, host, "folded")
}

/// The newest beat that carried `HostFacts`, as `{ heard, beat }` (`heard` in
/// epoch seconds here): what `sweep:disk` and `sweep:throttle` read.
pub fn beat_path(state: &Path, host: &str) -> PathBuf {
    beside(state, host, "beat.json")
}

/// One `seat.did` per line, as the event carries it, `heard` added: what
/// `ending:verification` reads in place of a transcript.
pub fn did_path(state: &Path, host: &str) -> PathBuf {
    beside(state, host, "did.jsonl")
}

/// seat -> its newest `seat.stuck`, `heard` added: what the stuck tick reads.
pub fn stuck_path(state: &Path, host: &str) -> PathBuf {
    beside(state, host, "stuck.json")
}

/// seat -> its `seat.stopped`, `heard` added: the seats the manager says have
/// ended, for the phase that tidies their panes.
pub fn ended_path(state: &Path, host: &str) -> PathBuf {
    beside(state, host, "ended.json")
}

/// order -> its newest `order.delivered` / `order.acked` / `order.refused`.
pub fn orders_path(state: &Path, host: &str) -> PathBuf {
    beside(state, host, "orders.json")
}

pub fn read_folded(state: &Path, host: &str) -> Option<EventId> {
    std::fs::read_to_string(folded_path(state, host)).ok()?.trim().parse().ok().map(EventId)
}

fn read(path: &Path) -> Value {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}))
}

fn write(path: &Path, v: &Value) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = write_atomic(path, &v.to_string());
}

/// One key of a JSON map file set to `v`.
fn put(path: &Path, key: &str, v: Value) {
    let mut m = match read(path) {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    m.insert(key.to_string(), v);
    write(path, &Value::Object(m));
}

fn with_heard(v: Result<Value, serde_json::Error>) -> Value {
    let mut v = v.unwrap_or(json!({}));
    v["heard"] = json!(util::epoch_secs());
    v
}

// ---------------------------------------------------------------- the fold

/// A seat's directory among the compound seats, made if it is not there: the
/// manager saying it runs the seat is the evidence the directory stands for.
fn seat_dir(store: &Store, seat: &str) -> Option<PathBuf> {
    let dir = Compound::at(store.state.join(crate::place_compound::SEATS)).dir_of(&Seat::new(seat)).ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// `said.json` with `f` applied, through the host's own writer.
fn say(store: &Store, seat: &str, f: impl FnOnce(&mut serde_json::Map<String, Value>)) {
    let Some(dir) = seat_dir(store, seat) else { return };
    let mut m = match LOCAL.said(&dir) {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    f(&mut m);
    LOCAL.say(&dir, &Value::Object(m));
}

/// `burn.json` with `f` applied.
fn burn(store: &Store, seat: &str, f: impl FnOnce(&mut serde_json::Map<String, Value>)) {
    let Some(dir) = seat_dir(store, seat) else { return };
    let path = dir.join(crate::host::BURN_FILE);
    let mut m = match read(&path) {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    f(&mut m);
    write(&path, &Value::Object(m));
}

/// The seat's state as `seat-since.json` keeps it, dated now.
fn since(store: &Store, seat: &str, state: &str) {
    let seen = [(seat.to_string(), state.to_string(), String::new(), Some(FROM.to_string()))];
    store.note_seat_states(&seen, util::epoch_secs(), &|_| false);
}

/// Fold one frame heard from `host`'s manager. `true` when it was written,
/// `false` when it was skipped: an event already folded, or a frame that
/// carries nothing to keep.
pub fn fold(store: &Store, host: &str, frame: &Frame) -> bool {
    if let Message::Beat(b) = &frame.msg {
        if b.host.is_none() {
            return false;
        }
        write(&beat_path(&store.state, host), &json!({ "heard": util::epoch_secs(), "beat": b }));
        return true;
    }
    let Some(id) = frame.id else { return false };
    if read_folded(&store.state, host).is_some_and(|f| id <= f) {
        return false;
    }
    apply(store, host, &frame.msg);
    let path = folded_path(&store.state, host);
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = write_atomic(&path, &format!("{}\n", id.0));
    true
}

fn apply(store: &Store, host: &str, msg: &Message) {
    let state = &store.state;
    match msg {
        Message::SeatStarted(s) => {
            let (seat, row) = (s.seat.0.as_str(), s.key.row.0.as_str());
            let mut rec = match store.spawned().remove(seat) {
                Some(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            rec.remove("refused");
            rec.insert(
                "started".into(),
                json!({ "host": host, "row": row, "attempt": s.key.attempt, "epoch": s.epoch.0, "pid": s.pid, "sockets": s.sockets }),
            );
            store.set_spawned(seat, Some(&Value::Object(rec)));
            // The row as the spawn named it, where nothing holds the seat or
            // the row already: a spawn's own records are richer and stand.
            if !row.is_empty() {
                store.locked(|| {
                    match store.bindings().get(seat) {
                        None => store.set_binding(
                            seat,
                            json!({ "task_id": row, "pane_id": seat, "host": host, "epoch": s.epoch.0, "started_at": util::now_iso() }),
                        ),
                        Some(b) if b.get("task_id").and_then(Value::as_str) == Some(row) && b.get("epoch") != Some(&json!(s.epoch.0)) => {
                            let mut b = b.clone();
                            b["epoch"] = json!(s.epoch.0);
                            store.set_binding(seat, b);
                        }
                        Some(_) => {}
                    }
                    if !store.claims().contains_key(row) {
                        store.set_claim(row, json!({ "workspace_id": seat, "host": host, "epoch": s.epoch.0, "claimed_at": util::now_iso() }));
                    }
                });
            }
            since(store, seat, "starting");
        }
        Message::SeatRefused(r) => {
            let seat = r.seat.0.as_str();
            let mut rec = match store.spawned().remove(seat) {
                Some(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            rec.insert(
                "refused".into(),
                json!({ "host": host, "row": r.key.row.0, "attempt": r.key.attempt, "why": r.why, "detail": r.detail }),
            );
            store.set_spawned(seat, Some(&Value::Object(rec)));
        }
        Message::Turn(t) => {
            let seat = t.seat.0.as_str();
            let word = match t.edge {
                TurnEdge::Began => "working",
                TurnEdge::Ended => "idle",
            };
            say(store, seat, |m| {
                m.insert("state".into(), json!(word));
                m.insert("at".into(), json!(util::iso_at((t.at.0 / 1000) as i64)));
                m.insert("epoch".into(), json!(t.epoch.0));
                m.insert("from".into(), json!(FROM));
            });
            if t.edge == TurnEdge::Began {
                burn(store, seat, |m| {
                    let n = m.get("turns").and_then(Value::as_u64).unwrap_or(0);
                    m.insert("turns".into(), json!(n + 1));
                });
            }
            since(store, seat, word);
        }
        Message::SeatSaid(s) => say(store, &s.seat.0, |m| {
            m.insert("said".into(), json!(s.text));
            m.insert("said_at".into(), json!(util::iso_at((s.at.0 / 1000) as i64)));
        }),
        Message::SeatSpent(s) => burn(store, &s.seat.0, |m| {
            // Totals so far for this start: each one replaces the last.
            m.insert("input".into(), json!(s.tokens.input));
            m.insert("output".into(), json!(s.tokens.output));
            m.insert("cache_read".into(), json!(s.tokens.cache_read));
            m.insert("cache_write".into(), json!(s.tokens.cache_write));
            m.insert("cost".into(), json!(s.cost_micros));
            m.insert("epoch".into(), json!(s.epoch.0));
        }),
        Message::SeatStopped(s) => {
            let seat = s.seat.0.as_str();
            if let Some(dir) = seat_dir(store, seat) {
                LOCAL.bank(&dir);
            }
            say(store, seat, |m| {
                m.insert("state".into(), json!("gone"));
                m.insert("at".into(), json!(util::now_iso()));
            });
            since(store, seat, "gone");
            put(&ended_path(state, host), seat, with_heard(serde_json::to_value(s)));
        }
        Message::SeatDid(d) => {
            let path = did_path(state, host);
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = LOCAL.append_line(&path, &with_heard(serde_json::to_value(d)).to_string());
        }
        Message::SeatStuck(s) => put(&stuck_path(state, host), &s.seat.0, with_heard(serde_json::to_value(s))),
        Message::OrderDelivered(o) => put(&orders_path(state, host), &o.order.0, json!({ "seat": o.seat.0, "delivered": util::epoch_secs() })),
        Message::OrderAcked(o) => put(&orders_path(state, host), &o.order.0, json!({ "seat": o.seat.0, "acked": o.turn_began.0 })),
        Message::OrderRefused(o) => put(&orders_path(state, host), &o.order.0, json!({ "seat": o.seat.0, "refused": o.why })),
        _ => {}
    }
}

// ---------------------------------------------------------------- what the tick reads
//
// Under `WSP_HOST=manager` the tick's phases read what was folded above, and
// probe nothing (`docs/agent-manager.md` 4.3; wsp-028's second leg):
// `sweep:disk` and `sweep:throttle` read [`facts`], `ending:verification`
// reads [`did_since`] and the stuck tick reads [`stalls`]. A fact not folded
// yet is no fact: nothing here falls back to the probe it replaces.

/// `WSP_HOST=manager`: the manager's events are the cause, and the phases
/// they replace do not run.
pub fn under_manager() -> bool {
    matches!(crate::host::Choice::from_env(), Ok(crate::host::Choice::Manager))
}

/// The host whose manager is folded, under `WSP_HOST=manager` with a machine
/// record; `None` otherwise.
fn folded_host(store: &Store) -> Option<String> {
    if !under_manager() {
        return None;
    }
    crate::manager::read_record(&store.state).map(|(local, _)| local.host)
}

/// The host facts of the newest beat folded, under `WSP_HOST=manager`.
pub fn facts(store: &Store) -> Option<wsp_host_api::HostFacts> {
    let host = folded_host(store)?;
    serde_json::from_value(read(&beat_path(&store.state, &host))["beat"]["host"].clone()).ok()
}

/// What a seat did since `since` (epoch seconds), out of the `seat.did`
/// digests folded for it: every digest line in [`crate::did`]'s own shape
/// (`- **ran:** …`, `- **drove:** …`, `- **read:** …`) taken in. `None` when no digest of the
/// seat since then says anything in that shape: a digest that is a summary
/// (`asked: …; tools: …`) names no command and no outcome, and a reading made
/// up from it would say the seat ran nothing.
pub(crate) fn did_since(store: &Store, seat: &str, since: i64) -> Option<crate::did::Did> {
    let host = folded_host(store)?;
    let text = std::fs::read_to_string(did_path(&store.state, &host)).ok()?;
    let from = (since.max(0) as u64).saturating_mul(1000);
    let mut did = crate::did::Did::default();
    let mut any = false;
    for d in text.lines().filter_map(|l| serde_json::from_str::<wsp_host_api::SeatDid>(l).ok()) {
        if d.seat.0 != seat || d.at.0 < from {
            continue;
        }
        for (key, body) in d.digest.lines().filter_map(crate::did::split_line) {
            if key != "checked" {
                did.absorb(key, body);
                any = true;
            }
        }
    }
    any.then_some(did)
}

/// The seat's stall as the manager reported it (`seat.stuck`), for the stuck
/// tick in place of the transcript's: open while no turn edge was folded for
/// the seat after it was heard. Its start is the heard time less the seconds
/// it had waited, kept at the one already recorded for the same command so a
/// later report of the same stall is not a new one.
pub(crate) fn stalls(store: &Store, seat: &str, recorded: Option<(&str, i64)>) -> Vec<crate::stuck::Stall> {
    let Some(host) = folded_host(store) else { return Vec::new() };
    let s = &read(&stuck_path(&store.state, &host))[seat];
    let (Some(command), Some(waited), Some(heard)) = (s["command"].as_str(), s["waited_secs"].as_i64(), s["heard"].as_i64()) else {
        return Vec::new();
    };
    let mut at = heard - waited;
    if let Some((was, then)) = recorded {
        if was == command && (then - at).abs() <= 10 {
            at = then;
        }
    }
    // A turn edge folded after the report: the call it was in has returned.
    let edge = seat_dir(store, seat).map(|d| LOCAL.said(&d)).and_then(|v| v["at"].as_str().map(util::epoch_of));
    let open = edge.is_none_or(|e| e <= heard);
    let kind = crate::stuck::classify(command).unwrap_or(crate::stuck::Kind::TimedOut);
    let secs = if open { (util::epoch_secs() - at).max(waited) } else { waited };
    vec![crate::stuck::Stall { kind, command: command.to_string(), at, secs, open }]
}

// ---------------------------------------------------------------- the thread

/// One ask of the subscription, and its frame folded. `Ok(true)` when a frame
/// was written.
pub fn step(store: &Store, host: &str, sub: &mut Subscription, wait: Duration) -> Result<bool, HostError> {
    Ok(sub.next(wait)?.is_some_and(|f| fold(store, host, &f)))
}

/// The thread's loop: ask, fold, again, until `stop`. A manager that cannot be
/// reached is asked again after a wait that doubles to [`RETRY_MAX`].
pub fn follow(store: &Store, host: &str, sub: &mut Subscription, stop: &AtomicBool) {
    let mut retry = Duration::from_secs(1);
    while !stop.load(Ordering::SeqCst) {
        match step(store, host, sub, ASK) {
            Ok(_) => retry = Duration::from_secs(1),
            Err(_) => {
                std::thread::sleep(retry);
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
    }
    sub.close();
}

/// Start the thread for the manager the local machine record names, under
/// `WSP_HOST=manager` only. `None` under `inproc`, or with no record.
pub fn start(store: &Store) -> Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)> {
    if !matches!(crate::host::Choice::from_env(), Ok(crate::host::Choice::Manager)) {
        return None;
    }
    let (local, token) = crate::manager::read_record(&store.state)?;
    let (root, state) = (store.root.clone(), store.state.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::Builder::new()
        .name(format!("fold:{}", local.host))
        .spawn(move || {
            let store = Store::at(root, state.clone());
            let mut sub = Subscription::new(local.socket(), token, &local.host, &state, env!("CARGO_PKG_VERSION"));
            follow(&store, &local.host, &mut sub, &flag);
        })
        .ok()?;
    Some((handle, stop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use wsp_host_api::{
        At, Beat, DiskFree, Epoch, HostFacts, Key, MemoryPressure, OrderId, RowId, SeatDid, SeatId, SeatSaid, SeatSpent,
        SeatStarted, SeatStopped, SeatStuck, Thermal, Token, Tokens, Turn, FileRef, HostId,
    };
    use wsp_manager::{Config, Manager, TOKEN_FILE};

    const HOST: &str = "test-host";

    fn manager(exec: &Path, port: u16) -> Manager {
        let mut cfg = Config::new(exec, HOST);
        cfg.port = port;
        // No beat in the window: the facts a beat carries are the manager's,
        // and its probes are not the fold's.
        cfg.beat = Duration::from_secs(3600);
        Manager::start(cfg).expect("the manager starts on loopback")
    }

    fn token(exec: &Path) -> Token {
        Token(std::fs::read_to_string(exec.join(TOKEN_FILE)).unwrap().trim().to_string())
    }

    fn seat() -> SeatId {
        SeatId("cpd-f1".into())
    }

    /// The stream a seat's life makes: started, a turn, a word, its spend, its stop.
    fn script(m: &Manager) -> Vec<u64> {
        let s = seat();
        let e = Epoch(1);
        let msgs = vec![
            Message::SeatStarted(SeatStarted {
                key: Key { row: RowId("wsp-028".into()), attempt: 1 },
                seat: s.clone(),
                epoch: e,
                pid: 4242,
                sockets: vec!["/tmp/s.sock".into()],
            }),
            Message::Turn(Turn { seat: s.clone(), epoch: e, edge: TurnEdge::Began, at: At(1_700_000_000_000), order: Some(OrderId("o-1".into())) }),
            Message::SeatSaid(SeatSaid { seat: s.clone(), epoch: e, at: At(1_700_000_001_000), text: "folded it".into() }),
            Message::SeatSpent(SeatSpent {
                seat: s.clone(),
                epoch: e,
                tokens: Tokens { input: 100, output: 20, cache_read: 5, cache_write: 2 },
                cost_micros: 1234,
            }),
            Message::Turn(Turn { seat: s.clone(), epoch: e, edge: TurnEdge::Ended, at: At(1_700_000_002_000), order: None }),
            Message::SeatDid(SeatDid {
                seat: s.clone(),
                epoch: e,
                at: At(1_700_000_002_000),
                digest: "read the tree".into(),
                transcript: FileRef { host: HostId(HOST.into()), path: "/t.jsonl".into() },
            }),
            Message::SeatStuck(SeatStuck { seat: s.clone(), epoch: e, command: "wsp wait".into(), waited_secs: 90 }),
            Message::SeatStopped(SeatStopped { seat: s.clone(), epoch: e, exit: Some(0), ended: vec![4242], renderer_left: false }),
        ];
        msgs.into_iter().map(|msg| m.emit(msg).unwrap().0).collect()
    }

    /// Ask until `want` frames were folded or ten seconds passed.
    fn pump(store: &Store, sub: &mut Subscription, want: usize) -> usize {
        let mut n = 0;
        let start = Instant::now();
        while n < want && start.elapsed() < Duration::from_secs(10) {
            if step(store, HOST, sub, Duration::from_millis(500)).unwrap() {
                n += 1;
            }
        }
        n
    }

    fn lines(path: &Path) -> usize {
        std::fs::read_to_string(path).map(|t| t.lines().count()).unwrap_or(0)
    }

    /// wsp-028's done-when: a scripted stream from a loopback manager —
    /// started, turn, said, spent, stopped — is folded into the files, with no
    /// `ps`, render or `df` in the call log for those ticks; and after the
    /// connection drops, the resubscribe resumes from the persisted id and
    /// folds nothing twice.
    #[test]
    fn a_scripted_stream_from_a_loopback_manager_is_folded_into_the_files_once() {
        let env = util::isolated("fold-stream");
        let store = Store::open();
        let exec = env.state().join("exec");
        let m = manager(&exec, 0);
        let addr = m.addrs()[0];

        // The manager's first beat, folded where the sweeps read it. The
        // manager runs in this test's process and reads its facts with its
        // own probes; those are its, so the tick below starts after them.
        let mut sub = Subscription::new(addr, token(&exec), HOST, &store.state, "test");
        let start = Instant::now();
        while !beat_path(&store.state, HOST).exists() {
            assert!(start.elapsed() < Duration::from_secs(10), "no beat with host facts within 10s");
            step(&store, HOST, &mut sub, Duration::from_millis(500)).unwrap();
        }
        assert!(read(&beat_path(&store.state, HOST))["beat"]["host"]["cpus"].as_u64().is_some_and(|n| n > 0));

        // The fold's ticks, as the call log records them.
        wsp_base::calls::tick_begin();
        wsp_base::calls::phase("fold");
        let ids = script(&m);
        assert_eq!(pump(&store, &mut sub, ids.len()), ids.len(), "every event folded");
        wsp_base::calls::tick_end();
        let ticks: Vec<Value> =
            wsp_base::calls::read_all(&store.state).into_iter().filter(|l| l["k"] == "tick" && l["phases"].get("fold").is_some()).collect();
        assert_eq!(ticks.len(), 1, "{ticks:?}");
        for probe in ["ps", "df", "compound-render", "lsof"] {
            assert!(ticks[0]["children"].get(probe).is_none(), "{probe} ran: {}", ticks[0]["children"]);
        }

        // spawned.json, bindings.json and claims.json: the row the spawn named.
        let spawned = store.spawned();
        assert_eq!(spawned["cpd-f1"]["started"]["row"], "wsp-028", "{spawned:?}");
        assert_eq!(spawned["cpd-f1"]["started"]["pid"], 4242);
        assert_eq!(store.bindings()["cpd-f1"]["task_id"], "wsp-028");
        assert_eq!(store.claims()["wsp-028"]["workspace_id"], "cpd-f1");

        // said.json: the word, and the state the stop left.
        let dir = store.state.join(crate::place_compound::SEATS).join("cpd-f1");
        let said = LOCAL.said(&dir);
        assert_eq!(said["said"], "folded it", "{said}");
        assert_eq!(said["state"], "gone", "{said}");
        assert_eq!(read(&store.state.join("seat-since.json"))["cpd-f1"]["state"], "gone");

        // The burn tally, and spent.jsonl once the seat stopped.
        let burn = read(&dir.join(crate::host::BURN_FILE));
        assert_eq!((burn["input"].as_u64(), burn["cost"].as_u64(), burn["turns"].as_u64()), (Some(100), Some(1234), Some(1)), "{burn}");
        let spent = std::fs::read_to_string(store.state.join(crate::host::SPENT_FILE)).unwrap();
        assert_eq!(spent.lines().count(), 1, "{spent}");
        assert!(spent.contains("\"seat\":\"cpd-f1\"") && spent.contains("\"cost\":1234"), "{spent}");

        // What leg 2 reads: the digests, the stuck seat, the ended seat.
        assert_eq!(lines(&did_path(&store.state, HOST)), 1);
        assert_eq!(read(&stuck_path(&store.state, HOST))["cpd-f1"]["command"], "wsp wait");
        assert_eq!(read(&ended_path(&store.state, HOST))["cpd-f1"]["exit"], 0);
        assert_eq!(read_folded(&store.state, HOST), Some(EventId(*ids.last().unwrap())));

        // The manager goes and comes back on the same port and journal; the
        // connection drops, and the next ask resubscribes from the last id.
        m.stop();
        let m = manager(&exec, addr.port());
        let later = m
            .emit(Message::SeatSaid(SeatSaid { seat: seat(), epoch: Epoch(1), at: At(1_700_000_009_000), text: "again".into() }))
            .unwrap();
        assert_eq!(pump(&store, &mut sub, 1), 1);
        assert!(sub.subscribed >= 2, "it resubscribed: {}", sub.subscribed);
        assert_eq!(LOCAL.said(&dir)["said"], "again");
        sub.close();

        // A next daemon whose subscription lost its place (the id it kept is
        // older than what was folded) hears the whole journal again, and the
        // fold skips every one of them.
        let _ = std::fs::remove_file(wsp_wire::host_tcp::last_path(&store.state, HOST));
        let mut again = Subscription::new(addr, token(&exec), HOST, &store.state, "test");
        let start = Instant::now();
        let mut heard = 0;
        while heard < ids.len() + 1 && start.elapsed() < Duration::from_secs(10) {
            if let Some(f) = again.next(Duration::from_millis(500)).unwrap() {
                if f.id.is_some() {
                    heard += 1;
                    assert!(!fold(&store, HOST, &f), "{:?} folded twice", f.id);
                }
            }
        }
        assert_eq!(heard, ids.len() + 1, "the whole journal was replayed");
        assert_eq!(lines(&store.state.join(crate::host::SPENT_FILE)), 1, "the burn was banked once");
        assert_eq!(lines(&did_path(&store.state, HOST)), 1, "the digest was kept once");
        assert_eq!(read_folded(&store.state, HOST), Some(later));
        drop(again);
        m.stop();
    }

    /// A beat's host facts are kept where `sweep:disk` and `sweep:throttle`
    /// read them; a beat without facts changes nothing, and is no event.
    #[test]
    fn a_beats_host_facts_are_kept_for_the_sweeps() {
        let _env = util::isolated("fold-beat");
        let store = Store::open();
        let bare = Frame { id: None, req: None, msg: Message::Beat(Beat { at: At(1), host: None, seats: vec![] }) };
        assert!(!fold(&store, HOST, &bare));
        assert!(!beat_path(&store.state, HOST).exists());
        let facts = HostFacts {
            load_centi: 250,
            cpus: 8,
            memory: MemoryPressure::Warn,
            swap_used_bytes: 0,
            disks: vec![DiskFree { volume: "/".into(), free_bytes: 10, total_bytes: 100 }],
            thermal: Thermal::Fair,
        };
        let beat = Frame { id: None, req: None, msg: Message::Beat(Beat { at: At(2), host: Some(facts), seats: vec![] }) };
        assert!(fold(&store, HOST, &beat));
        let kept = read(&beat_path(&store.state, HOST));
        assert_eq!(kept["beat"]["host"]["memory"], "warn", "{kept}");
        assert_eq!(read_folded(&store.state, HOST), None, "a beat is not an event");
    }
}
