//! Who holds each shared row's lease, as the hub's stream last said:
//! `$WSP_STATE/hub/holders.json` ([`Holders`]), kept by the sync worker from
//! the lease events it hears (`lease.taken` on a take or a renew,
//! `lease.released`, `lease.expired`), each carrying the holder, the expiry
//! and, from a hub that keys leases by machine, the machine `{id, name}`
//! (wsp-hub `docs/hub-deploy.md`, "The lease trail").
//!
//! What it cannot know: a lease taken before this spoke last heard the stream
//! (a worker that resumes from the hub's head after a gap skips the events
//! between, and the stream's opening state carries no lease for a user), and
//! anything while no worker runs here. A lease that has run out by its own
//! expiry is read as gone. This spoke's own leases are known from its own
//! book and claims, not from here ([`crate::spoke::Spoke::row_facts`]).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use wsp_engine::store::Store;
use wsp_store_contract::{EventKind, Principal};

/// One row's holder, as heard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The user holding it.
    pub user: String,
    /// The machine it is held on, where the hub named one: its id and name.
    pub machine: Option<(String, String)>,
    /// When it runs out, epoch seconds.
    pub expires: i64,
}

/// `$WSP_STATE/hub/holders.json`: row → [`Holder`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Holders(pub BTreeMap<String, Holder>);

pub(crate) fn path(store: &Store) -> std::path::PathBuf {
    store.state.join("hub").join("holders.json")
}

impl Holders {
    pub fn read(store: &Store) -> Holders {
        let v: Value = std::fs::read_to_string(path(store)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        Holders(
            v.as_object()
                .into_iter()
                .flatten()
                .filter_map(|(row, h)| {
                    let machine = h["machine_id"].as_str().map(|id| (id.to_string(), h["machine"].as_str().unwrap_or_default().to_string()));
                    Some((row.clone(), Holder { user: h["user"].as_str()?.to_string(), machine, expires: h["expires"].as_i64()? }))
                })
                .collect(),
        )
    }

    fn write(&self, store: &Store) -> std::io::Result<()> {
        let p = path(store);
        let dir = p.parent().expect("under hub/");
        std::fs::create_dir_all(dir)?;
        let doc: serde_json::Map<String, Value> = self
            .0
            .iter()
            .map(|(row, h)| {
                let mut o = json!({ "user": h.user, "expires": h.expires });
                if let Some((id, name)) = &h.machine {
                    o["machine_id"] = json!(id);
                    o["machine"] = json!(name);
                }
                (row.clone(), o)
            })
            .collect();
        let tmp = dir.join(format!(".holders.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, Value::Object(doc).to_string())?;
        std::fs::rename(&tmp, &p)
    }
}

/// A lease event heard on the stream, kept: a take or renew records its
/// holder, a release or an expiry forgets it. Any other event is passed over.
pub fn follow(store: &Store, kind: EventKind, payload: &Value) {
    let Some(row) = payload["row"].as_str() else { return };
    let row = row.to_string();
    let heard = match kind {
        EventKind::LeaseTaken => {
            let Some(user) = serde_json::from_value::<Principal>(payload["holder"].clone()).ok().map(|p| wsp_engine::door::user_of(&p)) else { return };
            let machine = payload["machine"]["id"].as_str().map(|id| (id.to_string(), payload["machine"]["name"].as_str().unwrap_or_default().to_string()));
            // The hub's `expires` is in milliseconds.
            let expires = payload["expires"].as_i64().map(|ms| ms / 1000).unwrap_or(0);
            Some(Holder { user, machine, expires })
        }
        EventKind::LeaseReleased | EventKind::LeaseExpired => None,
        _ => return,
    };
    let _ = store.locked(|| {
        let mut h = Holders::read(store);
        match heard {
            Some(holder) => drop(h.0.insert(row, holder)),
            None => drop(h.0.remove(&row)),
        }
        h.write(store)
    });
}

/// The holders not yet run out at `now`, as row facts: `held_here` where the
/// holder is `me` on `my_machine` (the id this spoke's hellos name).
pub fn facts(store: &Store, me: Option<&str>, my_machine: Option<&str>, now: i64) -> BTreeMap<String, wsp_engine::addon::RowFacts> {
    Holders::read(store)
        .0
        .into_iter()
        .filter(|(_, h)| h.expires > now)
        .map(|(row, h)| {
            let here = me == Some(h.user.as_str()) && h.machine.as_ref().map(|(id, _)| id.as_str()) == my_machine && my_machine.is_some();
            let facts = wsp_engine::addon::RowFacts {
                held_by: Some(h.user),
                held_on: h.machine.map(|(id, name)| if name.is_empty() { id } else { name }),
                held_until: Some(h.expires),
                held_here: here,
                head_only: false,
            };
            (row, facts)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holders_a_take_is_kept_with_its_machine_and_a_release_or_expiry_forgets_it() {
        let (_env, store) = wsp_engine::testkit::isolated_store("hub-holders");
        let took = |row: &str, who: &str, machine: Option<(&str, &str)>| {
            let mut p = json!({ "workspace": "shared:team", "row": row, "holder": { "person": who }, "token": 3, "expires": 1_900_000_000_000i64, "cause": "take" });
            if let Some((id, name)) = machine {
                p["machine"] = json!({ "id": id, "name": name });
            }
            follow(&store, EventKind::LeaseTaken, &p);
        };
        took("wsp-1", "bo", Some(("m-b", "box-b")));
        took("wsp-2", "ann", Some(("m-a", "box-a")));
        took("wsp-3", "ann", Some(("m-other", "laptop")));
        took("wsp-4", "cy", None);
        follow(&store, EventKind::RefMoved, &json!({ "row": "wsp-5" }));
        let f = facts(&store, Some("ann"), Some("m-a"), 1_800_000_000);
        assert_eq!(f.len(), 4);
        assert_eq!((f["wsp-1"].held_by.as_deref(), f["wsp-1"].held_on.as_deref(), f["wsp-1"].held_until, f["wsp-1"].held_here), (Some("bo"), Some("box-b"), Some(1_900_000_000), false));
        assert!(f["wsp-2"].held_here, "ann on this machine");
        assert_eq!((f["wsp-3"].held_by.as_deref(), f["wsp-3"].held_on.as_deref(), f["wsp-3"].held_here), (Some("ann"), Some("laptop"), false), "ann's other machine");
        assert_eq!(f["wsp-4"].held_on, None, "a hub that names no machine");
        follow(&store, EventKind::LeaseReleased, &json!({ "row": "wsp-1", "holder": { "person": "bo" } }));
        follow(&store, EventKind::LeaseExpired, &json!({ "row": "wsp-2", "holder": { "person": "ann" } }));
        let f = facts(&store, Some("ann"), Some("m-a"), 1_800_000_000);
        assert_eq!(f.keys().cloned().collect::<Vec<_>>(), ["wsp-3", "wsp-4"]);
        assert!(facts(&store, Some("ann"), Some("m-a"), 1_950_000_000).is_empty(), "run out by its own expiry");
    }
}
