//! The hub (wsp-460; the multiplayer design's "Version 1"): objects, refs and
//! leases for the `wsp-hub` binary, which hosts no agents, runs no builds and
//! holds no worktree. [`serve`] puts the store on wsp-server's carrier
//! ([`Config`] is the file `wsp-hub serve --config` reads); [`HubStore`] is
//! what it serves, whose event log announces each move as `ref.moved`.
//!
//! A data directory holds two things:
//!
//! - `objects/`, an [`ObjectDir`]: each object's bytes in a file named by its
//!   hash, written through a temp file and a rename, never rewritten.
//! - `hub.db`, one small SQLite opened as a [`SqliteStore`], so the hub's
//!   leases are wsp-store-sqlite's own (its `Leases`, and the fencing tokens
//!   that come with them), not a copy. Its domain is wsp-159's [`Identity`],
//!   so the hub's users and token records live there too, and [`auth`] reads
//!   them to say who a hello is (wsp-161). The message queue joins it later.
//!
//! Refs are kept in that file too, one workspace's [`Refs`] to a revisioned
//! text (row `hub-refs:<workspace>`, section `heads`). `rusqlite` is
//! wsp-store-sqlite's alone (D2, tests/layering.rs), so the hub writes through
//! the store's surface: [`Hub::move_ref`] reads the refs and their revision,
//! compares the subject's head with the move's `from`, and writes the moved
//! refs with `set_text` expecting that revision. The revision check and the
//! write are one `BEGIN IMMEDIATE` transaction, so of two moves from one head
//! exactly one is written; the other's write is refused `Conflict`, and it is
//! read again and answered stale with the winner's head. (A move of another
//! subject in the same workspace also bumps the revision; the loser of that
//! race reads again and, its own subject's head being still `from`, moves.)
//!
//! [`Hub::answer`] gives each of wsp-459's sync verbs the meaning wsp-client's
//! `MemHub` gives it (crates/client/src/testing.rs), stale moves included: an
//! `ok` answer, not an `err`.

pub mod auth;
mod config;
mod objects;
pub mod serve;
mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use wsp_api::{
    Answer, Blob, Code, Failure, Move, ObjectId, Outcome, RefMove, Refs, SyncCall, WorkspaceId,
};
use wsp_machines::Identity;
use wsp_store_contract::{
    Applied, Command, Commands, Domain, Draft, Error, Event, IdempotencyKey, LeaseName, Leases,
    Principal, Reads, Refusal, Result, Rev, RowId, State, Subject, Texts, Token,
};
use wsp_store_sqlite::SqliteStore;

pub use config::{Config, DEFAULT_PORT};
pub use objects::ObjectDir;
pub use store::HubStore;

/// The section of a workspace's refs text.
const HEADS: &str = "heads";

/// The row prefix of a workspace's announcements: one section per moved
/// subject (its JSON), written empty after each move, so that the store's
/// `text.set` for it says which subject moved ([`HubStore`] reads it as a
/// `ref.moved`).
pub(crate) const MOVED_PREFIX: &str = "hub-moved:";

/// The hub's store on one data directory.
#[derive(Debug)]
pub struct Hub {
    dir: PathBuf,
    objects: ObjectDir,
    pub(crate) db: SqliteStore,
}

/// The domain of the hub's SQLite: the identity machine's commands (the
/// `hub.user.*` and `hub.token.*` verbs, shared workspaces and members),
/// decided and folded by [`Identity`]; no other command. The events the store
/// writes itself (`lease.*`, `text.set`) change no state, since the rest of
/// the hub's state is its refs and leases, not wsp's rows.
struct HubDomain;

impl Domain for HubDomain {
    fn decide(
        &self,
        state: &State,
        cmd: &Command,
        by: &Principal,
    ) -> std::result::Result<Vec<Draft>, Refusal> {
        if Identity::owns(cmd) {
            return Identity.decide(state, cmd, by);
        }
        Err(Refusal::Invalid {
            reason: "the hub's store takes only the identity machine's commands".into(),
        })
    }

    fn evolve(&self, state: State, event: &Event) -> State {
        Identity.evolve(state, event)
    }
}

fn refs_row(workspace: &WorkspaceId) -> RowId {
    RowId(format!("hub-refs:{}", workspace.0))
}

fn backend(e: impl std::fmt::Display) -> Error {
    Error::Backend {
        reason: e.to_string(),
    }
}

