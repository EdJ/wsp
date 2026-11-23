//! Who may read and write what on the hub (wsp-173): a user reads and writes a
//! shared workspace they are a member of, and nothing else. Admin is no
//! exception for content: an admin who is no member of a workspace reads none
//! of it.
//!
//! - `sync.heads` and `refs.move` name a workspace; one the caller is no member
//!   of (or that is no shared workspace at all) is `forbidden`.
//! - `sync.put` names none, so each object that reads as one is held to its own
//!   workspace, which the caller must be a member of; a caller who is a member
//!   of none puts nothing.
//! - An object id is served (`sync.get`, and as held by `sync.have`) only if it
//!   is one a member workspace references: a head of it, or the parent chain
//!   behind a head, within that workspace. Knowing a hash is no right to its
//!   bytes, so an id that is not served reads as not held: `sync.have` lists it
//!   missing and `sync.get` leaves it out, and a `sync.get` that serves nothing
//!   is `not_found`, whether or not the hub holds what was asked for.
//!
//! [`Hub::answer`] is the verbs through this scope; the unscoped methods of
//! [`Hub`] are the host's own, with no caller.

use std::collections::BTreeSet;

use wsp_api::{Answer, Code, Failure, ObjectId, Outcome, SyncCall, WorkspaceId};
use wsp_store_contract::{Error, Principal, Refusal, SharedName, State, UserName};

use crate::Hub;

/// The shared workspaces a caller is a member of.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scope {
    names: Vec<SharedName>,
}

impl Scope {
    /// What `by` is a member of in `state`; a person who is none's member, and
    /// anyone who is not a person, have an empty scope.
    pub fn of(state: &State, by: &Principal) -> Scope {
        let Principal::Person(user) = by else { return Scope::default() };
        let user = UserName(user.clone());
        let names = state
            .members
            .iter()
            .filter(|(workspace, members)| members.contains(&user) && state.workspaces.contains_key(*workspace))
            .map(|(workspace, _)| workspace.clone())
            .collect();
        Scope { names }
    }

    /// The member workspaces, as the sync verbs name them (`shared:<name>`).
    pub fn workspaces(&self) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.names.iter().map(|n| WorkspaceId(format!("shared:{}", n.0)))
    }

    /// Whether `workspace` is a shared workspace of the scope.
    pub fn allows(&self, workspace: &WorkspaceId) -> bool {
        workspace
            .0
            .strip_prefix("shared:")
            .is_some_and(|name| self.names.iter().any(|n| n.0 == name))
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

fn forbidden(by: &Principal, why: String) -> Failure {
    Failure {
        code: Code::Forbidden,
        text: format!("{}: {why}", wsp_api::principal::text(by)),
        refusal: Some(Refusal::Forbidden { by: by.clone() }),
    }
}

fn not_found(text: String) -> Failure {
    Failure { code: Code::NotFound, text, refusal: None }
}

impl Hub {
    /// One sync verb's outcome for `by`, within the workspaces `by` is a member of.
    pub fn answer(&self, by: &Principal, call: SyncCall) -> Outcome {
        let scope = match self.state() {
            Ok(state) => Scope::of(&state, by),
            Err(Error::Backend { reason }) => {
                return Outcome::Err(Failure { code: Code::Backend, text: reason, refusal: None })
            }
            Err(e) => {
                return Outcome::Err(Failure { code: Code::Backend, text: format!("{e:?}"), refusal: None })
            }
        };
        // A have is answered here: what the scope does not serve is listed missing.
        if let SyncCall::Have { ids } = &call {
            return match self.served(&scope, ids) {
                Ok(served) => Outcome::Ok(Answer::Missing(ids.iter().filter(|id| !served.contains(id)).copied().collect())),
                Err(failure) => Outcome::Err(failure),
            };
        }
        match self.scoped(by, &scope, call) {
            Ok(call) => self.run(call),
            Err(failure) => Outcome::Err(failure),
        }
    }

    /// `call` as the hub runs it, narrowed to what `scope` serves, or why `by`
    /// may not make it.
    fn scoped(&self, by: &Principal, scope: &Scope, call: SyncCall) -> Result<SyncCall, Failure> {
        match call {
            SyncCall::Heads { workspace } => {
                self.member_of(by, scope, &workspace)?;
                Ok(SyncCall::Heads { workspace })
            }
            SyncCall::Move(mv) => {
                self.member_of(by, scope, &mv.workspace)?;
                Ok(SyncCall::Move(mv))
            }
            SyncCall::Put { objects } => {
                if scope.is_empty() {
                    return Err(forbidden(by, "a member of no shared workspace puts nothing".into()));
                }
                for blob in &objects {
                    // One that does not read as an object is the put's to refuse (invalid).
                    if let Ok(object) = wsp_objects::decode(&blob.bytes) {
                        if !scope.allows(object.workspace()) {
                            return Err(forbidden(
                                by,
                                format!("{} belongs to {}, which the caller is not a member of", blob.id, object.workspace().0),
                            ));
                        }
                    }
                }
                Ok(SyncCall::Put { objects })
            }
            SyncCall::Have { ids } => Ok(SyncCall::Have { ids }),
            SyncCall::Get { ids } => {
                let served = self.served(scope, &ids)?;
                let kept: Vec<ObjectId> = ids.iter().filter(|id| served.contains(id)).copied().collect();
                if kept.is_empty() {
                    if let Some(first) = ids.first() {
                        return Err(not_found(format!("{first} is not an object the caller's workspaces hold")));
                    }
                }
                Ok(SyncCall::Get { ids: kept })
            }
        }
    }

    fn member_of(&self, by: &Principal, scope: &Scope, workspace: &WorkspaceId) -> Result<(), Failure> {
        if scope.allows(workspace) {
            return Ok(());
        }
        Err(forbidden(by, format!("not a member of {}", workspace.0)))
    }

    /// Those of `ids` that some workspace of `scope` references.
    fn served(&self, scope: &Scope, ids: &[ObjectId]) -> Result<BTreeSet<ObjectId>, Failure> {
        let mut want: BTreeSet<ObjectId> = ids.iter().copied().collect();
        let mut served = BTreeSet::new();
        let backend = |e: Error| Failure { code: Code::Backend, text: format!("{e:?}"), refusal: None };
        for workspace in scope.workspaces() {
            if want.is_empty() {
                break;
            }
            let refs = self.heads(&workspace).map_err(backend)?;
            let mut seen = BTreeSet::new();
            for head in refs.heads.values() {
                let mut at = Some(*head);
                while let Some(id) = at.take() {
                    if !seen.insert(id) {
                        break;
                    }
                    let Some(bytes) = self.objects().get(&id).map_err(backend)? else { break };
                    let parent = match wsp_objects::decode(&bytes) {
                        // An object of another workspace is none of this one's, whatever moved to it.
                        Ok(object) if object.workspace() != &workspace => break,
                        Ok(object) => object.parent().copied(),
                        // One this build cannot read has no parent to follow.
                        Err(_) => None,
                    };
                    if want.remove(&id) {
                        served.insert(id);
                    }
                    at = parent;
                }
            }
        }
        Ok(served)
    }
}
