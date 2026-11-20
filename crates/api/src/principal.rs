//! A [`Principal`]'s text form (docs/state-core.md 2.1): `person:ed`,
//! `seat:cpd-3698`, `manager:<host>`, `executor:<machine>`, `wsp:<instance>`.
//! The contract serializes a principal in serde's derived form
//! (`{"person":"ed"}`) and leaves this rendering to the API (store-contract
//! `MAP.md`); the envelope's `by` and `hello`'s `by` are written in it.
//!
//! The kind is everything before the first `:`, the name everything after it,
//! so a name may itself hold a `:`. Rendering is total and parsing accepts
//! every rendering: [`parse`] of [`text`] is the principal again. A string with
//! no `:`, an unknown kind or an empty name is a [`Malformed`], never a panic.
//!
//! The module is also a `#[serde(with = "...")]` helper, so a field of type
//! `Principal` is written in the text form.

use serde::{Deserialize, Deserializer, Serializer};
use wsp_store_contract::{MachineName, Principal, SeatId};

/// The kinds of the text form, in the contract's order.
pub const KINDS: &[&str] = &["person", "seat", "manager", "executor", "wsp"];

/// A string that is not a principal's text form, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed {
    pub text: String,
    pub why: &'static str,
}

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not a principal ({}): {:?}", self.why, self.text)
    }
}

impl std::error::Error for Malformed {}

/// The text form of `p`.
pub fn text(p: &Principal) -> String {
    match p {
        Principal::Person(name) => format!("person:{name}"),
        Principal::Seat(SeatId(id)) => format!("seat:{id}"),
        Principal::Manager(MachineName(host)) => format!("manager:{host}"),
        Principal::Executor(MachineName(machine)) => format!("executor:{machine}"),
        Principal::Wsp(instance) => format!("wsp:{instance}"),
    }
}

/// The principal `s` writes, or why it writes none.
pub fn parse(s: &str) -> Result<Principal, Malformed> {
    let bad = |why| Malformed {
        text: s.to_string(),
        why,
    };
    let (kind, name) = s.split_once(':').ok_or_else(|| bad("no `kind:`"))?;
    if name.is_empty() {
        return Err(bad("an empty name"));
    }
    let name = name.to_string();
    Ok(match kind {
        "person" => Principal::Person(name),
        "seat" => Principal::Seat(SeatId(name)),
        "manager" => Principal::Manager(MachineName(name)),
        "executor" => Principal::Executor(MachineName(name)),
        "wsp" => Principal::Wsp(name),
        _ => return Err(bad("an unknown kind")),
    })
}

/// `#[serde(with = "wsp_api::principal")]`: write the text form.
pub fn serialize<S: Serializer>(p: &Principal, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&text(p))
}

/// `#[serde(with = "wsp_api::principal")]`: read the text form.
pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Principal, D::Error> {
    let s = String::deserialize(d)?;
    parse(&s).map_err(serde::de::Error::custom)
}
