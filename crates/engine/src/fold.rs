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
//! | `seat.started` | `manager/<host>.turns.json` (the seat's entry), `spawned.json` (`started`), `bindings.json` and `claims.json` (the row the spawn's key named, where none is held) |
//! | `seat.refused` | `spawned.json` (`refused`) |
//! | `turn` | the seat's `said.json` (`working`/`idle`), `manager/<host>.turns.json`, `burn.json`'s `turns` |
//! | `seat.said` | the seat's `said.json` (`said`, `said_at`) |
//! | `seat.spent` | the seat's `burn.json` (tokens and cost) |
//! | `seat.stopped` | `spent.jsonl` (the burn banked), `said.json` (`gone`), `manager/<host>.ended.json` |
//! | `seat.did` | `manager/<host>.did.jsonl`, one line per digest |
//! | `seat.stuck` | `manager/<host>.stuck.json`, the newest per seat |
//! | `order.*` | `manager/<host>.orders.json`, the newest per order (`order.acked` also `turns.json`) |
//! | `beat` (with `HostFacts`) | `manager/<host>.beat.json` |
//!
//! **The orders machine's second reporter** (wsp-123). Under
//! `orders::mode() == store` the fold applies the manager's answers to the
//! orders machine beside the delivery worker (`crate::deliver`), under the
//! worker's keys, so an answer both hear applies once, and one heard after a
//! restart with no worker waiting on it still lands: `order.delivered` is
//! `order.delivered` (`order:<id>:delivered`), `order.acked` is
//! `order.acked { turn_began }` (`order:<id>:acked`), and `order.refused` is
//! `order.refused { why, counts }`, `counts` false for compound-sup's `busy`,
//! applied only while the order is `typed` (under the `deliver:<seat>` lease's
//! token where one is held, the worker's own key). A `turn` that began on an
//! order names it (wsp-032, kept as the seat's `order` in `turns.json`), and
//! when that turn ends with the order `acknowledged` the fold applies
//! `order.done` (`order:<id>:done`). An order the store does not hold (sent
//! before the switch, or by a tell on the files path) goes to the files alone
//! and is not mirrored, which the server refuses under `Owned`. Under `files`
//! each `order.*` is mirrored as an `Order`, as before. `orders.json` is
//! written in both modes.
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use wsp_host_api::{EventId, Frame, Message, TurnEdge};
use wsp_server::SharedStore;
use wsp_store_contract::{
    self as c, At as CAt, Command, Entity, IdempotencyKey, MachineName, OrderState, Principal, RowId, SeatId, SeatState, Transport,
};

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
/// Where `said.json` says a seat's state came from.
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

