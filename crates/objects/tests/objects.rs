//! The object format's promises (wsp-458): an object's id is its canonical
//! bytes' hash, `verify` holds bytes to their id, and `diff` names exactly how
//! two sides' refs part.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use wsp_objects::*;
use wsp_store_contract::{
    At, Entity, EventKind, Principal, ProjectId, Row, RowId, RowState, SeatId, Subject,
};

fn ws() -> WorkspaceId {
    WorkspaceId("shared:core".into())
}

fn row(id: &str) -> Row {
    Row {
        workspace: Default::default(),
        id: RowId(id.into()),
        project: ProjectId("wsp".into()),
        title: "Hashed object format for the hub".into(),
        state: RowState::Doing,
        before_block: None,
        seat: Some(SeatId("cpd-522".into())),
        group: None,
        commit: None,
        fields: BTreeMap::from([
            ("tier".into(), "medium".into()),
            ("kind".into(), "task".into()),
        ]),
    }
}

fn record(id: &str, parent: Option<ObjectId>) -> Object {
    Object::Record {
        workspace: ws(),
        subject: Subject::Row(RowId(id.into())),
        entity: Entity::Row(row(id)),
        parent,
        by: Principal::Person("jesper".into()),
        at: At(1_795_400_000_000),
    }
}

/// Every object in `v` with its keys in reverse order, written with
/// whitespace: the same JSON as `v`, spelled another way.
fn reversed(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            out.push_str("{ ");
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            keys.reverse();
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(" ,\n ");
                }
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push_str(" : ");
                reversed(&map[k], out);
            }
            out.push_str(" }");
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                reversed(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).unwrap()),
    }
}

