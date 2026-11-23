//! The hub's message routing (wsp-470; the multiplayer design's "Message
//! routing and the user primary"): `msg.send`, `msg.take` and `msg.ack`.
//!
//! **Routing.** A spoke sends the hub a tell it cannot deliver on its own
//! machine, addressed as the sender wrote it. The hub resolves the address to
//! a user from what it holds:
//!
//! - `seat:<row>`: the holder of the row's lease (`row:<row>`), unexpired;
//! - `governor:<list>`: the holder of the list's drive lease (`drive:<list>`,
//!   wsp-467), unexpired;
//! - `primary:<user>`: that user, live on the hub.
//!
//! A lease is keyed on its holder's user (wsp-466), so a user is what the hub
//! can route to: the message is queued for the user, and the first of the
//! user's spokes to take and acknowledge it has it (one spoke per user is v1's
//! case). The sender must be a member of the workspace the lease was taken in,
//! or share a workspace with the user whose primary it names: knowing a row's
//! id is no right to reach its holder. A message whose `re` names one the hub
//! delivered to the sender goes back to that one's sender whatever `to` says,
//! so a reply needs no lease. What none of this resolves is refused
//! `not_found`, naming the address.
//!
//! **The queue** is kept as refs are (see the crate's documentation), through
//! the store's revisioned texts: one per user, `hub-inbox:<user>` section
//! `queue`, the user's unacknowledged messages as a JSON array, oldest first,
//! each change a compare-and-swap on its revision. The ids come from one
//! counter (`hub-msg-ids`, section `last`). Each message is also kept whole
//! under `hub-msgs`, section `<id>`, which is what a reply's `re` is read
//! against. `msg.take` reads the queue and changes nothing, so a spoke that
//! took and was cut off before its ack is handed the same messages again;
//! `msg.ack` takes them out, after which no `msg.take` hands them out again.
//!
//! **The stream.** A queued message is announced as the store's `text.set` of
//! `hub-queued:<user>` in a section that is `{ user, id, workspace }` as JSON,
//! which [`crate::HubStore`] reads as `order.queued` on the subject
//! [`wsp_api::msg_subject`] (the user) in `workspace`, a shared workspace the
//! user is a member of, so the server's scope lets it through to them: the
//! recipient's spoke hears that it has mail and takes it, and no subscriber
//! reads its text. A user in no shared workspace is announced nothing, and
//! takes the queue on its next turn. Like
//! a move's, the announcement follows the write and is no part of it: a spoke
//! that missed it takes the queue on its next turn all the same.

use wsp_api::{Answer, Code, Failure, HubMessage, MsgSend, Outcome};
use wsp_store_contract::{Error, IdentityState, Principal, Refusal, Result, RowId, Texts, UserName};

use crate::scope::Scope;
use crate::Hub;

/// The row prefix of a user's queue.
const INBOX_PREFIX: &str = "hub-inbox:";
/// The section of a user's queue.
const QUEUE: &str = "queue";
/// The row every message is kept whole in, one section per id.
const KEPT: &str = "hub-msgs";
/// The row of the message id counter.
const IDS: &str = "hub-msg-ids";
/// The row prefix of a user's announcements.
pub(crate) const QUEUED_PREFIX: &str = "hub-queued:";

/// What a queued message's announcement says: whose it is, its id, and the
/// workspace the stream carries it in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Queued {
    pub user: String,
    pub id: String,
    pub workspace: wsp_api::WorkspaceId,
}

fn inbox_row(user: &str) -> RowId {
    RowId(format!("{INBOX_PREFIX}{user}"))
}

fn failure(code: Code, text: String) -> Failure {
    Failure { code, text, refusal: None }
}

fn backend(e: Error) -> Failure {
    match e {
        Error::Backend { reason } => failure(Code::Backend, reason),
        other => failure(Code::Backend, format!("{other:?}")),
    }
}