impl Hub {
    /// Opens (creating, if absent) the hub's store in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Hub> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)
            .map_err(|e| backend(format!("creating {}: {e}", dir.display())))?;
        let objects = ObjectDir::open(dir.join("objects"))?;
        let db = SqliteStore::open(dir.join("hub.db"), Arc::new(HubDomain))?;
        Ok(Hub { dir, objects, db })
    }

    /// The data directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The object directory.
    pub fn objects(&self) -> &ObjectDir {
        &self.objects
    }

    // Identity.

    /// Applies an identity command in process as `by`, under `key`: what the
    /// hub's host does without a connection (the first admin's token, which
    /// no connection can issue before that admin has one). The identity
    /// machine decides it as it decides one sent over the API.
    pub fn apply(&self, cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Applied> {
        self.db.apply(cmd, key, by)
    }

    /// The hub's state: its users, token records, shared workspaces and members.
    pub fn state(&self) -> Result<State> {
        Ok(self.db.snapshot()?.0)
    }

    // Objects.

    /// Those of `ids` not held, in the order asked (`sync.have`).
    pub fn missing(&self, ids: &[ObjectId]) -> Vec<ObjectId> {
        ids.iter()
            .filter(|id| !self.objects.has(id))
            .copied()
            .collect()
    }

    /// Keeps `objects` (`sync.put`): every one is held to its id before any is
    /// kept, and one already held is not written again. Answers how many were
    /// written; one that fails the check refuses the whole put `Invalid`,
    /// naming it.
    pub fn put(&self, objects: &[Blob]) -> Result<u32> {
        for blob in objects {
            wsp_objects::verify(&blob.id, &blob.bytes).map_err(|e| {
                Error::Refused(Refusal::Invalid {
                    reason: format!("{}: {e}", blob.id),
                })
            })?;
        }
        let mut stored = 0;
        for blob in objects {
            if self.objects.put(&blob.id, &blob.bytes)? {
                stored += 1;
            }
        }
        Ok(stored)
    }

    /// Those of `ids` held, with their bytes as they were put (`sync.get`);
    /// one not held is left out.
    pub fn get(&self, ids: &[ObjectId]) -> Result<Vec<Blob>> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(bytes) = self.objects.get(id)? {
                out.push(Blob { id: *id, bytes });
            }
        }
        Ok(out)
    }

    // Refs.

    /// `workspace`'s refs and the revision they were read at.
    fn refs_at(&self, workspace: &WorkspaceId) -> Result<(Refs, Rev)> {
        let (body, rev) = self.db.text(&refs_row(workspace), HEADS)?;
        if body.is_empty() {
            return Ok((Refs::new(workspace.clone()), rev));
        }
        let refs: Refs = serde_json::from_str(&body)
            .map_err(|e| backend(format!("the refs of {}: {e}", workspace.0)))?;
        Ok((refs, rev))
    }

    /// `workspace`'s refs (`sync.heads`); none for one never moved.
    pub fn heads(&self, workspace: &WorkspaceId) -> Result<Refs> {
        Ok(self.refs_at(workspace)?.0)
    }

    /// Moves a subject's head from the one the writer saw (`refs.move`):
    /// [`Move::Moved`] when it was `from`, [`Move::Stale`] with the head it has
    /// when not. A compare-and-swap; see the crate's documentation.
    pub fn move_ref(&self, mv: &RefMove) -> Result<Move> {
        let row = refs_row(&mv.workspace);
        loop {
            let (mut refs, rev) = self.refs_at(&mv.workspace)?;
            let head = refs.get(&mv.subject).copied();
            if head != mv.from {
                return Ok(Move::Stale { head });
            }
            refs.set(mv.subject.clone(), mv.to);
            let body = serde_json::to_string(&refs).map_err(backend)?;
            match self.db.set_text(&row, HEADS, body, rev) {
                Ok(_) => {
                    self.announce(&mv.workspace, &mv.subject);
                    return Ok(Move::Moved { head: mv.to });
                }
                // Another move wrote these refs since they were read: read again.
                Err(Error::Refused(Refusal::Conflict { .. })) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Writes the announcement of a move: the store's `text.set` for the
    /// workspace's announcement row, in the section named by the subject. It
    /// follows the move's own write and is no part of it, so a crash between
    /// the two loses the announcement and keeps the move: the refs are the
    /// truth, an announcement a hint to fetch them, and a subscriber that
    /// reconnects syncs from them whatever it heard.
    fn announce(&self, workspace: &WorkspaceId, subject: &Subject) {
        let Ok(section) = serde_json::to_string(subject) else { return };
        let row = RowId(format!("{MOVED_PREFIX}{}", workspace.0));
        // Another announcement of this row may land first: write on the revision found.
        for _ in 0..64 {
            let Ok((_, rev)) = self.db.text(&row, &section) else { return };
            match self.db.set_text(&row, &section, String::new(), rev) {
                Err(Error::Refused(Refusal::Conflict { .. })) => continue,
                _ => return,
            }
        }
    }

    // Leases: wsp-store-sqlite's, with its tokens.

    /// The lease table, as wsp-store-sqlite keeps it: `take` issues a token
    /// strictly greater than every one before it, and `renew` and `release`
    /// act only for the current token.
    pub fn leases(&self) -> &dyn Leases {
        &self.db
    }

    /// Takes `name` for `holder` for `ttl`, if no one holds it unexpired.
    pub fn take_lease(
        &self,
        name: &LeaseName,
        holder: &Principal,
        ttl: Duration,
    ) -> Result<Option<Token>> {
        self.db.take(name, holder, ttl)
    }

    // The verbs.

    /// One sync verb's outcome, as wsp-client's `MemHub` answers it.
    pub fn answer(&self, call: SyncCall) -> Outcome {
        let answered = match call {
            SyncCall::Heads { workspace } => self.heads(&workspace).map(Answer::Heads),
            SyncCall::Have { ids } => Ok(Answer::Missing(self.missing(&ids))),
            SyncCall::Put { objects } => self.put(&objects).map(|stored| Answer::Stored { stored }),
            SyncCall::Get { ids } => self.get(&ids).map(Answer::Objects),
            SyncCall::Move(mv) => self.move_ref(&mv).map(Answer::Move),
        };
        match answered {
            Ok(answer) => Outcome::Ok(answer),
            Err(Error::Refused(Refusal::Invalid { reason })) => Outcome::Err(Failure {
                code: Code::Invalid,
                text: reason,
                refusal: None,
            }),
            Err(Error::Refused(refusal)) => Outcome::Err(Failure {
                code: Code::Invalid,
                text: format!("{refusal:?}"),
                refusal: Some(refusal),
            }),
            Err(Error::Backend { reason }) => Outcome::Err(Failure {
                code: Code::Backend,
                text: reason,
                refusal: None,
            }),
        }
    }
}