/// seat -> `{ began }`, the epoch seconds of the newest turn the manager says
/// began at it (a `turn` edge, or the turn an `order.acked` names): what the
/// ending and the untaken-order looks read in place of a transcript or a
/// screen. A seat's entry is made when its start is folded, at `0`, so a seat
/// with an entry and no turn since is a fact (`false`) and a seat with none is
/// not (`None`).
pub fn turns_path(state: &Path, host: &str) -> PathBuf {
    beside(state, host, "turns.json")
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

/// `turns.json` with the seat's newest turn start, at `began_ms` (epoch ms) if
/// that is newer than the one held; `None` makes the seat's entry and no more.
/// The entry's other keys (its turn's `order`) stand.
fn turned(state: &Path, host: &str, seat: &str, began_ms: Option<u64>) {
    let path = turns_path(state, host);
    let entry = read(&path)[seat].clone();
    let held = entry["began"].as_i64();
    let at = began_ms.map(|ms| (ms / 1000) as i64);
    let next = match (held, at) {
        (Some(h), Some(a)) => h.max(a),
        (Some(h), None) => h,
        (None, a) => a.unwrap_or(0),
    };
    if held != Some(next) {
        let mut m = match entry {
            Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        m.insert("began".into(), json!(next));
        put(&path, seat, Value::Object(m));
    }
}

/// `turns.json`'s `order` for the seat: the order whose ack began its newest
/// turn (wsp-032), or none when that turn began on no order.
fn turn_order(state: &Path, host: &str, seat: &str, order: Option<&str>) {
    let path = turns_path(state, host);
    let entry = read(&path)[seat].clone();
    if entry["order"].as_str() == order {
        return;
    }
    let mut m = match entry {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    match order {
        Some(o) => m.insert("order".into(), json!(o)),
        None => m.remove("order"),
    };
    put(&path, seat, Value::Object(m));
}

/// Since when each of `seats` has been in its state, RFC 3339, as the fold
/// dated it: the turn edge's time while it turns or idles, the stop's once it
/// is gone, in the seat's own `said.json`. A seat the manager said nothing of
/// has none.
pub fn state_since<'a>(store: &Store, seats: impl IntoIterator<Item = &'a str>) -> BTreeMap<String, String> {
    let compound = Compound::at(store.state.join(crate::place_compound::SEATS));
    seats
        .into_iter()
        .filter_map(|seat| {
            let said = LOCAL.said(&compound.dir_of(&Seat::new(seat)).ok()?);
            Some((seat.to_string(), said.get("at")?.as_str()?.to_string()))
        })
        .collect()
}

/// Whether a turn began at `seat` at or after `since` (epoch seconds), as the
/// manager's events folded say: `Some(false)` for a seat whose start was
/// folded and began no turn since, `None` for one nothing was heard of. A
/// caller holds on `None`.
pub fn turn_began_since(store: &Store, seat: &str, since: i64) -> Option<bool> {
    let dir = store.state.join("manager");
    let mut said = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().ends_with(".turns.json") {
            continue;
        }
        if let Some(began) = read(&e.path())[seat]["began"].as_i64() {
            said = Some(said.unwrap_or(false) || began >= since);
        }
    }
    said
}

/// Fold one frame heard from `host`'s manager. `true` when it was written,
/// `false` when it was skipped: an event already folded, or a frame that
/// carries nothing to keep.
pub fn fold(store: &Store, host: &str, frame: &Frame) -> bool {
    fold_into(store, None, host, frame)
}

/// [`fold`], and with a `db` the event's one `Command::Mirror` after the files
/// are written and before `.folded` moves (wsp-075). Built from the event and
/// from what `apply` just wrote, never from a probe.
pub fn fold_into(store: &Store, db: Option<&SharedStore>, host: &str, frame: &Frame) -> bool {
    fold_as(store, db, host, frame, crate::orders::mode())
}

/// [`fold_into`] under the orders mode `orders`: under `Store` an `order.*`
/// is applied to the orders machine rather than mirrored, and a turn's end
/// after its order's ack is `order.done` (wsp-123).
fn fold_as(store: &Store, db: Option<&SharedStore>, host: &str, frame: &Frame, orders: crate::orders::Mode) -> bool {
    if let Message::Beat(b) = &frame.msg {
        if b.host.is_none() {
            return false;
        }
        write(&beat_path(&store.state, host), &json!({ "heard": util::epoch_secs(), "beat": b }));
        if let Some(db) = db {
            let cmd = mirror_beat(host, b);
            mirror_apply(store, db, host, "beat", cmd);
        }
        return true;
    }
    let Some(id) = frame.id else { return false };
    if read_folded(&store.state, host).is_some_and(|f| id <= f) {
        return false;
    }
    apply(store, host, &frame.msg);
    if let Some(db) = db {
        let owned = orders == crate::orders::Mode::Store;
        let is_order = matches!(frame.msg, Message::OrderDelivered(_) | Message::OrderAcked(_) | Message::OrderRefused(_));
        if owned && is_order {
            answer(store, db, host, &frame.msg);
        } else if let Some(cmd) = mirror_of(store, db, host, &frame.msg) {
            mirror_apply(store, db, host, &id.0.to_string(), cmd);
        }
        if let (true, Message::Turn(t)) = (owned, &frame.msg) {
            if t.edge == TurnEdge::Ended {
                done(store, db, host, &t.seat.0);
            }
        }
    }
    let path = folded_path(&store.state, host);
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = write_atomic(&path, &format!("{}\n", id.0));
    true
}

fn mirror_apply(store: &Store, db: &SharedStore, host: &str, id: &str, cmd: Command) {
    // An event's key is its id, so a replay after a restart is the same
    // command; a beat's is its own moment, so each beat (every 10 s) writes one
    // machine.beat (wsp-304), its `beat` time having moved (a mirror.put where
    // more than its beat and liveness facts moved).
    let key = match id {
        "beat" => format!(
            "fold:{host}:beat:{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ),
        id => format!("fold:{host}:{id}"),
    };
    if let Err(e) = db.apply(cmd, IdempotencyKey(key), Principal::Manager(MachineName(host.to_string()))) {
        wsp_store::store::log_line(store, &format!("mirror: fold of {host} {id} not applied: {e:?}"));
    }
}

/// One of the orders machine's commands, applied as the manager `host`.
fn machine_apply(store: &Store, db: &SharedStore, host: &str, cmd: Command, key: String) {
    if let Err(e) = db.apply(cmd, IdempotencyKey(key.clone()), Principal::Manager(MachineName(host.to_string()))) {
        wsp_store::store::log_line(store, &format!("orders: fold of {host} {key} not applied: {e:?}"));
    }
}

/// The manager's answer to an order, applied to the orders machine under the
/// delivery worker's keys. Nothing for an order the store does not hold: it
/// went to the files alone.
fn answer(store: &Store, db: &SharedStore, host: &str, msg: &Message) {
    let order = match msg {
        Message::OrderDelivered(o) => &o.order,
        Message::OrderAcked(o) => &o.order,
        Message::OrderRefused(o) => &o.order,
        _ => return,
    };
    let id = c::OrderId(order.0.clone());
    let Ok((state, _)) = db.snapshot() else { return };
    let Some(held) = state.orders.get(&id) else { return };
    let (cmd, key) = match msg {
        Message::OrderDelivered(_) => (Command::OrderDelivered { order: id.clone() }, format!("order:{}:delivered", id.0)),
        Message::OrderAcked(o) => {
            (Command::OrderAcked { order: id.clone(), turn_began: CAt(o.turn_began.0) }, format!("order:{}:acked", id.0))
        }
        Message::OrderRefused(o) => {
            // Answered already (by the worker, or by this fold before a
            // restart): the order is queued again, or failed.
            if held.state != OrderState::Typed {
                return;
            }
            let why = serde_json::to_value(o.why).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            let counts = o.why != wsp_host_api::OrderFail::Busy;
            // The worker keys a refusal by its lease's token; while it holds
            // the lease this is its key, else the attempt it ends.
            let tail = match state.leases.get(&c::LeaseName::deliver(&held.to)) {
                Some(l) => l.token.0.to_string(),
                None => format!("a{}", held.attempts),
            };
            (Command::OrderRefused { order: id.clone(), why, counts }, format!("order:{}:refused:{tail}", id.0))
        }
        _ => return,
    };
    machine_apply(store, db, host, cmd, key);
}

/// A turn ended at `seat`: the order whose ack began that turn, if the store
/// holds it `acknowledged` at this seat, is `done`.
fn done(store: &Store, db: &SharedStore, host: &str, seat: &str) {
    let Some(order) = read(&turns_path(&store.state, host))[seat]["order"].as_str().map(str::to_string) else { return };
    let id = c::OrderId(order);
    let Ok((state, _)) = db.snapshot() else { return };
    let Some(held) = state.orders.get(&id) else { return };
    if held.state != OrderState::Acknowledged || held.to.0 != seat {
        return;
    }
    let key = format!("order:{}:done", id.0);
    machine_apply(store, db, host, Command::OrderDone { order: id }, key);
}

fn scalars(m: &serde_json::Map<String, Value>, keys: &[&str], into: &mut BTreeMap<String, String>) {
    for k in keys {
        match m.get(*k) {
            Some(Value::String(s)) if !s.is_empty() => {
                into.insert((*k).into(), s.clone());
            }
            Some(v @ (Value::Number(_) | Value::Bool(_))) => {
                into.insert((*k).into(), v.to_string());
            }
            _ => {}
        }
    }
}

/// The seat as the files now say it, over what the database held of it.
fn seat_entity(store: &Store, db: &SharedStore, host: &str, seat: &str, state: SeatState) -> c::Seat {
    // One seat, read alone: no snapshot of the whole state (wsp-757).
    let held = db.seat(&SeatId(seat.to_string())).ok().flatten();
    // The state word is the event's.
    let census = match state {
        SeatState::Working => crate::place::State::Working,
        SeatState::Idle => crate::place::State::Idle,
        SeatState::Gone | SeatState::Ending | SeatState::Lost => crate::place::State::Gone,
        _ => crate::place::State::Starting,
    };
    seat_of(store, held.as_ref(), host, seat, state, census)
}

/// Every seat this machine's backend's census names, each as [`seat_entity`]
/// builds it, at the census's own word and with nothing held before it: the
/// seats `mirror::state_of` hands `wsp-view` (wsp-308). The census reads the
/// store's own seat records, as `stage::census`'s backend does.
pub fn seats_of(store: &Store, host: &str) -> Vec<c::Seat> {
    use crate::place::{Place as _, State};
    let place = Compound::at(store.state.join(crate::place_compound::SEATS));
    let Ok(census) = place.census() else { return Vec::new() };
    census
        .seats()
        .map(|s| {
            let state = match s.state {
                State::Working => SeatState::Working,
                State::Idle | State::Blocked => SeatState::Idle,
                State::Starting => SeatState::Started,
                State::Gone | State::Empty => SeatState::Gone,
                State::Unreachable | State::Unknown => SeatState::Lost,
            };
            seat_of(store, None, host, s.seat.as_str(), state, s.state)
        })
        .collect()
}

/// [`seat_entity`]'s build, over what was `held` of the seat and at the given
/// census word.
fn seat_of(store: &Store, held: Option<&c::Seat>, host: &str, seat: &str, state: SeatState, census: crate::place::State) -> c::Seat {
    let rec = match store.spawned().remove(seat) {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let mut fields = held.as_ref().map(|h| h.fields.clone()).unwrap_or_default();
    if let Some(Value::Object(st)) = rec.get("started") {
        scalars(st, &["host", "epoch"], &mut fields);
    }
    if let Some(Value::Object(b)) = store.bindings().get(seat) {
        scalars(b, &["task_id", "pane_id", "host", "epoch"], &mut fields);
    }
    // The records whole, and the census row as the files path builds it
    // (wsp-076). No event and no `seats` reply carries the label, cwd, agent
    // or session: the census reads them off the seat's own record and
    // `said.json` (`Compound::seated_as`), and so does this, by the same fn.
    let seated = Compound::at(store.state.join(crate::place_compound::SEATS)).seated_as(&Seat::new(seat), census);
    let spawned = (!rec.is_empty()).then(|| Value::Object(rec.clone()));
    let governs = crate::mirror::governs_of(&store.governors(), seat);
    crate::mirror::seat_records(&mut fields, store.bindings().get(seat), spawned.as_ref(), &seated, governs.as_deref());
    let started = rec.get("started").and_then(Value::as_object);
    let row = started
        .and_then(|s| s.get("row"))
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
        .map(|r| RowId(r.to_string()))
        .or_else(|| held.as_ref().and_then(|h| h.row.clone()));
    let attempt = started.and_then(|s| s.get("attempt")).and_then(Value::as_u64).map(|n| n as u32);
    let text = |k: &str| rec.get(k).and_then(Value::as_str).map(str::to_string);
    c::Seat {
        id: SeatId(seat.to_string()),
        machine: MachineName(host.to_string()),
        kind: text("kind").or_else(|| held.as_ref().map(|h| h.kind.clone())).unwrap_or_else(|| "claude".into()),
        model: text("model").or_else(|| held.as_ref().and_then(|h| h.model.clone())),
        state,
        before_lost: None,
        row,
        attempt: attempt.or_else(|| held.as_ref().map(|h| h.attempt)).unwrap_or(0),
        pid: started.and_then(|s| s.get("pid")).and_then(Value::as_u64).map(|p| p as u32).or_else(|| held.as_ref().and_then(|h| h.pid)),
        fields,
    }
}

/// The daemon's sweep of seats that are gone with nothing to say (wsp-094):
/// each tick, the state core's seats that `wip` would not draw
/// (`cmd_agent::mute_seats`: gone and holding nothing, and either
/// ended by a despawn, with no sentence and no agent on record, or gone past
/// `GONE_FOR`) are mirrored gone, and their `spawned.json`
/// entry dropped. What a machine restart leaves — every
/// seat that ran, ended by the fold and named for good — clears on the first
/// tick; one that said something, a day after it was first seen gone.
/// Returns the seats forgotten. `pick` is that reading, passed in by the
/// daemon: it is `wip`'s, a layer above this one.
pub fn sweep_mute(store: &Store, db: &SharedStore, pick: impl FnOnce(&[crate::place::Seated]) -> Vec<String>) -> Vec<String> {
    let Ok((state, _)) = db.snapshot() else { return Vec::new() };
    let seats: Vec<crate::place::Seated> = state.seats.values().map(crate::mirror::seated_of).collect();
    let mute = pick(&seats);
    if mute.is_empty() {
        return mute;
    }
    let gone: Vec<c::Subject> = mute.iter().map(|s| c::Subject::Seat(SeatId(s.clone()))).collect();
    let key = format!("sweep:mute:{}:{}", util::epoch_secs(), mute.join(","));
    let cmd = Command::Mirror { put: Vec::new(), gone };
    if let Err(e) = db.apply(cmd, IdempotencyKey(key), Principal::Wsp("daemon".into())) {
        wsp_store::store::log_line(store, &format!("sweep: {} gone seats not forgotten in the state core: {e:?}", mute.len()));
        return Vec::new();
    }
    store.forget_seats(&mute);
    let shown: Vec<&str> = mute.iter().take(8).map(String::as_str).collect();
    let more = if mute.len() > shown.len() { format!(" and {} more", mute.len() - shown.len()) } else { String::new() };
    wsp_store::store::log_line(
        store,
        &format!("sweep: forgot {} gone seat(s) with nothing to say: {}{more}", mute.len(), shown.join(" ")),
    );
    mute
}

fn put_cmd(put: Vec<Entity>) -> Command {
    Command::Mirror { put, gone: Vec::new() }
}

fn mirror_beat(host: &str, b: &wsp_host_api::Beat) -> Command {
    let mut f = BTreeMap::new();
    if let Some(Value::Object(m)) = b.host.as_ref().and_then(|h| serde_json::to_value(h).ok()) {
        for (k, v) in m {
            f.insert(
                k,
                match v {
                    Value::String(s) => s,
                    v => v.to_string(),
                },
            );
        }
    }
    put_cmd(vec![Entity::Machine(c::Machine {
        name: MachineName(host.to_string()),
        transport: Transport::Local,
        beat: Some(CAt(b.at.0)),
        facts: f,
    })])
}

fn mirror_of(store: &Store, db: &SharedStore, host: &str, msg: &Message) -> Option<Command> {
    Some(match msg {
        Message::SeatStarted(s) => {
            let seat = s.seat.0.as_str();
            let mut put = vec![Entity::Seat(seat_entity(store, db, host, seat, SeatState::Started))];
            let row = s.key.row.0.as_str();
            if !row.is_empty() && row != seat {
                // The claim as the file holds it, projected as the rows half does.
                let mut one = BTreeMap::new();
                if let Some(v) = store.claims().get(row) {
                    one.insert(row.to_string(), v.clone());
                }
                put.extend(crate::mirror::claims(&one).into_iter().filter(|e| matches!(e, Entity::Claim(c) if c.seat.0 == seat)));
            }
            put_cmd(put)
        }
        Message::SeatStopped(s) => {
            put_cmd(vec![Entity::Seat(seat_entity(store, db, host, &s.seat.0, SeatState::Gone))])
        }
        Message::Turn(t) => {
            let state = match t.edge {
                TurnEdge::Began => SeatState::Working,
                TurnEdge::Ended => SeatState::Idle,
            };
            put_cmd(vec![Entity::Seat(seat_entity(store, db, host, &t.seat.0, state))])
        }
        Message::OrderDelivered(o) => put_cmd(vec![order_entity(db, host, &o.order.0, &o.seat.0, OrderState::Delivered)]),
        Message::OrderAcked(o) => put_cmd(vec![order_entity(db, host, &o.order.0, &o.seat.0, OrderState::Acknowledged)]),
        Message::OrderRefused(o) => put_cmd(vec![order_entity(db, host, &o.order.0, &o.seat.0, OrderState::Failed)]),
        _ => return None,
    })
}

fn order_entity(db: &SharedStore, host: &str, order: &str, seat: &str, state: OrderState) -> Entity {
    let held = db.snapshot().ok().and_then(|(s, _)| s.orders.get(&c::OrderId(order.to_string())).cloned());
    Entity::Order(c::Order {
        // A held order keeps its workspace; one the store has not seen reads private.
        workspace: held.as_ref().map(|h| h.workspace.clone()).unwrap_or_default(),
        id: c::OrderId(order.to_string()),
        from: held.as_ref().map(|h| h.from.clone()).unwrap_or(Principal::Manager(MachineName(host.to_string()))),
        to: SeatId(seat.to_string()),
        text: held.as_ref().map(|h| h.text.clone()).unwrap_or_default(),
        state,
        attempts: held.as_ref().map(|h| h.attempts).unwrap_or(0),
        first: held.as_ref().is_some_and(|h| h.first),
        why: held.and_then(|h| h.why),
    })
}

fn apply(store: &Store, host: &str, msg: &Message) {
    let state = &store.state;
    match msg {
        Message::SeatStarted(s) => {
            let seat = s.seat.0.as_str();
            // No row: a seat the manager adopted from the in-process host
            // goes out with an empty one, and a spawn without a key is keyed
            // by its seat's own id. Neither names a row of the store's.
            let row = Some(s.key.row.0.as_str()).filter(|r| !r.is_empty() && *r != seat);
            let mut rec = match store.spawned().remove(seat) {
                Some(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            rec.remove("refused");
            // A new start of the id: a despawn's end was the last one's (wsp-094).
            rec.remove("ended");
            // Without a row, the row and attempt the record already has stand.
            let prior = rec.get("started").filter(|_| row.is_none());
            let (row_was, attempt) = match prior {
                Some(p) => (p.get("row").cloned().unwrap_or(json!("")), p.get("attempt").cloned().unwrap_or(json!(0))),
                None => (json!(row.unwrap_or("")), json!(s.key.attempt)),
            };
            rec.insert(
                "started".into(),
                json!({ "host": host, "row": row_was, "attempt": attempt, "epoch": s.epoch.0, "pid": s.pid, "sockets": s.sockets }),
            );
            store.set_spawned(seat, Some(&Value::Object(rec)));
            // The row as the spawn named it, where nothing holds the seat or
            // the row already: a spawn's own records are richer and stand.
            if let Some(row) = row {
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
                    // **Never for a leg's or a piece's seat** (`wsp-334`): a part
                    // claims nothing. The claim this wrote for each of wsp-030's
                    // legs kept the next leg from opening (a row someone holds
                    // whole opens no leg). It also made the run read the leg's seat
                    // as the member's, so the seat was told to finish, the
                    // person was flagged, and each boundary cost two ticks.
                    let part = store.find_task(row).is_some_and(|t| crate::leg_seat::a_parts_seat(store, &t, seat));
                    if !part && !store.claims().contains_key(row) {
                        let mut claim = json!({ "workspace_id": seat, "host": host, "epoch": s.epoch.0, "claimed_at": util::now_iso() });
                        wsp_store::store::stamp_claim_expiry(&mut claim);
                        store.set_claim(row, claim);
                    }
                });
            }
            turned(state, host, seat, None);
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
            turned(state, host, seat, (t.edge == TurnEdge::Began).then_some(t.at.0));
            if t.edge == TurnEdge::Began {
                turn_order(state, host, seat, t.order.as_ref().map(|o| o.0.as_str()));
            }
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
        Message::OrderAcked(o) => {
            put(&orders_path(state, host), &o.order.0, json!({ "seat": o.seat.0, "acked": o.turn_began.0 }));
            turned(state, host, &o.seat.0, Some(o.turn_began.0));
        }
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

/// The host whose manager is folded: the one `$WSP_HOST` chooses, the
/// in-process manager or the machine record's; `None` when there is none.
fn folded_host(store: &Store) -> Option<String> {
    crate::host_tcp::record(&store.state).ok().map(|(local, _)| local.host)
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
pub fn did_since(store: &Store, seat: &str, since: i64) -> Option<crate::did::Did> {
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
///
/// The report only: what shape the command is, and so the `Stall`, is the
/// seats' to say ([`crate::cmd_agent::stuck_of_seat`]), not the fold's.
pub fn stall(store: &Store, seat: &str, now: i64, recorded: Option<(&str, i64)>) -> Option<Reported> {
    let host = folded_host(store)?;
    let s = &read(&stuck_path(&store.state, &host))[seat];
    let (Some(command), Some(waited), Some(heard)) = (s["command"].as_str(), s["waited_secs"].as_i64(), s["heard"].as_i64()) else {
        return None;
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
    let secs = if open { (now - at).max(waited) } else { waited };
    Some(Reported { command: command.to_string(), at, secs, open })
}

/// A seat's stall as the manager reported it and the fold kept it ([`stall`]).
pub struct Reported {
    /// The command, as the report gave it.
    pub command: String,
    /// When the call started, epoch seconds.
    pub at: i64,
    /// Seconds it held: to now while open, else what the report said it waited.
    pub secs: i64,
    /// No turn edge folded since the report.
    pub open: bool,
}

// ---------------------------------------------------------------- the thread

/// One ask of the subscription, and its frame folded. `Ok(true)` when a frame
/// was written.
pub fn step(store: &Store, db: Option<&SharedStore>, host: &str, sub: &mut Subscription, wait: Duration) -> Result<bool, HostError> {
    Ok(sub.next(wait)?.is_some_and(|f| fold_into(store, db, host, &f)))
}

/// The thread's loop: ask, fold, again, until `stop`. A manager that cannot be
/// reached is asked again after a wait that doubles to [`RETRY_MAX`].
pub fn follow(store: &Store, db: Option<&SharedStore>, host: &str, sub: &mut Subscription, stop: &AtomicBool) {
    let mut retry = Duration::from_secs(1);
    while !stop.load(Ordering::SeqCst) {
        match step(store, db, host, sub, ASK) {
            Ok(_) => retry = Duration::from_secs(1),
            Err(_) => {
                std::thread::sleep(retry);
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
    }
    sub.close();
}

/// Start the thread for the manager `$WSP_HOST` chooses: under `inproc` the
/// one run in this process (started here if no wsp process runs it yet),
/// under `manager` the one the local machine record names. `None` with
/// neither.
pub fn start(store: &Store, db: Option<SharedStore>) -> Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)> {
    let (local, token) = crate::host_tcp::record(&store.state).ok()?;
    let (root, state) = (store.root.clone(), store.state.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::Builder::new()
        .name(format!("fold:{}", local.host))
        .spawn(move || {
            let store = Store::at(root, state.clone());
            let mut sub = Subscription::new(local.socket(), token, &local.host, &state, env!("CARGO_PKG_VERSION"));
            follow(&store, db.as_ref(), &local.host, &mut sub, &flag);
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

    /// The upper bound on every wait here for a frame to come and be folded:
    /// generous, so a machine under load is slow and not failed. Each wait
    /// ends as soon as what it waits for is seen (as wsp-114's do).
    const WAIT: Duration = Duration::from_secs(120);

    /// Ask until the fold has reached event `id`, and fail if `WAIT` runs out.
    /// What it waits on is the folded id, not a count of frames: a manager
    /// that comes back beats first, and under load that beat can be the frame
    /// a count would stop at, before the event the test emitted.
    fn fold_to(store: &Store, sub: &mut Subscription, id: u64) {
        let deadline = Instant::now() + WAIT;
        while read_folded(&store.state, HOST).is_none_or(|EventId(n)| n < id) {
            assert!(Instant::now() < deadline, "event {id} not folded within {WAIT:?}: at {:?}", read_folded(&store.state, HOST));
            step(store, None, HOST, sub, Duration::from_millis(500)).unwrap();
        }
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
        let deadline = Instant::now() + WAIT;
        while !beat_path(&store.state, HOST).exists() {
            assert!(Instant::now() < deadline, "no beat with host facts within {WAIT:?}");
            step(&store, None, HOST, &mut sub, Duration::from_millis(500)).unwrap();
        }
        assert!(read(&beat_path(&store.state, HOST))["beat"]["host"]["cpus"].as_u64().is_some_and(|n| n > 0));

        // The fold's ticks, as the call log records them.
        wsp_base::calls::tick_begin();
        wsp_base::calls::phase("fold");
        let ids = script(&m);
        fold_to(&store, &mut sub, *ids.last().unwrap());
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
        assert!(state_since(&store, ["cpd-f1"]).contains_key("cpd-f1"), "and dated the gone");

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
        fold_to(&store, &mut sub, later.0);
        assert!(sub.subscribed >= 2, "it resubscribed: {}", sub.subscribed);
        assert_eq!(LOCAL.said(&dir)["said"], "again");
        sub.close();

        // A next daemon whose subscription lost its place (the id it kept is
        // older than what was folded) hears the whole journal again, and the
        // fold skips every one of them.
        let _ = std::fs::remove_file(wsp_wire::host_tcp::last_path(&store.state, HOST));
        let mut again = Subscription::new(addr, token(&exec), HOST, &store.state, "test");
        let deadline = Instant::now() + WAIT;
        let mut heard = 0;
        while heard < ids.len() + 1 && Instant::now() < deadline {
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

    /// wsp-029: a seat the manager adopts from the in-process host goes out
    /// with an empty row, and a spawn without a key is keyed by its own seat:
    /// folding either writes no claim and no binding for a row named after
    /// the seat, and the seat's binding and its `spawned` row stand.
    #[test]
    fn an_adopted_seats_start_claims_no_row_and_leaves_its_records() {
        let _env = util::isolated("fold-adopted");
        let store = Store::open();
        let bound = json!({ "task_id": "wsp-100", "pane_id": "cpd-a1", "host": HOST, "epoch": 1, "started_at": "2026-11-17T00:00:00Z" });
        store.set_binding("cpd-a1", bound.clone());
        store.set_spawned("cpd-a1", Some(&json!({ "started": { "host": HOST, "row": "wsp-100", "attempt": 2, "epoch": 1, "pid": 7, "sockets": [] } })));
        let started = |id: u64, seat: &str, row: &str| Frame {
            id: Some(EventId(id)),
            req: None,
            msg: Message::SeatStarted(SeatStarted {
                key: Key { row: RowId(row.into()), attempt: 0 },
                seat: SeatId(seat.into()),
                epoch: Epoch(2),
                pid: 4343,
                sockets: vec![format!("/run/{seat}.sock")],
            }),
        };
        // Adopted (an empty row), for a seat the store knows and one it does
        // not; and a keyless spawn's start, its row its own seat.
        assert!(fold(&store, HOST, &started(1, "cpd-a1", "")));
        assert!(fold(&store, HOST, &started(2, "cpd-a2", "")));
        assert!(fold(&store, HOST, &started(3, "cpd-a3", "cpd-a3")));

        let claims = store.claims();
        for row in ["", "cpd-a1", "cpd-a2", "cpd-a3"] {
            assert!(!claims.contains_key(row), "a claim on {row:?}: {claims:?}");
        }
        let bindings = store.bindings();
        assert_eq!(bindings.get("cpd-a1"), Some(&bound), "the seat's binding stands");
        assert!(!bindings.contains_key("cpd-a2") && !bindings.contains_key("cpd-a3"), "{bindings:?}");

        let spawned = store.spawned();
        let s = &spawned["cpd-a1"]["started"];
        assert_eq!((&s["row"], &s["attempt"]), (&json!("wsp-100"), &json!(2)), "its spawned row stands: {s}");
        assert_eq!((&s["epoch"], &s["pid"]), (&json!(2), &json!(4343)), "the new start is recorded: {s}");
        assert_eq!(spawned["cpd-a2"]["started"]["row"], "", "{spawned:?}");
        assert_eq!(spawned["cpd-a3"]["started"]["row"], "", "{spawned:?}");
    }

    /// `wsp-334`: a leg's seat starting claims nothing, found by the entry the
    /// spawn seated or by the binding it wrote with `leg`. A member's seat on
    /// another row is still claimed as before.
    #[test]
    fn a_legs_seat_start_claims_no_row() {
        let _env = util::isolated("fold-leg");
        let store = Store::open();
        let mut t = crate::model::Task::new("legged", "wsp-200");
        t.status_raw = "doing".into();
        let mut leg = crate::legs::Leg::pending("build", "do it");
        leg.state = crate::legs::State::Running;
        leg.pane = Some("cpd-l1".into());
        crate::legs::write(&mut t, &[leg]);
        store.save_task(&t).unwrap();
        let mut plain = crate::model::Task::new("plain", "wsp-201");
        plain.status_raw = "doing".into();
        store.save_task(&plain).unwrap();
        store.set_binding("cpd-l2", json!({ "task_id": "wsp-202", "pane_id": "cpd-l2", "leg": "verbs" }));
        let mut bound = crate::model::Task::new("bound", "wsp-202");
        bound.status_raw = "doing".into();
        store.save_task(&bound).unwrap();
        let started = |id: u64, seat: &str, row: &str| Frame {
            id: Some(EventId(id)),
            req: None,
            msg: Message::SeatStarted(SeatStarted { key: Key { row: RowId(row.into()), attempt: 1 }, seat: SeatId(seat.into()), epoch: Epoch(1), pid: 1, sockets: vec![] }),
        };
        assert!(fold(&store, HOST, &started(1, "cpd-l1", "wsp-200")));
        assert!(fold(&store, HOST, &started(2, "cpd-l2", "wsp-202")));
        assert!(fold(&store, HOST, &started(3, "cpd-m1", "wsp-201")));
        let claims = store.claims();
        assert!(!claims.contains_key("wsp-200") && !claims.contains_key("wsp-202"), "{claims:?}");
        assert_eq!(claims["wsp-201"]["workspace_id"], "cpd-m1", "a member's seat is claimed as before");
    }

    /// `wsp-477`: the claim a `seat.started` writes for a row holding none
    /// carries an expiry like `wsp claim`'s: 7 days after `claimed_at` and
    /// `ttl_s` 604800 by default, neither under `WSP_CLAIM_TTL=off`; a row that
    /// already holds a claim keeps its record.
    #[test]
    fn a_fresh_claim_from_a_seat_start_carries_an_expiry() {
        let _env = util::isolated("fold-claim-expiry");
        let store = Store::open();
        for row in ["wsp-300", "wsp-301", "wsp-302"] {
            let mut t = crate::model::Task::new(row, row);
            t.status_raw = "doing".into();
            store.save_task(&t).unwrap();
        }
        let held = json!({ "workspace": "someone", "claimed_at": "2026-11-01T00:00:00Z" });
        store.set_claim("wsp-302", held.clone());
        let started = |id: u64, seat: &str, row: &str| Frame {
            id: Some(EventId(id)),
            req: None,
            msg: Message::SeatStarted(SeatStarted { key: Key { row: RowId(row.into()), attempt: 1 }, seat: SeatId(seat.into()), epoch: Epoch(1), pid: 1, sockets: vec![] }),
        };
        std::env::remove_var("WSP_CLAIM_TTL");
        assert!(fold(&store, HOST, &started(1, "cpd-e1", "wsp-300")));
        std::env::set_var("WSP_CLAIM_TTL", "off");
        assert!(fold(&store, HOST, &started(2, "cpd-e2", "wsp-301")));
        assert!(fold(&store, HOST, &started(3, "cpd-e3", "wsp-302")));
        std::env::remove_var("WSP_CLAIM_TTL");
        let claims = store.claims();
        let c = &claims["wsp-300"];
        assert_eq!(c["workspace_id"], "cpd-e1", "{c}");
        assert_eq!(c["ttl_s"], 604_800, "{c}");
        let at = util::epoch_of(c["claimed_at"].as_str().unwrap());
        assert_eq!(util::epoch_of(c["expires"].as_str().unwrap()), at + 7 * 86_400, "{c}");
        let off = &claims["wsp-301"];
        assert_eq!(off["workspace_id"], "cpd-e2", "{off}");
        assert!(off.get("expires").is_none() && off.get("ttl_s").is_none(), "{off}");
        assert_eq!(claims["wsp-302"], held, "a held claim is kept as it was");
    }

    /// A seat's start makes its entry, a `turn` edge and an `order.acked` date
    /// the newest turn begun, and [`turn_began_since`] answers from them: `None`
    /// for a seat nothing was heard of, `Some(false)` for one that began none
    /// since, `Some(true)` once one did.
    #[test]
    fn the_folded_turns_say_whether_a_turn_began_since() {
        let env = util::isolated("fold-turns");
        let store = Store::open();
        let s = seat();
        assert_eq!(turn_began_since(&store, "cpd-1", 0), None, "nothing heard");
        let started = SeatStarted { key: Key { row: RowId("r".into()), attempt: 0 }, seat: s.clone(), epoch: Epoch(1), pid: 1, sockets: vec![] };
        apply(&store, "h", &Message::SeatStarted(started));
        assert_eq!(turn_began_since(&store, s.0.as_str(), 100), Some(false), "started, no turn");
        let turn = |at: u64| Message::Turn(Turn { seat: s.clone(), epoch: Epoch(1), edge: TurnEdge::Began, at: At(at), order: None });
        apply(&store, "h", &turn(200_000));
        assert_eq!(turn_began_since(&store, s.0.as_str(), 200), Some(true));
        assert_eq!(turn_began_since(&store, s.0.as_str(), 201), Some(false), "an older turn is not since");
        let acked = wsp_host_api::OrderAcked { order: OrderId("o".into()), seat: s.clone(), turn_began: At(300_000) };
        apply(&store, "h", &Message::OrderAcked(acked));
        assert_eq!(turn_began_since(&store, s.0.as_str(), 300), Some(true), "the acked turn counts");
        drop(env);
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
            load5_centi: None,
            load15_centi: None,
            speed_limit_pct: None,
            hid_idle_secs: None,
            free_memory_bytes: None,
            pageouts: None,
        };
        let beat = Frame { id: None, req: None, msg: Message::Beat(Beat { at: At(2), host: Some(facts), seats: vec![] }) };
        assert!(fold(&store, HOST, &beat));
        let kept = read(&beat_path(&store.state, HOST));
        assert_eq!(kept["beat"]["host"]["memory"], "warn", "{kept}");
        assert_eq!(read_folded(&store.state, HOST), None, "a beat is not an event");
    }

    /// wsp-075's done-when: a manager's `seat.started` and `seat.stopped`,
    /// through the loopback manager and the fold running against a database a
    /// `wsp_server::Server` serves, reach a subscriber as `mirror.put` for the
    /// seat once each; and a daemon restart (the fold stopped and run again on
    /// the same database, a new subscription, even with the `.folded` id and
    /// the subscription's place lost) puts neither again.
    #[test]
    fn seat_events_reach_a_subscriber_once_each_across_a_daemon_restart() {
        use wsp_store_contract::Principal as P;
        let env = util::isolated("fold-served");
        let store = Store::open();
        let exec = env.state().join("exec");
        let m = manager(&exec, 0);
        let addr = m.addrs()[0];
        let db: SharedStore = Arc::new(
            wsp_store_sqlite::SqliteStore::open(store.state.join("state.db"), Arc::new(wsp_server::Mirror)).unwrap(),
        );
        let mut cfg = wsp_server::Config::new(&store.state);
        cfg.port = 0;
        let server = wsp_server::Server::start(db.clone(), cfg).expect("the API serves on loopback");
        let client = wsp_client::Client::dial(&store.state, P::Wsp("test".into()), Duration::from_secs(10)).expect("dial");

        // A daemon's fold thread, as `start` runs it.
        let run_fold = |tok: Token| {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let (root, state, db) = (store.root.clone(), store.state.clone(), db.clone());
            let h = std::thread::spawn(move || {
                let store = Store::at(root, state.clone());
                let mut sub = Subscription::new(addr, tok, HOST, &state, "test");
                follow(&store, Some(&db), HOST, &mut sub, &stop_ref(&flag));
            });
            (h, stop)
        };
        fn stop_ref(f: &Arc<AtomicBool>) -> &AtomicBool {
            f
        }
        let seat_puts = |from: Option<u64>, wait: Duration| -> Vec<Value> {
            let mut feed = client.subscribe(from.map(wsp_api::EventId));
            let end = Instant::now() + wait;
            let mut out = Vec::new();
            while Instant::now() < end {
                if let Some(wsp_client::Item::Event(e)) = feed.next_within(Duration::from_millis(100)) {
                    let v = serde_json::to_value(&e).unwrap();
                    if v["kind"] == "mirror.put" && v.to_string().contains("cpd-f1") {
                        out.push(v);
                    }
                }
            }
            out
        };

        let (h, stop) = run_fold(token(&exec));
        let started = m
            .emit(Message::SeatStarted(SeatStarted {
                key: Key { row: RowId("wsp-075".into()), attempt: 1 },
                seat: seat(),
                epoch: Epoch(1),
                pid: 4242,
                sockets: vec![],
            }))
            .unwrap();
        let stopped = m
            .emit(Message::SeatStopped(SeatStopped { seat: seat(), epoch: Epoch(1), exit: Some(0), ended: vec![4242], renderer_left: false }))
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while read_folded(&store.state, HOST) != Some(EventId(stopped.0)) {
            assert!(Instant::now() < end, "the fold never reached the stop ({:?}, {started:?})", read_folded(&store.state, HOST));
            std::thread::sleep(Duration::from_millis(50));
        }
        let before = seat_puts(Some(0), Duration::from_secs(2));
        let seat_of = |v: &Value, word: &str| v.to_string().contains(word);
        let seats: Vec<&Value> = before.iter().filter(|v| v["subject"].to_string().contains("seat")).collect();
        assert_eq!(seats.len(), 2, "one put for the seat's start and one for its stop: {before:?}");
        assert!(seat_of(seats[0], "started") && seat_of(seats[1], "gone"), "{seats:?}");
        let head = db.head().unwrap();

        // The daemon restarts: the fold stops and runs again over the same
        // database, with its place and the folded id lost.
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap();
        let _ = std::fs::remove_file(wsp_wire::host_tcp::last_path(&store.state, HOST));
        let _ = std::fs::remove_file(folded_path(&store.state, HOST));
        let (h, stop) = run_fold(token(&exec));
        let end = Instant::now() + Duration::from_secs(10);
        while read_folded(&store.state, HOST) != Some(EventId(stopped.0)) {
            assert!(Instant::now() < end, "the second fold never replayed the journal");
            std::thread::sleep(Duration::from_millis(50));
        }
        let live = seat_puts(Some(head.0), Duration::from_secs(2));
        assert!(live.is_empty(), "the replay put the seat again: {live:?}");
        let all = seat_puts(Some(0), Duration::from_secs(2));
        assert_eq!(all.len(), before.len(), "the journal holds each put once: {all:?}");
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap();
        server.stop();
        m.stop();
    }
}

#[cfg(test)]
mod mirror_tests {
    use super::*;
    use wsp_host_api::{Epoch, Key, RowId, SeatId, SeatStarted, SeatStopped};
    use wsp_store_contract::{EventKind, Events};

    fn open(state: &Path) -> SharedStore {
        Arc::new(wsp_store_sqlite::SqliteStore::open(state.join("state.db"), Arc::new(wsp_server::Mirror)).unwrap())
    }

    fn frame(id: u64, msg: Message) -> Frame {
        Frame { id: Some(EventId(id)), req: None, msg }
    }

    fn puts(db: &SharedStore) -> Vec<(String, String)> {
        db.events_after(wsp_store_contract::EventId(0), 1000)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EventKind::MirrorPut)
            .map(|e| (format!("{:?}", e.subject), e.payload.to_string()))
            .collect()
    }

    /// A manager's `seat.started` and `seat.stopped` each put the seat once;
    /// a new fold over the same database (a restart, with even the `.folded`
    /// id lost) puts neither again.
    #[test]
    fn a_started_and_a_stopped_each_put_the_seat_once_across_a_restart() {
        let env = util::isolated("fold-mirror");
        let store = Store::open();
        let started = frame(
            1,
            Message::SeatStarted(SeatStarted {
                key: Key { row: RowId("wsp-t1".into()), attempt: 1 },
                seat: SeatId("cpd-t1".into()),
                epoch: Epoch(1),
                pid: 4242,
                sockets: vec![],
            }),
        );
        let stopped = frame(
            2,
            Message::SeatStopped(SeatStopped { seat: SeatId("cpd-t1".into()), epoch: Epoch(1), exit: Some(0), ended: vec![4242], renderer_left: false }),
        );
        let db = open(&env.state());
        assert!(fold_into(&store, Some(&db), "h", &started));
        let seat_puts = |db: &SharedStore| puts(db).into_iter().filter(|(s, p)| s.contains("cpd-t1") || p.contains("cpd-t1")).collect::<Vec<_>>();
        let first = seat_puts(&db);
        assert_eq!(first.len(), 2, "the seat and its claim: {first:?}");
        assert!(first.iter().any(|(_, p)| p.contains("\"started\"") && p.contains("4242")), "{first:?}");
        assert!(fold_into(&store, Some(&db), "h", &stopped));
        let both = seat_puts(&db);
        assert_eq!(both.len(), 3, "the stop is one more put: {both:?}");
        assert!(both[2].1.contains("\"gone\""), "{both:?}");
        let head = db.head().unwrap();
        drop(db);

        // The restart: the same file, a new store handle, and the fold again.
        let db = open(&env.state());
        assert!(!fold_into(&store, Some(&db), "h", &started), ".folded keeps it once");
        let _ = std::fs::remove_file(folded_path(&store.state, "h"));
        fold_into(&store, Some(&db), "h", &started);
        fold_into(&store, Some(&db), "h", &stopped);
        assert_eq!(db.head().unwrap(), head, "the same keys write nothing");
    }

    /// wsp-094: after a machine restart the manager journalled every seat
    /// stopped and the fold put each gone; nothing ever forgot them. The tick's
    /// sweep forgets a gone seat with nothing to say — in the state core and in
    /// `spawned.json` — keeps one that said something,
    /// forgets a seat mirrored before the census word was carried, and a
    /// second sweep finds nothing.
    #[test]
    fn the_sweep_forgets_seats_a_restart_left_gone_with_nothing_to_say() {
        let env = util::isolated("fold-sweep-mute");
        let store = Store::open();
        let db = open(&env.state());
        let mut id = 0;
        for seat in ["cpd-r1", "cpd-r2"] {
            let s = SeatId(seat.into());
            id += 1;
            let started = SeatStarted { key: Key { row: RowId(String::new()), attempt: 0 }, seat: s.clone(), epoch: Epoch(1), pid: 19479, sockets: vec![] };
            assert!(fold_into(&store, Some(&db), "h", &frame(id, Message::SeatStarted(started))));
            id += 1;
            let stopped = SeatStopped { seat: s, epoch: Epoch(1), exit: None, ended: vec![], renderer_left: false };
            assert!(fold_into(&store, Some(&db), "h", &frame(id, Message::SeatStopped(stopped))));
        }
        store.set_sentence("cpd-r2", "stopped at the barrier", None);
        // As a build before the census word mirrored it: gone, and nothing else.
        let old = c::Seat {
            id: c::SeatId("cpd-r3".into()),
            machine: MachineName("h".into()),
            kind: "claude".into(),
            model: None,
            state: SeatState::Gone,
            before_lost: None,
            row: None,
            attempt: 0,
            pid: Some(1),
            fields: BTreeMap::from([("epoch".to_string(), "1".to_string())]),
        };
        db.apply(put_cmd(vec![Entity::Seat(old)]), IdempotencyKey("old".into()), Principal::Wsp("test".into())).unwrap();
        assert!(store.spawned().contains_key("cpd-r1"), "the fold recorded the start");
        assert!(state_since(&store, ["cpd-r1"]).contains_key("cpd-r1"), "and dated the gone");

        let mut forgot = sweep_mute(&store, &db, |s| crate::cmd_agent::mute_seats(&store, s));
        forgot.sort();
        assert_eq!(forgot, ["cpd-r1", "cpd-r3"]);
        let seats = db.snapshot().unwrap().0.seats;
        assert!(!seats.contains_key(&c::SeatId("cpd-r1".into())) && !seats.contains_key(&c::SeatId("cpd-r3".into())), "{seats:?}");
        assert!(seats.contains_key(&c::SeatId("cpd-r2".into())), "it said something");
        assert!(!store.spawned().contains_key("cpd-r1"));
        assert!(store.spawned().contains_key("cpd-r2"));
        assert!(sweep_mute(&store, &db, |s| crate::cmd_agent::mute_seats(&store, s)).is_empty(), "nothing left to forget");

        // A day after it was first seen gone, the one that said something goes too.
        let day_ago = util::epoch_secs() - crate::cmd_agent::GONE_FOR - 60;
        say(&store, "cpd-r2", |m| {
            m.insert("at".into(), json!(util::iso_at(day_ago)));
        });
        assert_eq!(sweep_mute(&store, &db, |s| crate::cmd_agent::mute_seats(&store, s)), ["cpd-r2"]);
    }

    /// A despawn of a gone seat that said something ends its record, and the
    /// next sweep forgets it at once.
    #[test]
    fn the_sweep_forgets_a_despawned_seat_at_once() {
        let env = util::isolated("fold-sweep-despawned");
        let store = Store::open();
        let db = open(&env.state());
        let s = SeatId("cpd-d1".into());
        let started = SeatStarted { key: Key { row: RowId(String::new()), attempt: 0 }, seat: s.clone(), epoch: Epoch(1), pid: 1, sockets: vec![] };
        fold_into(&store, Some(&db), "h", &frame(1, Message::SeatStarted(started)));
        let stopped = SeatStopped { seat: s, epoch: Epoch(1), exit: None, ended: vec![], renderer_left: false };
        fold_into(&store, Some(&db), "h", &frame(2, Message::SeatStopped(stopped)));
        store.set_sentence("cpd-d1", "stopped at the barrier", None);
        assert!(sweep_mute(&store, &db, |s| crate::cmd_agent::mute_seats(&store, s)).is_empty(), "it said something");
        store.end_spawned("cpd-d1", util::epoch_secs());
        assert_eq!(sweep_mute(&store, &db, |s| crate::cmd_agent::mute_seats(&store, s)), ["cpd-d1"]);
    }
}

#[cfg(test)]
mod orders_tests {
    //! wsp-123: under `store` the fold is the orders machine's second reporter.
    use super::*;
    use crate::orders::tests::{db, ed, seat};
    use crate::orders::Mode;
    use wsp_host_api::{At, Epoch, OrderAcked, OrderDelivered, OrderFail, OrderId, OrderRefused, Turn};
    use wsp_store_contract::{EventId as CEventId, EventKind, Fence, LeaseName, Leases, Subject};

    fn frame(id: u64, msg: Message) -> Frame {
        Frame { id: Some(EventId(id)), req: None, msg }
    }

    fn send(db: &SharedStore, to: &str) -> c::OrderId {
        let cmd = Command::OrderSend { to: SeatId(to.into()), text: "do it".into(), first: false };
        crate::orders::send_in(db, cmd, &ed()).unwrap()
    }

    /// `queued` → `typed`, as the worker does it, under its lease and key.
    fn typed(db: &SharedStore, id: &c::OrderId, to: &str) {
        let lease = LeaseName::deliver(&SeatId(to.into()));
        let by = Principal::Wsp("wsp".into());
        let t = db.take(&lease, &by, Duration::from_secs(60)).unwrap().unwrap();
        let cmd = Command::OrderDeliver { order: id.clone(), fence: Fence { lease: lease.clone(), token: t } };
        db.apply(cmd, IdempotencyKey(format!("order:{}:deliver:{}", id.0, t.0)), by).unwrap();
        Leases::release(&**db, &lease, t).unwrap();
    }

    fn state(db: &SharedStore, id: &c::OrderId) -> c::Order {
        db.snapshot().unwrap().0.orders[id].clone()
    }

    fn count(db: &SharedStore, id: &c::OrderId, kind: EventKind) -> usize {
        db.events_after(CEventId(0), 10_000)
            .unwrap()
            .into_iter()
            .filter(|e| e.subject == Subject::Order(id.clone()) && e.kind == kind)
            .count()
    }

    fn hid(id: &c::OrderId) -> OrderId {
        OrderId(id.0.clone())
    }

    fn delivered(id: &c::OrderId, to: &str) -> Message {
        Message::OrderDelivered(OrderDelivered { order: hid(id), seat: wsp_host_api::SeatId(to.into()) })
    }

    fn acked(id: &c::OrderId, to: &str) -> Message {
        Message::OrderAcked(OrderAcked { order: hid(id), seat: wsp_host_api::SeatId(to.into()), turn_began: At(500_000) })
    }

    fn turn(to: &str, edge: TurnEdge, order: Option<&c::OrderId>) -> Message {
        Message::Turn(Turn { seat: wsp_host_api::SeatId(to.into()), epoch: Epoch(1), edge, at: At(500_000), order: order.map(hid) })
    }

    /// A manager's `order.acked` for a typed order moves it to `acknowledged`
    /// once, and a new fold over the same file (a restart, `.folded` lost)
    /// writes nothing more; `orders.json` is written as under `files`.
    #[test]
    fn a_managers_ack_moves_a_typed_order_to_acknowledged_once_across_a_restart() {
        let env = util::isolated("fold-orders-ack");
        let store = Store::open();
        let dir = env.state().join("db");
        let db = db(&dir, wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let id = send(&db, "cpd-1");
        typed(&db, &id, "cpd-1");
        assert!(fold_as(&store, Some(&db), "h", &frame(1, delivered(&id, "cpd-1")), Mode::Store));
        assert!(fold_as(&store, Some(&db), "h", &frame(2, acked(&id, "cpd-1")), Mode::Store));
        assert_eq!(state(&db, &id).state, OrderState::Acknowledged);
        assert_eq!(count(&db, &id, EventKind::OrderAcknowledged), 1);
        assert_eq!(read(&orders_path(&store.state, "h"))[&id.0]["acked"].as_u64(), Some(500_000));
        let head = db.head().unwrap();
        drop(db);

        let db: SharedStore = Arc::new(
            wsp_store_sqlite::SqliteStore::open(dir.join("state.db"), Arc::new(wsp_server::Composed::new(wsp_server::Mode::Owned)))
                .unwrap(),
        );
        assert!(!fold_as(&store, Some(&db), "h", &frame(2, acked(&id, "cpd-1")), Mode::Store), ".folded keeps it once");
        let _ = std::fs::remove_file(folded_path(&store.state, "h"));
        fold_as(&store, Some(&db), "h", &frame(1, delivered(&id, "cpd-1")), Mode::Store);
        fold_as(&store, Some(&db), "h", &frame(2, acked(&id, "cpd-1")), Mode::Store);
        assert_eq!(db.head().unwrap(), head, "the same keys write nothing");
        assert_eq!(count(&db, &id, EventKind::OrderAcknowledged), 1);
    }

    /// The worker heard the ack first: the fold's same answer is no second
    /// `acknowledged`.
    #[test]
    fn an_ack_the_worker_applied_first_is_applied_once() {
        let env = util::isolated("fold-orders-worker");
        let store = Store::open();
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let id = send(&db, "cpd-1");
        let mut w = crate::deliver::Worker::new(db.clone(), "wsp", Box::new(|_, _, _| Ok(crate::place::Delivery::Acked)));
        assert_eq!(w.pass(), vec![id.clone()]);
        assert_eq!(state(&db, &id).state, OrderState::Acknowledged);
        let head = db.head().unwrap();
        fold_as(&store, Some(&db), "h", &frame(1, delivered(&id, "cpd-1")), Mode::Store);
        fold_as(&store, Some(&db), "h", &frame(2, acked(&id, "cpd-1")), Mode::Store);
        assert_eq!(db.head().unwrap(), head);
        assert_eq!(count(&db, &id, EventKind::OrderAcknowledged), 1);
    }

    /// The turn the ack began ends: `done`. A turn begun on another order, or
    /// on none, ends: the acknowledged order stays so.
    #[test]
    fn a_turn_that_ends_after_its_orders_ack_is_done_and_another_turn_is_not() {
        let env = util::isolated("fold-orders-done");
        let store = Store::open();
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let a = send(&db, "cpd-1");
        typed(&db, &a, "cpd-1");
        let mut n = 0;
        let mut fold = |msg: Message| {
            n += 1;
            fold_as(&store, Some(&db), "h", &frame(n, msg), Mode::Store)
        };
        fold(acked(&a, "cpd-1"));
        fold(turn("cpd-1", TurnEdge::Began, None));
        fold(turn("cpd-1", TurnEdge::Ended, None));
        assert_eq!(state(&db, &a).state, OrderState::Acknowledged, "a turn on no order");
        let other = c::OrderId("o-other".into());
        fold(turn("cpd-1", TurnEdge::Began, Some(&other)));
        fold(turn("cpd-1", TurnEdge::Ended, None));
        assert_eq!(state(&db, &a).state, OrderState::Acknowledged, "a turn on another order");
        fold(turn("cpd-1", TurnEdge::Began, Some(&a)));
        assert_eq!(state(&db, &a).state, OrderState::Acknowledged, "still turning");
        fold(turn("cpd-1", TurnEdge::Ended, None));
        assert_eq!(state(&db, &a).state, OrderState::Done);
        assert_eq!(count(&db, &a, EventKind::OrderDone), 1);
    }

    /// An order the store does not hold goes to the files alone: no `Order`
    /// is mirrored, and nothing is refused.
    #[test]
    fn an_order_the_store_does_not_hold_is_written_to_the_files_only() {
        let env = util::isolated("fold-orders-unheld");
        let store = Store::open();
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        let id = c::OrderId("o-files".into());
        let head = db.head().unwrap();
        assert!(fold_as(&store, Some(&db), "h", &frame(1, acked(&id, "cpd-1")), Mode::Store));
        assert_eq!(db.head().unwrap(), head);
        assert!(db.snapshot().unwrap().0.orders.is_empty());
        assert_eq!(read(&orders_path(&store.state, "h"))["o-files"]["acked"].as_u64(), Some(500_000));
    }

    /// compound-sup's `busy` is a refusal that counts nothing: the order is
    /// queued again with no attempt spent, and a second hearing writes nothing.
    #[test]
    fn a_busy_refusal_counts_no_attempt() {
        let env = util::isolated("fold-orders-busy");
        let store = Store::open();
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let id = send(&db, "cpd-1");
        typed(&db, &id, "cpd-1");
        let busy = Message::OrderRefused(OrderRefused { order: hid(&id), seat: wsp_host_api::SeatId("cpd-1".into()), why: OrderFail::Busy });
        fold_as(&store, Some(&db), "h", &frame(1, busy.clone()), Mode::Store);
        let o = state(&db, &id);
        assert_eq!((o.state, o.attempts, o.why.as_deref()), (OrderState::Queued, 0, Some("busy")));
        let head = db.head().unwrap();
        let _ = std::fs::remove_file(folded_path(&store.state, "h"));
        fold_as(&store, Some(&db), "h", &frame(1, busy), Mode::Store);
        assert_eq!(db.head().unwrap(), head);
    }
}
