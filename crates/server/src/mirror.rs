//! The mirror domain (docs/state-core.md section 7's step 1, wsp-070): the
//! [`Domain`] step 1's store runs, while the files are the truth and the store
//! only copies them. Pure: no IO, no clock, nothing but the [`State`] it is
//! given.
//!
//! **`decide`** takes [`Command::Mirror`] alone. Each entity of `put` that
//! differs from what the state holds at its subject (or is not there) becomes a
//! `mirror.put` carrying the whole [`Entity`]; each subject of `gone` that the
//! state holds becomes a `mirror.gone` with payload `null`. `put` is decided
//! first, then `gone`, each in the order given, and each against the state as
//! the drafts before it leave it, so a subject put twice is written once and a
//! subject put and then gone is written put, gone. An entity equal to what is
//! there writes nothing, so a re-mirror of an unchanged store is silent. A
//! machine that differs from the one held only in its liveness (its `beat`
//! and the [`LIVENESS`] facts: `last_seen` and the gauges a beat carries) is a
//! heartbeat, not a change: it becomes a `machine.beat` carrying the whole
//! [`Machine`] instead of a `mirror.put` (wsp-304), so a subscriber that wakes
//! on entity events is not woken every beat, while the log keeps every beat in
//! order and a resume replays them as it replays the rest. Every
//! other verb is refused `illegal`, with the state `mirrored`: step 1's store
//! owns nothing, so no machine's verb applies to it.
//!
//! **`evolve`** folds `mirror.put` (the entity replaces what its subject held),
//! `machine.beat` (the machine replaces what its name held, as a put does),
//! `mirror.gone` (what the subject names is removed), and the events the store
//! writes itself (the `store` module's table in the contract): `lease.taken`
//! puts its [`Lease`], `lease.released` removes it, `claim.taken` puts its
//! [`Claim`], `claim.released` removes it, and `text.set` changes no state. Any
//! other kind leaves the state as it was. So `snapshot()` is the fold of the
//! log (2.3 case 6) with this domain too.
//!
//! The subjects a mirror may name are those `State` keys: a row, a list, a
//! seat, a claim (by its row), an order, an ask, a machine, and for `gone` a
//! lease. A `gone` of a project, a group or a run names nothing the state
//! keys, so it writes nothing.

use serde_json::Value;
use wsp_store_contract::{
    Claim, Command, Domain, Draft, Entity, Event, EventKind, Lease, Machine, Principal, ProjectId, Refusal, State,
    Subject,
};

/// The state a refused verb names: step 1's store owns nothing, it mirrors.
pub const MIRRORED: &str = "mirrored";

/// The mirror domain. Stateless: everything it knows is in the [`State`] it is given.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mirror;

/// The subject an entity is put under (a claim's is its row's `Claim`).
pub fn subject_of(entity: &Entity) -> Subject {
    match entity {
        Entity::Row(r) => Subject::Row(r.id.clone()),
        Entity::List(l) => Subject::List(l.id.clone()),
        Entity::Seat(s) => Subject::Seat(s.id.clone()),
        Entity::Claim(c) => Subject::Claim(c.row.clone()),
        Entity::Order(o) => Subject::Order(o.id.clone()),
        Entity::Ask(a) => Subject::Ask(a.id.clone()),
        Entity::Machine(m) => Subject::Machine(m.name.clone()),
    }
}

/// A machine's facts that move with every beat and say nothing new of it:
/// `last_seen` (`machines.json`, written by the tunnel supervisor) and the
/// gauges a manager's beat carries (`HostFacts`: the load averages, swap and
/// free memory, pageouts, HID idle, the speed limit and the disks' free
/// bytes). What a beat carries that is a state — `memory` and `thermal`
/// (banded), `cpus` — and every fact of the record (`reachable`, `ready`,
/// `status`, `error`, ...) is not here: a change in one is a change.
pub const LIVENESS: &[&str] = &[
    "last_seen",
    "load_centi",
    "load5_centi",
    "load15_centi",
    "swap_used_bytes",
    "free_memory_bytes",
    "pageouts",
    "hid_idle_secs",
    "speed_limit_pct",
    "disks",
];

/// Whether `now` differs from `held` only in its liveness: the same name and
/// transport, and the same facts once the [`LIVENESS`] ones are set aside
/// (its `beat` may be anything).
pub fn beat_only(held: &Machine, now: &Machine) -> bool {
    fn rest(m: &Machine) -> Vec<(&String, &String)> {
        m.facts.iter().filter(|(k, _)| !LIVENESS.contains(&k.as_str())).collect()
    }
    held.name == now.name && held.transport == now.transport && rest(held) == rest(now)
}

