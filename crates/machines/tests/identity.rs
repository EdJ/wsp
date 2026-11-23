//! The identity machine (wsp-159): who may, the rules, and the fold, through
//! `Log`, a model of a backend's `apply` (decide, stamp, fold).

use std::result::Result;
use wsp_machines::{token, Identity};
use wsp_store_contract::*;

struct Log {
    state: State,
    n: u64,
}

impl Log {
    fn new() -> Self {
        Log { state: State::default(), n: 0 }
    }

    fn run(&mut self, cmd: Command, by: &Principal) -> Result<Vec<Event>, Refusal> {
        let drafts = Identity.decide(&self.state, &cmd, by)?;
        let mut out = vec![];
        for d in drafts {
            self.n += 1;
            let e = Event {
                id: EventId(self.n),
                at: At(1000 + self.n),
                principal: by.clone(),
                kind: d.kind,
                subject: d.subject,
                workspace: d.workspace,
                payload: d.payload,
            };
            self.state = Identity.evolve(std::mem::take(&mut self.state), &e);
            out.push(e);
        }
        Ok(out)
    }
}

fn hub() -> Principal {
    Principal::Wsp("hub".into())
}
fn person(n: &str) -> Principal {
    Principal::Person(n.into())
}
fn u(n: &str) -> UserName {
    UserName(n.into())
}
fn add(n: &str, admin: bool) -> Command {
    Command::HubUserAdd { name: u(n), admin }
}
fn issue(id: &str, user: &str, hash: &str) -> Command {
    Command::HubTokenIssue { id: TokenId(id.into()), user: u(user), hash: hash.into() }
}
fn revoke_token(id: &str) -> Command {
    Command::HubTokenRevoke { id: TokenId(id.into()) }
}

/// A hub with admin `ed` (made by the bootstrap) and member `sam`.
fn world() -> Log {
    let mut l = Log::new();
    l.run(add("ed", true), &hub()).unwrap();
    l.run(add("sam", false), &person("ed")).unwrap();
    l
}

fn all() -> Vec<Command> {
    vec![
        add("x", false),
        Command::HubUserRevoke { name: u("sam") },
        issue("t9", "sam", &token::hash("x")),
        revoke_token("t1"),
    ]
}

#[test]
fn an_admin_user_may_every_hub_command() {
    let mut l = world();
    l.run(add("kim", false), &person("ed")).unwrap();
    let h = token::hash("a");
    let ev = l.run(issue("t1", "sam", &h), &person("ed")).unwrap();
    assert_eq!(ev[0].kind, EventKind::HubTokenIssued);
    assert_eq!(l.state.credentials[&TokenId("t1".into())].hash, h);
    assert_eq!(l.state.credentials[&TokenId("t1".into())].issued, At(ev[0].at.0));
    l.run(revoke_token("t1"), &person("ed")).unwrap();
    assert_eq!(l.state.credentials[&TokenId("t1".into())].state, IdentityState::Revoked);
    assert!(l.state.credentials[&TokenId("t1".into())].revoked.is_some());
    l.run(Command::HubUserRevoke { name: u("kim") }, &person("ed")).unwrap();
    assert_eq!(l.state.users[&u("kim")].state, IdentityState::Revoked);
}

#[test]
fn anyone_else_is_forbidden_and_nothing_is_written() {
    let mut l = world();
    let others = [person("sam"), person("nobody"), hub(), Principal::Seat(SeatId("cpd-1".into())), Principal::Manager(MachineName("m".into()))];
    for by in others {
        for cmd in all() {
            let before = l.state.clone();
            assert_eq!(l.run(cmd.clone(), &by), Err(Refusal::Forbidden { by: by.clone() }), "{cmd:?} by {by:?}");
            assert_eq!(l.state, before);
        }
    }
    assert_eq!(l.n, 2);
}

#[test]
fn a_revoked_admin_may_no_more() {
    let mut l = world();
    l.run(add("kim", true), &person("ed")).unwrap();
    l.run(Command::HubUserRevoke { name: u("ed") }, &person("kim")).unwrap();
    assert_eq!(l.run(add("z", false), &person("ed")), Err(Refusal::Forbidden { by: person("ed") }));
}

#[test]
fn the_bootstrap_adds_an_admin_only_while_none_exists() {
    let mut l = Log::new();
    assert_eq!(l.run(add("sam", false), &hub()), Err(Refusal::Forbidden { by: hub() }), "not a non-admin");
    assert_eq!(
        l.run(issue("t1", "sam", &token::hash("a")), &hub()),
        Err(Refusal::Forbidden { by: hub() }),
        "not another verb"
    );
    assert_eq!(l.run(add("ed", true), &person("ed")), Err(Refusal::Forbidden { by: person("ed") }), "not a person");
    let ev = l.run(add("ed", true), &hub()).unwrap();
    assert_eq!(ev[0].kind, EventKind::HubUserAdded);
    assert!(l.state.users[&u("ed")].admin);
    assert_eq!(l.run(add("kim", true), &hub()), Err(Refusal::Forbidden { by: hub() }), "an admin exists");
    // Once every admin is revoked, the bootstrap may make one again.
    l.run(Command::HubUserRevoke { name: u("ed") }, &person("ed")).unwrap();
    l.run(add("kim", true), &hub()).unwrap();
}