#[test]
fn the_same_entity_encodes_to_the_same_id() {
    let (a, a_bytes) = encode(&record("wsp-458", None));
    let (b, b_bytes) = encode(&record("wsp-458", None));
    assert_eq!(a, b);
    assert_eq!(a_bytes, b_bytes);
    assert_eq!(a, ObjectId::of(&a_bytes));

    // Canonical: no whitespace, keys sorted, absent parent written as null.
    let text = String::from_utf8(a_bytes.clone()).unwrap();
    assert_eq!(canonical(&a_bytes).unwrap(), a_bytes);
    assert!(
        !text.contains(": ") && !text.contains(", ") && !text.contains('\n'),
        "{text}"
    );
    assert!(
        text.starts_with(r#"{"record":{"at":1795400000000,"by":{"person":"jesper"},"entity":"#),
        "{text}"
    );
    assert!(text.contains(r#""parent":null"#), "{text}");

    // Read back, it is the same object, and encodes to the same id.
    let back = decode(&a_bytes).unwrap();
    assert_eq!(back, record("wsp-458", None));
    assert_eq!(encode(&back).0, a);
}

#[test]
fn reordering_the_input_keys_does_not_change_the_id() {
    let object = record("wsp-458", None);
    let (id, bytes) = encode(&object);
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let mut shuffled = String::new();
    reversed(&value, &mut shuffled);
    assert_ne!(shuffled.as_bytes(), &bytes[..]);

    // The reordered text reads as the same object, which encodes to the same id.
    let read = decode(shuffled.as_bytes()).unwrap();
    assert_eq!(read, object);
    assert_eq!(encode(&read).0, id);
    // And written canonically it is the same bytes.
    assert_eq!(canonical(shuffled.as_bytes()).unwrap(), bytes);

    // An event's free-form payload is held to the same order.
    let event = |payload: &str| Object::Event {
        workspace: ws(),
        subject: Subject::Row(RowId("wsp-458".into())),
        kind: EventKind::RowClaimed,
        payload: serde_json::from_str(payload).unwrap(),
        parent: Some(id),
        by: Principal::Seat(SeatId("cpd-522".into())),
        at: At(1_795_400_000_001),
    };
    assert_eq!(
        encode(&event(
            r#"{"seat":"cpd-522","attempt":1,"why":{"z":1,"a":[2,{"y":3,"b":4}]}}"#
        ))
        .0,
        encode(&event(
            r#"{"why":{"a":[2,{"b":4,"y":3}],"z":1},"attempt":1,"seat":"cpd-522"}"#
        ))
        .0,
    );
}

#[test]
fn changing_any_byte_changes_the_id() {
    let (id, bytes) = encode(&record("wsp-458", None));
    let mut seen = BTreeSet::from([id]);
    for i in 0..bytes.len() {
        let mut changed = bytes.clone();
        changed[i] ^= 0x01;
        let other = ObjectId::of(&changed);
        assert_ne!(other, id, "flipping byte {i} kept the id");
        seen.insert(other);
        assert!(verify(&id, &changed).is_err());
    }
    assert_eq!(
        seen.len(),
        bytes.len() + 1,
        "two changed texts shared an id"
    );

    // A change made the honest way, as a new object, has a new id too.
    let mut edited = row("wsp-458");
    edited.state = RowState::Review;
    let child = Object::Record {
        workspace: ws(),
        subject: Subject::Row(RowId("wsp-458".into())),
        entity: Entity::Row(edited),
        parent: Some(id),
        by: Principal::Seat(SeatId("cpd-522".into())),
        at: At(1_795_400_000_002),
    };
    let (child_id, _) = encode(&child);
    assert_ne!(child_id, id);
    assert_eq!(child.parent(), Some(&id));
}

#[test]
fn verify_refuses_bytes_whose_hash_is_not_the_id() {
    let (id, bytes) = encode(&record("wsp-458", None));
    assert_eq!(verify(&id, &bytes), Ok(()));

    // Another object's bytes, offered under this id.
    let (other_id, other) = encode(&record("wsp-460", None));
    assert_eq!(
        verify(&id, &other),
        Err(Error::Mismatch {
            id,
            actual: other_id
        })
    );

    // The right object, spelled non-canonically: its hash is not the id, and
    // offered under its own hash it is still refused, so an object has one id.
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    assert!(matches!(verify(&id, &pretty), Err(Error::Mismatch { .. })));
    assert_eq!(
        verify(&ObjectId::of(&pretty), &pretty),
        Err(Error::NotCanonical)
    );

    // Bytes that are not JSON at all, under their own hash.
    let junk = b"not json";
    assert!(matches!(
        verify(&ObjectId::of(junk), junk),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn verify_keeps_objects_of_kinds_this_build_does_not_know() {
    // A newer writer's event: a kind and a field this build has never seen.
    let newer = json!({"event": {
        "at": 1, "by": {"person": "ed"}, "kind": "row.teleported", "parent": null,
        "payload": null, "subject": {"row": "wsp-1"}, "workspace": "shared:core",
        "zeal": true,
    }});
    let bytes = canonical(newer.to_string().as_bytes()).unwrap();
    let id = ObjectId::of(&bytes);
    assert_eq!(verify(&id, &bytes), Ok(()));
    match decode(&bytes).unwrap() {
        Object::Event { kind, .. } => assert_eq!(kind, EventKind::Unknown),
        other => panic!("read as {other:?}"),
    }
}

#[test]
fn an_id_has_one_text_form() {
    let (id, _) = encode(&record("wsp-458", None));
    let text = id.to_string();
    assert!(text.starts_with("sha256:"));
    assert_eq!(text.len(), "sha256:".len() + 64);
    assert_eq!(text.parse::<ObjectId>(), Ok(id));
    assert_eq!(
        serde_json::to_value(id).unwrap(),
        Value::String(text.clone())
    );
    assert_eq!(
        serde_json::from_value::<ObjectId>(Value::String(text.clone())).unwrap(),
        id
    );

    for bad in [
        text.to_uppercase(),
        text.replace("sha256:", "sha1:"),
        text[..text.len() - 1].to_string(),
        format!("{text}0"),
        text.replacen('a', "g", 1),
    ] {
        if bad != text {
            assert!(bad.parse::<ObjectId>().is_err(), "{bad} parsed");
        }
    }
    // sha256 of the empty string, as everyone else writes it.
    assert_eq!(
        ObjectId::of(b"").to_string(),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

fn h(n: u8) -> ObjectId {
    ObjectId::of(&[n])
}

fn s(id: &str) -> Subject {
    Subject::Row(RowId(id.into()))
}

#[test]
fn diff_names_exactly_what_each_side_lacks_and_what_differs() {
    let mut ours = Refs::new(ws());
    let mut theirs = Refs::new(ws());
    // Both, same head.
    ours.set(s("same"), h(1));
    theirs.set(s("same"), h(1));
    // Both, different heads.
    ours.set(s("moved"), h(2));
    theirs.set(s("moved"), h(3));
    // Only ours.
    ours.set(s("mine"), h(4));
    ours.set(Subject::Seat(SeatId("cpd-522".into())), h(5));
    // Only theirs.
    theirs.set(s("yours"), h(6));

    let d = diff(&ours, &theirs).unwrap();
    assert_eq!(d.we_lack, BTreeSet::from([s("yours")]));
    assert_eq!(
        d.they_lack,
        BTreeSet::from([s("mine"), Subject::Seat(SeatId("cpd-522".into()))])
    );
    assert_eq!(d.differ, BTreeSet::from([s("moved")]));
    assert!(!d.is_empty());

    // The other side sees the mirror image.
    let back = diff(&theirs, &ours).unwrap();
    assert_eq!(back.we_lack, d.they_lack);
    assert_eq!(back.they_lack, d.we_lack);
    assert_eq!(back.differ, d.differ);

    // Equal refs, and empty ones, agree.
    assert!(diff(&ours, &ours.clone()).unwrap().is_empty());
    assert!(diff(&Refs::new(ws()), &Refs::new(ws())).unwrap().is_empty());
    assert_eq!(
        diff(&Refs::new(ws()), &theirs).unwrap().we_lack.len(),
        theirs.heads.len()
    );

    // Refs of another workspace are not compared.
    let elsewhere = Refs::new(WorkspaceId("private".into()));
    assert!(matches!(
        diff(&ours, &elsewhere),
        Err(Error::OtherWorkspace { .. })
    ));
}

#[test]
fn refs_are_data_that_round_trip() {
    let mut refs = Refs::new(ws());
    refs.set(s("wsp-458"), h(1));
    refs.set(Subject::Seat(SeatId("cpd-522".into())), h(2));
    assert_eq!(refs.set(s("wsp-458"), h(3)), Some(h(1)));

    let text = serde_json::to_string(&refs).unwrap();
    assert_eq!(serde_json::from_str::<Refs>(&text).unwrap(), refs);

    // A subject named twice is refused rather than one head silently winning.
    let twice = json!({"workspace": "shared:core", "heads": [
        {"subject": {"row": "wsp-1"}, "head": h(1)},
        {"subject": {"row": "wsp-1"}, "head": h(2)},
    ]});
    assert!(serde_json::from_value::<Refs>(twice).is_err());
}

/// Retired subjects: written after the heads, the key left out when there is
/// none (so the bytes are as before), `successor` left out when none; read
/// back the same; a reader of the old shape passes the key over.
#[test]
fn retired_subjects_are_an_additive_key_of_refs() {
    let mut refs = Refs::new(ws());
    refs.set(s("pp-007"), h(1));
    let plain = serde_json::to_string(&refs).unwrap();
    assert!(!plain.contains("retired"), "{plain}");

    refs.retired.insert(s("inbox-001"), Some(s("pp-007")));
    refs.retired.insert(s("inbox-002"), None);
    let text = serde_json::to_string(&refs).unwrap();
    assert_eq!(
        text,
        format!(
            r#"{{"workspace":"shared:core","heads":[{{"subject":{{"row":"pp-007"}},"head":"{}"}}],"retired":[{{"subject":{{"row":"inbox-001"}},"successor":{{"row":"pp-007"}}}},{{"subject":{{"row":"inbox-002"}}}}]}}"#,
            h(1)
        )
    );
    assert_eq!(serde_json::from_str::<Refs>(&text).unwrap(), refs);

    // The old shape, which knows no `retired`, reads the heads and passes it over.
    #[derive(serde::Deserialize)]
    struct Old {
        heads: Vec<serde_json::Value>,
    }
    assert_eq!(serde_json::from_str::<Old>(&text).unwrap().heads.len(), 1);

    // A subject retired and with a head is refused.
    let both = json!({"workspace": "shared:core", "heads": [{"subject": {"row": "wsp-1"}, "head": h(1)}],
        "retired": [{"subject": {"row": "wsp-1"}}]});
    assert!(serde_json::from_value::<Refs>(both).is_err());
}