/// What `state` holds at `subject`, as an entity; `None` when nothing, or when
/// the subject is not one an entity is put under (a lease, a project, ...).
fn entity_at(state: &State, subject: &Subject) -> Option<Entity> {
    match subject {
        Subject::Row(id) => state.rows.get(id).cloned().map(Entity::Row),
        Subject::List(id) => state.lists.get(id).cloned().map(Entity::List),
        Subject::Seat(id) => state.seats.get(id).cloned().map(Entity::Seat),
        Subject::Claim(row) => state.claims.get(row).cloned().map(Entity::Claim),
        Subject::Order(id) => state.orders.get(id).cloned().map(Entity::Order),
        Subject::Ask(id) => state.asks.get(id).cloned().map(Entity::Ask),
        Subject::Machine(name) => state.machines.get(name).cloned().map(Entity::Machine),
        Subject::Project(_) | Subject::Group(_) | Subject::Run(_) | Subject::Lease(_) => None,
    }
}

/// Whether `state` holds anything at `subject`.
fn present(state: &State, subject: &Subject) -> bool {
    match subject {
        Subject::Lease(name) => state.leases.contains_key(name),
        other => entity_at(state, other).is_some(),
    }
}

/// `state` with `entity` at its subject.
fn put(state: &mut State, entity: Entity) {
    match entity {
        Entity::Row(r) => {
            state.rows.insert(r.id.clone(), r);
        }
        Entity::List(l) => {
            state.lists.insert(l.id.clone(), l);
        }
        Entity::Seat(s) => {
            state.seats.insert(s.id.clone(), s);
        }
        Entity::Claim(c) => {
            state.claims.insert(c.row.clone(), c);
        }
        Entity::Order(o) => {
            state.orders.insert(o.id.clone(), o);
        }
        Entity::Ask(a) => {
            state.asks.insert(a.id.clone(), a);
        }
        Entity::Machine(m) => {
            state.machines.insert(m.name.clone(), m);
        }
    }
}

/// `state` without what `subject` names.
fn remove(state: &mut State, subject: &Subject) {
    match subject {
        Subject::Row(id) => {
            state.rows.remove(id);
        }
        Subject::List(id) => {
            state.lists.remove(id);
        }
        Subject::Seat(id) => {
            state.seats.remove(id);
        }
        Subject::Claim(row) => {
            state.claims.remove(row);
        }
        Subject::Order(id) => {
            state.orders.remove(id);
        }
        Subject::Ask(id) => {
            state.asks.remove(id);
        }
        Subject::Machine(name) => {
            state.machines.remove(name);
        }
        Subject::Lease(name) => {
            state.leases.remove(name);
        }
        Subject::Project(_) | Subject::Group(_) | Subject::Run(_) => {}
    }
}