#[test]
fn a_name_is_unique_and_never_reused_after_revocation() {
    let mut l = world();
    let dup = l.run(add("sam", false), &person("ed"));
    assert!(matches!(dup, Err(Refusal::Invalid { .. })), "{dup:?}");
    l.run(Command::HubUserRevoke { name: u("sam") }, &person("ed")).unwrap();
    let again = l.run(add("sam", false), &person("ed"));
    assert!(matches!(again, Err(Refusal::Invalid { .. })), "{again:?}");
    assert!(matches!(l.run(add(" ", false), &person("ed")), Err(Refusal::Invalid { .. })));
}

#[test]
fn revoking_a_user_revokes_their_tokens_in_one_decision() {
    let mut l = world();
    l.run(add("kim", false), &person("ed")).unwrap();
    for (id, user) in [("t1", "sam"), ("t2", "sam"), ("t3", "kim")] {
        l.run(issue(id, user, &token::hash(id)), &person("ed")).unwrap();
    }
    l.run(revoke_token("t2"), &person("ed")).unwrap();
    let ev = l.run(Command::HubUserRevoke { name: u("sam") }, &person("ed")).unwrap();
    let kinds: Vec<_> = ev.iter().map(|e| (e.kind.clone(), e.subject.clone())).collect();
    assert_eq!(
        kinds,
        vec![
            (EventKind::HubUserRevoked, Subject::User(u("sam"))),
            (EventKind::HubTokenRevoked, Subject::Credential(TokenId("t1".into()))),
        ],
        "the user, then each token still active, and no other user's"
    );
    let c = &l.state.credentials;
    assert_eq!(c[&TokenId("t1".into())].state, IdentityState::Revoked);
    assert_eq!(c[&TokenId("t2".into())].state, IdentityState::Revoked);
    assert_eq!(c[&TokenId("t3".into())].state, IdentityState::Active);
    // A repeat decides nothing.
    assert_eq!(l.run(Command::HubUserRevoke { name: u("sam") }, &person("ed")), Ok(vec![]));
}

#[test]
fn a_token_is_for_an_active_user_and_a_new_id() {
    let mut l = world();
    let h = token::hash("a");
    assert_eq!(
        l.run(issue("t1", "ghost", &h), &person("ed")),
        Err(Refusal::NotFound { subject: Subject::User(u("ghost")) })
    );
    l.run(issue("t1", "sam", &h), &person("ed")).unwrap();
    assert!(matches!(l.run(issue("t1", "sam", &h), &person("ed")), Err(Refusal::Invalid { .. })));
    assert!(matches!(l.run(issue("t2", "sam", "not-a-hash"), &person("ed")), Err(Refusal::Invalid { .. })));
    l.run(Command::HubUserRevoke { name: u("sam") }, &person("ed")).unwrap();
    assert_eq!(
        l.run(issue("t2", "sam", &h), &person("ed")),
        Err(Refusal::Illegal { subject: Subject::User(u("sam")), state: "revoked".into(), verb: "hub.token.issue".into() })
    );
}

#[test]
fn revoking_a_token_twice_or_one_that_is_not_there() {
    let mut l = world();
    l.run(issue("t1", "sam", &token::hash("a")), &person("ed")).unwrap();
    assert_eq!(l.run(revoke_token("t1"), &person("ed")).unwrap().len(), 1);
    assert_eq!(l.run(revoke_token("t1"), &person("ed")), Ok(vec![]));
    assert_eq!(
        l.run(revoke_token("t7"), &person("ed")),
        Err(Refusal::NotFound { subject: Subject::Credential(TokenId("t7".into())) })
    );
    assert_eq!(
        l.run(Command::HubUserRevoke { name: u("ghost") }, &person("ed")),
        Err(Refusal::NotFound { subject: Subject::User(u("ghost")) })
    );
}

#[test]
fn other_verbs_are_not_its_own() {
    assert!(Identity::owns(&add("x", false)));
    let cmd = Command::OrderCancel { order: OrderId("o".into()) };
    assert!(!Identity::owns(&cmd));
    assert!(matches!(Identity.decide(&State::default(), &cmd, &hub()), Err(Refusal::Invalid { .. })));
}

#[test]
fn mint_gives_64_hex_and_a_hash_that_hash_reproduces() {
    let (t, h) = token::mint();
    assert_eq!(t.len(), 64);
    assert!(t.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(h.len(), 64);
    assert_eq!(token::hash(&t), h);
    assert_ne!(token::mint().0, t, "tokens are not repeated");
}

#[test]
fn hash_is_the_hex_sha256() {
    assert_eq!(token::hash(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(token::hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
}

#[test]
fn same_is_false_on_any_differing_byte() {
    let (t, _) = token::mint();
    assert!(token::same(&t, &t.clone()));
    for i in 0..t.len() {
        let mut b = t.clone().into_bytes();
        b[i] = if b[i] == b'0' { b'1' } else { b'0' };
        assert!(!token::same(&t, std::str::from_utf8(&b).unwrap()), "byte {i}");
    }
    assert!(!token::same(&t, &t[..63]));
    assert!(!token::same("", "a"));
    assert!(token::same("", ""));
}