impl Hub {
    /// One message verb for `by`: `msg.send`, `msg.take` or `msg.ack`.
    pub(crate) fn answer_message(&self, by: &Principal, scope: &Scope, call: wsp_api::SyncCall) -> Outcome {
        let Principal::Person(me) = by else {
            return Outcome::Err(failure(
                Code::Forbidden,
                format!("{}: only a user sends and takes messages", wsp_api::principal::text(by)),
            ));
        };
        let answered = match call {
            wsp_api::SyncCall::MsgSend(send) => self.send_message(me, scope, send).map(|m| Answer::Queued { id: m.id, user: m.user }),
            wsp_api::SyncCall::MsgTake(_) => self.queued_for(me).map(Answer::Messages).map_err(backend),
            wsp_api::SyncCall::MsgAck { ids } => self.ack_messages(me, &ids).map(|acked| Answer::Acked { acked }).map_err(backend),
            other => Err(failure(Code::Invalid, format!("{} is no message verb", other.verb()))),
        };
        match answered {
            Ok(a) => Outcome::Ok(a),
            Err(f) => Outcome::Err(f),
        }
    }

    /// The user `send` goes to, for `me` within `scope`, or why none.
    pub fn route_message(&self, me: &str, scope: &Scope, send: &MsgSend) -> std::result::Result<String, Failure> {
        let not_found = |why: String| failure(Code::NotFound, format!("{}: {why}", send.to));
        if let Some(re) = &send.re {
            if let Some(first) = self.kept_message(re).map_err(backend)? {
                if first.user == me {
                    return Ok(first.from_user);
                }
            }
        }
        let Some((kind, name)) = send.to.split_once(':') else {
            return Err(not_found("not an address the hub routes (seat:, governor:, primary:)".into()));
        };
        let name = name.trim();
        let leased = |row: RowId, what: &str| -> std::result::Result<String, Failure> {
            let lease = self.row_lease(&row).map_err(backend)?.filter(|l| l.expires > self.clock().now());
            let Some(lease) = lease else {
                return Err(not_found(format!("no spoke holds {what} on the hub")));
            };
            let member = self.lease_workspace(&row).map_err(backend)?.is_some_and(|w| scope.allows(&w));
            if !member {
                return Err(failure(
                    Code::Forbidden,
                    format!("person:{me}: {} is in no workspace the caller is a member of", send.to),
                ));
            }
            match lease.holder {
                Principal::Person(user) => Ok(user),
                other => Err(not_found(format!("held by {}, which is no user", wsp_api::principal::text(&other)))),
            }
        };
        match kind {
            "seat" => leased(RowId(name.to_string()), &format!("{name}'s lease")),
            "governor" => leased(RowId(format!("drive:{name}")), &format!("the drive of {name}")),
            "primary" => {
                let state = self.state().map_err(backend)?;
                let live = state.users.get(&UserName(name.to_string())).is_some_and(|u| u.state == IdentityState::Active);
                if !live {
                    return Err(not_found(format!("no user `{name}` on the hub")));
                }
                let theirs = Scope::of(&state, &Principal::Person(name.to_string()));
                if name != me && !scope.names().iter().any(|n| theirs.names().contains(n)) {
                    return Err(failure(Code::Forbidden, format!("person:{me}: shares no workspace with {name}")));
                }
                Ok(name.to_string())
            }
            _ => Err(not_found("not an address the hub routes (seat:, governor:, primary:)".into())),
        }
    }

    /// `msg.send` for `me`: routed, kept, queued for its user and announced.
    pub fn send_message(&self, me: &str, scope: &Scope, send: MsgSend) -> std::result::Result<HubMessage, Failure> {
        let user = self.route_message(me, scope, &send)?;
        let id = self.next_message_id().map_err(backend)?;
        let msg = HubMessage {
            id,
            to: send.to,
            user,
            from_user: me.to_string(),
            from: send.from,
            seat: send.seat,
            text: send.text,
            re: send.re,
            at: self.clock().now(),
        };
        self.keep_message(&msg).map_err(backend)?;
        self.change_queue(&msg.user, |q| {
            q.push(msg.clone());
            true
        })
        .map_err(backend)?;
        if let Ok(state) = self.state() {
            if let Some(workspace) = Scope::of(&state, &Principal::Person(msg.user.clone())).workspaces().next() {
                self.announce_queued(&Queued { user: msg.user.clone(), id: msg.id.clone(), workspace });
            }
        }
        Ok(msg)
    }

