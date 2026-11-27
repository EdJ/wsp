//! `wsp-objects`: the hub's object format (wsp-458; the design's "Hash-based
//! sync, and why it scales", decisions d5 and d6).
//!
//! An [`Object`] is one version of an entity (a [`Object::Record`]) or one thing
//! that happened (an [`Object::Event`]). It is named by its [`ObjectId`], the
//! SHA-256 of its canonical bytes, so it never changes: there is no update API,
//! and a change is a new object whose `parent` is the old one. Because an id
//! names exactly one sequence of bytes, objects can be cached anywhere, stored
//! once however many spokes send them, and served by any front end without
//! asking anybody.
//!
//! The only state that does change is [`Refs`]: per workspace, which object is
//! each subject's head. Two sides sync by comparing their refs with [`diff`]
//! and moving only the objects behind the subjects that differ. Refs are data
//! here; whoever holds them stores them (the hub's store is wsp-460, a spoke's
//! wsp-464 and wsp-469).
//!
//! Pure: no IO, no clock. A moment and a principal are the caller's to give.
//!
//! # Canonical bytes
//!
//! An object's bytes are its serde form (the contract's derived form, as
//! `{"record": {...}}`) written as canonical JSON: UTF-8, no insignificant
//! whitespace, every object's keys sorted by their UTF-8 bytes, every field
//! present (an absent `parent` is `null`). Strings and numbers are written as
//! `serde_json` writes them: strings escape only `"`, `\` and control
//! characters, and numbers in their shortest form that reads back the same, so
//! a canonical text read and written again is the same text. The sort is done
//! here rather than left to `serde_json`'s map, whose order a feature elsewhere
//! in the build (`preserve_order`) would change.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use wsp_store_contract::{At, Entity, EventKind, Principal, Subject};

/// A workspace: the scope an object belongs to and refs are kept for.
///
/// A plain name until wsp-168 lands the contract's `WorkspaceId` (`private` or
/// `shared:<name>`); this newtype is then replaced by that one. Its serde form
/// is the bare string, as that one's text form is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(pub String);

/// The text form's prefix: `sha256:<64 lowercase hex digits>`.
const PREFIX: &str = "sha256:";