/// The verb a command is, as the wire names it (`row.claim`).
fn verb(cmd: &Command) -> String {
    serde_json::to_value(cmd)
        .ok()
        .and_then(|v| v.get("verb").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

/// The subject a refused command is about: the id it names first, or the
/// project `""` when it names none.
fn about(cmd: &Command) -> Subject {
    match cmd {
        Command::RowAdd { project, .. } => Subject::Project(project.clone()),
        Command::RowEdit { row, .. }
        | Command::RowClaim { row, .. }
        | Command::RowReview { row, .. }
        | Command::RowBlock { row, .. }
        | Command::RowPark { row }
        | Command::RowLand { row, .. } => Subject::Row(row.clone()),
        Command::ListGo { list } | Command::ListHold { list, .. } | Command::ListRelease { list, .. } => {
            Subject::List(list.clone())
        }
        Command::SeatSpawn { row: Some(row), .. } => Subject::Row(row.clone()),
        Command::SeatEnd { seat, .. } | Command::SeatDespawn { seat, .. } => Subject::Seat(seat.clone()),
        Command::OrderSend { to, .. } => Subject::Seat(to.clone()),
        Command::OrderCancel { order }
        | Command::OrderDeliver { order, .. }
        | Command::OrderDelivered { order }
        | Command::OrderAcked { order, .. }
        | Command::OrderRefused { order, .. }
        | Command::OrderDone { order } => Subject::Order(order.clone()),
        Command::AskAnswer { ask, .. } | Command::AskAck { ask } => Subject::Ask(ask.clone()),
        Command::AskRaise { about: Some(s), .. } => s.clone(),
        Command::MachineSet { name, .. } => Subject::Machine(name.clone()),
        Command::SeatSpawn { row: None, .. } | Command::AskRaise { about: None, .. } | Command::Mirror { .. } => {
            Subject::Project(ProjectId(String::new()))
        }
    }
}

impl Domain for Mirror {
    fn decide(&self, state: &State, cmd: &Command, _by: &Principal) -> Result<Vec<Draft>, Refusal> {
        let Command::Mirror { put: puts, gone } = cmd else {
            return Err(Refusal::Illegal { subject: about(cmd), state: MIRRORED.into(), verb: verb(cmd) });
        };
        // What the drafts so far have made of each subject they touched:
        // `Some` an entity, `None` removed. Untouched subjects read `state`.
        let mut touched: std::collections::BTreeMap<Subject, Option<Entity>> = Default::default();
        let mut drafts = Vec::new();
        for entity in puts {
            let subject = subject_of(entity);
            let now = match touched.get(&subject) {
                Some(e) => e.clone(),
                None => entity_at(state, &subject),
            };
            if now.as_ref() == Some(entity) {
                continue;
            }
            let (kind, payload) = match (&now, entity) {
                (Some(Entity::Machine(held)), Entity::Machine(m)) if beat_only(held, m) => {
                    (EventKind::MachineBeat, serde_json::to_value(m))
                }
                _ => (EventKind::MirrorPut, serde_json::to_value(entity)),
            };
            let payload =
                payload.map_err(|e| Refusal::Invalid { reason: format!("an entity that does not serialize: {e}") })?;
            drafts.push(Draft { kind, subject: subject.clone(), payload });
            touched.insert(subject, Some(entity.clone()));
        }
        for subject in gone {
            let here = match touched.get(subject) {
                Some(e) => e.is_some(),
                None => present(state, subject),
            };
            if !here {
                continue;
            }
            drafts.push(Draft { kind: EventKind::MirrorGone, subject: subject.clone(), payload: Value::Null });
            touched.insert(subject.clone(), None);
        }
        Ok(drafts)
    }

    fn evolve(&self, mut state: State, event: &Event) -> State {
        let p = &event.payload;
        match event.kind {
            EventKind::MirrorPut => {
                if let Ok(entity) = serde_json::from_value::<Entity>(p.clone()) {
                    put(&mut state, entity);
                }
            }
            EventKind::MachineBeat => {
                if let Ok(m) = serde_json::from_value::<Machine>(p.clone()) {
                    state.machines.insert(m.name.clone(), m);
                }
            }
            EventKind::MirrorGone => remove(&mut state, &event.subject),
            EventKind::LeaseTaken => {
                if let Ok(l) = serde_json::from_value::<Lease>(p.clone()) {
                    state.leases.insert(l.name.clone(), l);
                }
            }
            EventKind::LeaseReleased => {
                if let Subject::Lease(name) = &event.subject {
                    state.leases.remove(name);
                }
            }
            EventKind::ClaimTaken => {
                if let Ok(c) = serde_json::from_value::<Claim>(p.clone()) {
                    state.claims.insert(c.row.clone(), c);
                }
            }
            EventKind::ClaimReleased => {
                if let Subject::Claim(row) = &event.subject {
                    state.claims.remove(row);
                }
            }
            _ => {}
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_store_contract::{At, EventId, LeaseName, ProjectId, Row, RowId, RowState, SeatId, Token};

    fn ed() -> Principal {
        Principal::Person("ed".into())
    }

    fn row(id: &str, title: &str) -> Row {
        Row {
            id: RowId(id.into()),
            project: ProjectId("wsp".into()),
            title: title.into(),
            state: RowState::Todo,
            before_block: None,
            seat: None,
            group: None,
            commit: None,
            fields: Default::default(),
        }
    }

    /// Stamps drafts as a store would and folds them.
    fn fold(state: State, drafts: Vec<Draft>) -> State {
        drafts.into_iter().enumerate().fold(state, |s, (i, d)| {
            let e = Event {
                id: EventId(i as u64 + 1),
                at: At(0),
                principal: ed(),
                kind: d.kind,
                subject: d.subject,
                payload: d.payload,
            };
            Mirror.evolve(s, &e)
        })
    }

    fn mirror(put: Vec<Entity>, gone: Vec<Subject>) -> Command {
        Command::Mirror { put, gone }
    }

    /// The orders machine's verbs (wsp-117) are refused as every verb but
    /// `mirror` is: `illegal`, state `mirrored`, naming the order.
    #[test]
    fn the_orders_machine_verbs_are_refused_mirrored() {
        use wsp_store_contract::{Fence, OrderId};
        let o = OrderId("ord-1".into());
        let fence = Fence { lease: LeaseName::deliver(&SeatId("cpd-1".into())), token: Token(1) };
        for cmd in [
            Command::OrderDeliver { order: o.clone(), fence },
            Command::OrderDelivered { order: o.clone() },
            Command::OrderAcked { order: o.clone(), turn_began: At(1) },
            Command::OrderRefused { order: o.clone(), why: "busy".into(), counts: true },
            Command::OrderDone { order: o.clone() },
        ] {
            assert_eq!(
                Mirror.decide(&State::default(), &cmd, &ed()),
                Err(Refusal::Illegal { subject: Subject::Order(o.clone()), state: MIRRORED.into(), verb: verb(&cmd) })
            );
        }
    }

    #[test]
    fn an_unchanged_entity_writes_nothing_a_changed_one_a_put_and_a_removed_one_a_gone() {
        let first = mirror(vec![Entity::Row(row("r-1", "one")), Entity::Row(row("r-2", "two"))], vec![]);
        let drafts = Mirror.decide(&State::default(), &first, &ed()).unwrap();
        assert_eq!(drafts.len(), 2);
        assert!(drafts.iter().all(|d| d.kind == EventKind::MirrorPut));
        let state = fold(State::default(), drafts);
        assert_eq!(state.rows.len(), 2);

        assert_eq!(Mirror.decide(&state, &first, &ed()).unwrap(), vec![], "a re-mirror of the same is silent");

        let changed = mirror(vec![Entity::Row(row("r-1", "one")), Entity::Row(row("r-2", "TWO"))], vec![]);
        let drafts = Mirror.decide(&state, &changed, &ed()).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!((drafts[0].kind, &drafts[0].subject), (EventKind::MirrorPut, &Subject::Row(RowId("r-2".into()))));
        let state = fold(state, drafts);
        assert_eq!(state.rows[&RowId("r-2".into())].title, "TWO");

        let gone = mirror(vec![], vec![Subject::Row(RowId("r-1".into())), Subject::Row(RowId("r-9".into()))]);
        let drafts = Mirror.decide(&state, &gone, &ed()).unwrap();
        assert_eq!(drafts.len(), 1, "a subject that is not there is not gone again");
        assert_eq!(drafts[0].kind, EventKind::MirrorGone);
        assert_eq!(drafts[0].payload, Value::Null);
        let state = fold(state, drafts);
        assert_eq!(state.rows.keys().cloned().collect::<Vec<_>>(), vec![RowId("r-2".into())]);
    }

    #[test]
    fn one_command_is_decided_against_its_own_drafts() {
        let twice = mirror(vec![Entity::Row(row("r-1", "a")), Entity::Row(row("r-1", "a"))], vec![]);
        assert_eq!(Mirror.decide(&State::default(), &twice, &ed()).unwrap().len(), 1);
        let put_then_gone = mirror(vec![Entity::Row(row("r-1", "a"))], vec![Subject::Row(RowId("r-1".into()))]);
        let drafts = Mirror.decide(&State::default(), &put_then_gone, &ed()).unwrap();
        assert_eq!(drafts.iter().map(|d| d.kind).collect::<Vec<_>>(), [EventKind::MirrorPut, EventKind::MirrorGone]);
        assert_eq!(fold(State::default(), drafts), State::default());
    }

    #[test]
    fn a_claim_is_put_under_its_row_and_a_lease_may_be_gone() {
        let claim = Claim { row: RowId("r-1".into()), seat: SeatId("s-1".into()), token: Token(3), expires: At(9) };
        let drafts = Mirror.decide(&State::default(), &mirror(vec![Entity::Claim(claim.clone())], vec![]), &ed()).unwrap();
        assert_eq!(drafts[0].subject, Subject::Claim(RowId("r-1".into())));
        let mut state = fold(State::default(), drafts);
        assert_eq!(state.claims[&RowId("r-1".into())], claim);

        let name = LeaseName("tick".into());
        state.leases.insert(name.clone(), Lease { name: name.clone(), holder: ed(), token: Token(1), expires: At(1) });
        let drafts = Mirror.decide(&state, &mirror(vec![], vec![Subject::Lease(name.clone())]), &ed()).unwrap();
        assert_eq!(drafts.len(), 1);
        assert!(fold(state, drafts).leases.is_empty());
    }

    #[test]
    fn every_other_verb_is_illegal() {
        let add = Command::RowAdd { project: ProjectId("wsp".into()), title: "x".into(), fields: Default::default() };
        assert_eq!(
            Mirror.decide(&State::default(), &add, &ed()),
            Err(Refusal::Illegal {
                subject: Subject::Project(ProjectId("wsp".into())),
                state: MIRRORED.into(),
                verb: "row.add".into()
            })
        );
        let park = Command::RowPark { row: RowId("r-1".into()) };
        assert!(matches!(
            Mirror.decide(&State::default(), &park, &ed()),
            Err(Refusal::Illegal { verb, .. }) if verb == "row.park"
        ));
    }

    #[test]
    fn evolve_folds_the_stores_own_events_and_ignores_the_rest() {
        let ev = |kind, subject, payload| Event { id: EventId(1), at: At(0), principal: ed(), kind, subject, payload };
        let name = LeaseName("tick".into());
        let lease = Lease { name: name.clone(), holder: ed(), token: Token(1), expires: At(5) };
        let s = Mirror.evolve(
            State::default(),
            &ev(EventKind::LeaseTaken, Subject::Lease(name.clone()), serde_json::to_value(&lease).unwrap()),
        );
        assert_eq!(s.leases[&name], lease);
        let s = Mirror.evolve(s, &ev(EventKind::LeaseReleased, Subject::Lease(name), Value::Null));
        assert!(s.leases.is_empty());
        let s2 = Mirror.evolve(s.clone(), &ev(EventKind::RowParked, Subject::Row(RowId("r-1".into())), Value::Null));
        assert_eq!(s2, s);
        let s3 = Mirror.evolve(s.clone(), &ev(EventKind::TextSet, Subject::Row(RowId("r-1".into())), Value::Null));
        assert_eq!(s3, s);
    }

    fn machine(beat: u64, facts: &[(&str, &str)]) -> Machine {
        Machine {
            name: wsp_store_contract::MachineName("mb2".into()),
            transport: wsp_store_contract::Transport::Tcp { addr: "mb2".into() },
            beat: Some(At(beat)),
            facts: facts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    /// wsp-304: a heartbeat (the beat, `last_seen`, a beat's gauges) is a
    /// `machine.beat` that still moves the state, and no `mirror.put`; a real
    /// change of the machine, and a machine not held before, is a `mirror.put`.
    #[test]
    fn a_beat_only_machine_update_is_a_machine_beat_and_a_real_change_a_put() {
        let first = machine(1, &[("reachable", "true"), ("last_seen", "10:00"), ("load_centi", "120")]);
        let drafts = Mirror.decide(&State::default(), &mirror(vec![Entity::Machine(first.clone())], vec![]), &ed()).unwrap();
        assert_eq!(drafts.iter().map(|d| d.kind).collect::<Vec<_>>(), vec![EventKind::MirrorPut], "a new machine is a put");
        let state = fold(State::default(), drafts);

        let beat = machine(2, &[("reachable", "true"), ("last_seen", "10:01"), ("load_centi", "340")]);
        let drafts = Mirror.decide(&state, &mirror(vec![Entity::Machine(beat.clone())], vec![]), &ed()).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!((drafts[0].kind, &drafts[0].subject), (EventKind::MachineBeat, &Subject::Machine(beat.name.clone())));
        assert!(drafts.iter().all(|d| d.kind != EventKind::MirrorPut), "a beat is no entity event");
        assert_eq!(serde_json::from_value::<Machine>(drafts[0].payload.clone()).unwrap(), beat);
        let state = fold(state, drafts);
        assert_eq!(state.machines.get(&beat.name), Some(&beat), "the beat still moves the state");

        let again = Mirror.decide(&state, &mirror(vec![Entity::Machine(beat.clone())], vec![]), &ed()).unwrap();
        assert_eq!(again, vec![], "the same beat twice writes nothing");

        let down = machine(3, &[("reachable", "false"), ("last_seen", "10:01"), ("load_centi", "340")]);
        let drafts = Mirror.decide(&state, &mirror(vec![Entity::Machine(down.clone())], vec![]), &ed()).unwrap();
        assert_eq!(drafts.iter().map(|d| d.kind).collect::<Vec<_>>(), vec![EventKind::MirrorPut], "a real change is a put");
        let state = fold(state, drafts);
        assert_eq!(state.machines.get(&down.name), Some(&down));

        let mut moved = down.clone();
        moved.transport = wsp_store_contract::Transport::Local;
        let drafts = Mirror.decide(&state, &mirror(vec![Entity::Machine(moved)], vec![]), &ed()).unwrap();
        assert_eq!(drafts[0].kind, EventKind::MirrorPut, "a transport change is a put");
    }
}