    /// The messages queued for `user`, oldest first: every one not acknowledged.
    pub fn queued_for(&self, user: &str) -> Result<Vec<HubMessage>> {
        Ok(self.queue_at(user)?.0)
    }

    /// `msg.ack` for `user`: those of `ids` in its queue leave it; how many did.
    pub fn ack_messages(&self, user: &str, ids: &[String]) -> Result<u32> {
        let mut acked = 0;
        self.change_queue(user, |q| {
            let before = q.len();
            q.retain(|m| !ids.contains(&m.id));
            acked = (before - q.len()) as u32;
            acked > 0
        })?;
        Ok(acked)
    }

    fn queue_at(&self, user: &str) -> Result<(Vec<HubMessage>, wsp_store_contract::Rev)> {
        let (body, rev) = self.db.text(&inbox_row(user), QUEUE)?;
        if body.trim().is_empty() {
            return Ok((Vec::new(), rev));
        }
        let queue = serde_json::from_str(&body)
            .map_err(|e| Error::Backend { reason: format!("the queue of {user}: {e}") })?;
        Ok((queue, rev))
    }

    /// `user`'s queue changed by `change` (which says whether it changed it),
    /// written expecting the revision read; read again on a conflict.
    fn change_queue(&self, user: &str, mut change: impl FnMut(&mut Vec<HubMessage>) -> bool) -> Result<()> {
        loop {
            let (mut queue, rev) = self.queue_at(user)?;
            if !change(&mut queue) {
                return Ok(());
            }
            let body = serde_json::to_string(&queue).map_err(|e| Error::Backend { reason: e.to_string() })?;
            match self.db.set_text(&inbox_row(user), QUEUE, body, rev) {
                Err(Error::Refused(Refusal::Conflict { .. })) => continue,
                other => return other.map(|_| ()),
            }
        }
    }

    fn next_message_id(&self) -> Result<String> {
        let row = RowId(IDS.into());
        loop {
            let (body, rev) = self.db.text(&row, "last")?;
            let last: u64 = body.trim().parse().unwrap_or(0);
            let next = last + 1;
            match self.db.set_text(&row, "last", next.to_string(), rev) {
                Ok(_) => return Ok(format!("hm-{next}")),
                Err(Error::Refused(Refusal::Conflict { .. })) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn keep_message(&self, msg: &HubMessage) -> Result<()> {
        let row = RowId(KEPT.into());
        let body = serde_json::to_string(msg).map_err(|e| Error::Backend { reason: e.to_string() })?;
        loop {
            let (_, rev) = self.db.text(&row, &msg.id)?;
            match self.db.set_text(&row, &msg.id, body.clone(), rev) {
                Err(Error::Refused(Refusal::Conflict { .. })) => continue,
                other => return other.map(|_| ()),
            }
        }
    }

    /// A message the hub routed, by id, acknowledged or not.
    pub fn kept_message(&self, id: &str) -> Result<Option<HubMessage>> {
        let (body, _) = self.db.text(&RowId(KEPT.into()), id)?;
        if body.trim().is_empty() {
            return Ok(None);
        }
        Ok(serde_json::from_str(&body).ok())
    }

    /// The announcement of a queued message: see the module.
    fn announce_queued(&self, queued: &Queued) {
        let Ok(section) = serde_json::to_string(queued) else { return };
        let row = RowId(format!("{QUEUED_PREFIX}{}", queued.user));
        for _ in 0..64 {
            let Ok((_, rev)) = self.db.text(&row, &section) else { return };
            match self.db.set_text(&row, &section, String::new(), rev) {
                Err(Error::Refused(Refusal::Conflict { .. })) => continue,
                _ => return,
            }
        }
    }
}