/// An object's id: the SHA-256 of its canonical bytes. Written `sha256:<hex>`,
/// in text and in serde alike.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    /// The id of these bytes: their SHA-256. Says nothing of whether they are
    /// an object; [`verify`] does.
    pub fn of(bytes: &[u8]) -> ObjectId {
        ObjectId(Sha256::digest(bytes).into())
    }

    /// The digest itself.
    pub fn digest(&self) -> &[u8; 32] {
        &self.0
    }

    /// The digest as 64 lowercase hex digits, without the `sha256:` prefix.
    pub fn hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(DIGITS[(b >> 4) as usize] as char);
            s.push(DIGITS[(b & 0xf) as usize] as char);
        }
        s
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", self.hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for ObjectId {
    type Err = Error;

    /// Reads `sha256:<hex>`, lowercase only, so an id has one text form.
    fn from_str(s: &str) -> Result<ObjectId> {
        let bad = || Error::BadId(s.to_string());
        let hex = s.strip_prefix(PREFIX).ok_or_else(bad)?.as_bytes();
        if hex.len() != 64 {
            return Err(bad());
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut digest = [0u8; 32];
        for (i, pair) in hex.chunks(2).enumerate() {
            digest[i] =
                (nibble(pair[0]).ok_or_else(bad)? << 4) | nibble(pair[1]).ok_or_else(bad)?;
        }
        Ok(ObjectId(digest))
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<ObjectId, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// An object: immutable, named by its [`ObjectId`]. Serialized in serde's
/// derived form, as the contract's enums are (`{"record": {...}}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Object {
    /// One version of an entity: a row, a list, a seat, ... whole. A project's
    /// record waits for the contract to carry a project [`Entity`].
    Record {
        workspace: WorkspaceId,
        subject: Subject,
        entity: Entity,
        /// The version this one follows; none for the first.
        parent: Option<ObjectId>,
        by: Principal,
        at: At,
    },
    /// One thing that happened to a subject, as the contract's
    /// [`Event`](wsp_store_contract::Event) says it, without the store's number.
    Event {
        workspace: WorkspaceId,
        subject: Subject,
        kind: EventKind,
        /// What the kind carries beyond its subject; `null` when nothing.
        payload: Value,
        /// The object this one follows; none for the first.
        parent: Option<ObjectId>,
        by: Principal,
        at: At,
    },
}

impl Object {
    pub fn workspace(&self) -> &WorkspaceId {
        match self {
            Object::Record { workspace, .. } | Object::Event { workspace, .. } => workspace,
        }
    }

    pub fn subject(&self) -> &Subject {
        match self {
            Object::Record { subject, .. } | Object::Event { subject, .. } => subject,
        }
    }

    pub fn parent(&self) -> Option<&ObjectId> {
        match self {
            Object::Record { parent, .. } | Object::Event { parent, .. } => parent.as_ref(),
        }
    }

    pub fn by(&self) -> &Principal {
        match self {
            Object::Record { by, .. } | Object::Event { by, .. } => by,
        }
    }

    pub fn at(&self) -> At {
        match self {
            Object::Record { at, .. } | Object::Event { at, .. } => *at,
        }
    }
}

/// An object's canonical bytes and the id they hash to.
pub fn encode(object: &Object) -> (ObjectId, Vec<u8>) {
    // Every map an object holds is keyed by strings, so its serde form is
    // always a JSON value.
    let value = serde_json::to_value(object).expect("an object's serde form is JSON");
    let mut bytes = Vec::new();
    write_canonical(&value, &mut bytes);
    (ObjectId::of(&bytes), bytes)
}

/// Reads an object from JSON. Any JSON of the right shape is read, canonical or
/// not; [`verify`] is what holds bytes to their id.
///
/// A field or a kind this build does not know, from a newer writer, is read as
/// the contract reads it (dropped, or `EventKind::Unknown`), so the object read
/// may not encode back to the bytes it came from. Keep the bytes, not the
/// object, when it is the bytes you will hand on.
pub fn decode(bytes: &[u8]) -> Result<Object> {
    serde_json::from_slice(bytes).map_err(|e| Error::Malformed(e.to_string()))
}

/// Holds bytes to an id: refused unless they hash to it and are canonical JSON.
///
/// It does not read them as an [`Object`], so a store of an older build keeps
/// and serves objects of kinds it does not know; [`decode`] is for whoever
/// reads them. The canonical check means an object has exactly one id: the
/// same object sent with its keys in another order is refused, not stored
/// twice under two names.
pub fn verify(id: &ObjectId, bytes: &[u8]) -> Result<()> {
    let actual = ObjectId::of(bytes);
    if actual != *id {
        return Err(Error::Mismatch { id: *id, actual });
    }
    if canonical(bytes)? != bytes {
        return Err(Error::NotCanonical);
    }
    Ok(())
}

/// Any JSON text, written canonically (see the crate's documentation).
pub fn canonical(json: &[u8]) -> Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(json).map_err(|e| Error::Malformed(e.to_string()))?;
    let mut out = Vec::with_capacity(json.len());
    write_canonical(&value, &mut out);
    Ok(out)
}

fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            out.push(b'{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_scalar(&Value::String(key.clone()), out);
                out.push(b':');
                write_canonical(item, out);
            }
            out.push(b'}');
        }
        scalar => write_scalar(scalar, out),
    }
}

fn write_scalar(value: &Value, out: &mut Vec<u8>) {
    // Writing a scalar into a Vec cannot fail.
    serde_json::to_writer(&mut *out, value).expect("a JSON scalar writes");
}

/// Each subject's head, in one workspace: which object is its latest.
///
/// Data, not a store: whoever holds the refs stores them, and moving a head is
/// theirs to fence. Serialized as `{"workspace": ..., "heads": [{"subject": ...,
/// "head": "sha256:..."}, ...]}` in subject order, since a subject is not a
/// string and so cannot key a JSON map.
///
/// **Retired subjects.** A subject its workspace no longer has (a row moved
/// to a new id, say) is in `retired`, with the subject it continues as where
/// there is one, and in no head. Written after the heads as `"retired":
/// [{"subject": ..., "successor": ...}, ...]`, `successor` left out where
/// there is none, and the whole key left out where nothing is retired, so
/// refs with none are the bytes they always were; a reader that does not know
/// the key passes it over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "RefsForm", try_from = "RefsForm")]
pub struct Refs {
    pub workspace: WorkspaceId,
    pub heads: BTreeMap<Subject, ObjectId>,
    /// The retired subjects, each with the subject it continues as (none:
    /// it is gone, and continues as nothing).
    pub retired: BTreeMap<Subject, Option<Subject>>,
}

impl Refs {
    /// No heads yet.
    pub fn new(workspace: WorkspaceId) -> Refs {
        Refs {
            workspace,
            heads: BTreeMap::new(),
            retired: BTreeMap::new(),
        }
    }

    pub fn get(&self, subject: &Subject) -> Option<&ObjectId> {
        self.heads.get(subject)
    }

    /// Moves a subject's head; returns the one it replaced.
    pub fn set(&mut self, subject: Subject, head: ObjectId) -> Option<ObjectId> {
        self.heads.insert(subject, head)
    }
}

#[derive(Serialize, Deserialize)]
struct RefsForm {
    workspace: WorkspaceId,
    heads: Vec<Head>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired: Vec<Retired>,
}

#[derive(Serialize, Deserialize)]
struct Retired {
    subject: Subject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    successor: Option<Subject>,
}

#[derive(Serialize, Deserialize)]
struct Head {
    subject: Subject,
    head: ObjectId,
}

impl From<Refs> for RefsForm {
    fn from(refs: Refs) -> RefsForm {
        RefsForm {
            workspace: refs.workspace,
            heads: refs
                .heads
                .into_iter()
                .map(|(subject, head)| Head { subject, head })
                .collect(),
            retired: refs
                .retired
                .into_iter()
                .map(|(subject, successor)| Retired { subject, successor })
                .collect(),
        }
    }
}

impl TryFrom<RefsForm> for Refs {
    type Error = Error;

    /// Refused when a subject is named twice: which head would it have? A
    /// subject both with a head and retired is refused too.
    fn try_from(form: RefsForm) -> Result<Refs> {
        let mut refs = Refs::new(form.workspace);
        for Head { subject, head } in form.heads {
            if refs.heads.contains_key(&subject) {
                return Err(Error::Malformed(format!(
                    "subject {subject:?} has two heads"
                )));
            }
            refs.heads.insert(subject, head);
        }
        for Retired { subject, successor } in form.retired {
            if refs.heads.contains_key(&subject) || refs.retired.contains_key(&subject) {
                return Err(Error::Malformed(format!(
                    "subject {subject:?} is retired and has a head, or is retired twice"
                )));
            }
            refs.retired.insert(subject, successor);
        }
        Ok(refs)
    }
}

/// How two sides' refs part: the subjects only they have a head for, those
/// only we have, and those both have with different heads. Which side is ahead
/// on a differing subject is the objects' parents' to say, not the refs'.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub we_lack: BTreeSet<Subject>,
    pub they_lack: BTreeSet<Subject>,
    pub differ: BTreeSet<Subject>,
}

impl Diff {
    /// The two sides agree on every head.
    pub fn is_empty(&self) -> bool {
        self.we_lack.is_empty() && self.they_lack.is_empty() && self.differ.is_empty()
    }
}

/// Compares our refs with theirs, subject by subject. Refused when they are
/// refs of two workspaces: nothing syncs across workspaces.
pub fn diff(ours: &Refs, theirs: &Refs) -> Result<Diff> {
    if ours.workspace != theirs.workspace {
        return Err(Error::OtherWorkspace {
            ours: ours.workspace.clone(),
            theirs: theirs.workspace.clone(),
        });
    }
    let mut out = Diff::default();
    for (subject, head) in &ours.heads {
        match theirs.heads.get(subject) {
            None => {
                out.they_lack.insert(subject.clone());
            }
            Some(other) if other != head => {
                out.differ.insert(subject.clone());
            }
            Some(_) => {}
        }
    }
    for subject in theirs.heads.keys() {
        if !ours.heads.contains_key(subject) {
            out.we_lack.insert(subject.clone());
        }
    }
    Ok(out)
}

/// Why bytes, an id or refs were refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not JSON, or not of the shape asked for.
    Malformed(String),
    /// The bytes hash to `actual`, not to the `id` they were offered under.
    Mismatch { id: ObjectId, actual: ObjectId },
    /// The bytes hash to their id but are not canonical JSON, so the object
    /// they hold has another id.
    NotCanonical,
    /// Not `sha256:<64 lowercase hex digits>`.
    BadId(String),
    /// [`diff`] was given refs of two workspaces.
    OtherWorkspace {
        ours: WorkspaceId,
        theirs: WorkspaceId,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed(why) => write!(f, "not an object: {why}"),
            Error::Mismatch { id, actual } => write!(f, "bytes offered as {id} hash to {actual}"),
            Error::NotCanonical => f.write_str("bytes are not canonical JSON"),
            Error::BadId(s) => write!(f, "not an object id: {s:?}"),
            Error::OtherWorkspace { ours, theirs } => {
                write!(
                    f,
                    "refs of workspace {:?} compared with {:?}",
                    ours.0, theirs.0
                )
            }
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
