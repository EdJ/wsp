//! A hub's refusal in words, and its stable machine-readable form.
//!
//! The hub sends a refusal as `{ code, text, refusal? }` ([`Failure`]). For a
//! contract refusal (`refusal` present) its `text` is, at the hub this was
//! built against, Rust's debug form of the refusal (`wsp-hub
//! crates/ledger/src/conn.rs`, `store_failure`: `format!("{r:?}")`), except
//! `too_old`, whose text is a sentence, and `forbidden` from the hub's own
//! admission check, whose text names why. So a refusal is rendered here from
//! its fields, and the hub's text is used only where it is not that debug
//! form. A failure with no refusal carries the hub's text, which is a sentence.
//!
//! [`refusal_fields`] is the refusal as the hub's wire spells it (`code` and
//! its fields), for `--json`; [`code`] a [`Code`] as its wire name.

use wsp_hub_protocol::{principal, Code, Failure, Refused};
use wsp_store_contract::{Refusal, Subject};

/// `code`'s wire name (`not_found`, `too_old`, `unauthorised`).
pub fn code(c: Code) -> String {
    serde_json::to_value(c).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "unknown".into())
}

/// The refusal's wire code (`illegal`, `not_found`).
pub fn refusal_code(r: &Refusal) -> String {
    refusal_fields(r).get("code").and_then(|c| c.as_str()).unwrap_or("unknown").to_string()
}

/// The refusal as the hub's wire spells it: `{"code": …, <its fields>}`.
pub fn refusal_fields(r: &Refusal) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(r) {
        Ok(serde_json::Value::Object(m)) => m,
        // `Unknown` serializes as a bare tag.
        _ => {
            let mut m = serde_json::Map::new();
            m.insert("code".into(), "unknown".into());
            m
        }
    }
}

/// What a subject names, in words: `user bo`, `token record t-bo-1`,
/// `shared workspace team`, `bo's membership of team`.
pub fn subject(s: &Subject) -> String {
    match s {
        Subject::Project(p) => format!("project {}", p.0),
        Subject::Row(r) => format!("row {}", r.0),
        Subject::List(l) => format!("list {}", l.0),
        Subject::Group(g) => format!("group {} of list {}", g.n, g.list.0),
        Subject::Seat(s) => format!("seat {}", s.0),
        Subject::Claim(r) => format!("the claim on row {}", r.0),
        Subject::Order(o) => format!("order {}", o.0),
        Subject::Ask(a) => format!("ask {}", a.0),
        Subject::Run(k) => format!("run {} of row {} (attempt {})", serde_json::to_value(k.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(), k.row.0, k.attempt),
        Subject::Machine(m) => format!("machine {}", m.0),
        Subject::Lease(l) => format!("lease {}", l.0),
        Subject::User(u) => format!("user {}", u.0),
        Subject::Credential(t) => format!("token record {}", t.0),
        Subject::Workspace(w) => format!("shared workspace {}", w.0),
        Subject::Member { workspace, user } => format!("{}'s membership of {}", user.0, workspace.0),
    }
}

/// Whether the hub's `text` of `r` says more than the refusal's fields: not
/// empty, and not Rust's debug form of it.
fn text_says_more(r: &Refusal, text: &str) -> bool {
    !text.trim().is_empty() && text != format!("{r:?}")
}

/// The refusal `r` in a sentence; `text` is what the hub said with it.
pub fn refusal(r: &Refusal, text: &str) -> String {
    let more = text_says_more(r, text);
    match r {
        Refusal::Forbidden { .. } if more => format!("forbidden: {text}"),
        Refusal::Forbidden { by } => format!("forbidden: {} may not do this", principal::text(by)),
        Refusal::NotFound { subject: s } => format!("the hub has no {}", subject(s)),
        Refusal::Illegal { subject: s, state, verb } => format!("{} is {state}, so the hub refuses {verb}", subject(s)),
        Refusal::Invalid { reason } => reason.clone(),
        Refusal::TooOld { oldest } => format!("the events asked for are past what the hub keeps: the oldest it keeps is event {}", oldest.0),
        Refusal::Held { .. } if more => text.to_string(),
        Refusal::Held { lease, holder } => format!("{} is held by {}", lease.0, principal::text(holder)),
        Refusal::Stale { lease, current } => format!("stale: {} has moved on to token {}, past the one this was sent under", lease.0, current.0),
        Refusal::Conflict { current } => format!("conflict: the revision named is not the current one ({})", current.0),
        Refusal::Unknown if more => text.to_string(),
        Refusal::Unknown => "the hub refused it, for a reason this wsp does not know (a newer hub)".into(),
    }
}

/// A failure with no contract refusal (or one with), in a sentence.
pub fn failure(f: &Failure) -> String {
    if let Some(r) = &f.refusal {
        return refusal(r, &f.text);
    }
    coded(f.code, &f.text)
}

/// A hello or login the hub refused, in a sentence.
pub fn refused(r: &Refused) -> String {
    coded(r.code, &r.text)
}

fn coded(c: Code, text: &str) -> String {
    let what = match c {
        Code::Forbidden => "forbidden",
        Code::Unauthorised => "the hub does not take this login",
        Code::Malformed => "the hub could not read the request",
        Code::Protocol => "the hub speaks another protocol",
        Code::Unsupported => "the hub does not run this verb",
        Code::Backend => "the hub's store failed it (a retry may pass)",
        Code::Fenced => "fenced: the lease token is not the row's current one",
        Code::TooLarge => "the hub's answer would be too large for one line",
        _ => return if text.trim().is_empty() { format!("the hub refused it ({})", code(c)) } else { format!("{} ({})", text, code(c)) },
    };
    if text.trim().is_empty() {
        what.to_string()
    } else {
        format!("{what}: {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_store_contract::{EventId, LeaseName, Principal, Rev, SharedName, Token, TokenId, UserName};

    #[test]
    fn no_refusal_kind_reads_as_rusts_debug_form() {
        let all = [
            Refusal::Stale { lease: LeaseName("row:wsp-1".into()), current: Token(4) },
            Refusal::TooOld { oldest: EventId(41) },
            Refusal::Conflict { current: Rev("r9".into()) },
            Refusal::Held { lease: LeaseName("row:wsp-1".into()), holder: Principal::Person("bo".into()) },
            Refusal::NotFound { subject: Subject::User(UserName("cy".into())) },
            Refusal::Illegal { subject: Subject::User(UserName("cy".into())), state: "revoked".into(), verb: "hub.member.set".into() },
            Refusal::Forbidden { by: Principal::Person("bo".into()) },
            Refusal::Invalid { reason: "shared workspace team exists".into() },
            Refusal::Unknown,
        ];
        for r in all {
            let said = refusal(&r, &format!("{r:?}"));
            assert!(!said.contains('{') && !said.contains("UserName(") && !said.contains("Person("), "{said}");
            assert!(refusal_fields(&r).contains_key("code"), "{r:?}");
        }
        let held = Refusal::Held { lease: LeaseName("row:wsp-1".into()), holder: Principal::Person("bo".into()) };
        assert_eq!(refusal(&held, "row:wsp-1 is held by bo on box-b"), "row:wsp-1 is held by bo on box-b", "the hub's own sentence is used");
        assert_eq!(subject(&Subject::Member { workspace: SharedName("team".into()), user: UserName("bo".into()) }), "bo's membership of team");
        assert_eq!(subject(&Subject::Credential(TokenId("gh-bo-1".into()))), "token record gh-bo-1");
    }

    #[test]
    fn a_failure_with_no_refusal_is_its_code_in_words_and_the_hubs_text() {
        let f = Failure { code: Code::Unsupported, text: "hub.lease.drop".into(), refusal: None };
        assert_eq!(failure(&f), "the hub does not run this verb: hub.lease.drop");
        assert_eq!(code(Code::TooOld), "too_old");
        assert_eq!(refused(&Refused { code: Code::Unauthorised, text: "the token names nobody".into() }), "the hub does not take this login: the token names nobody");
    }
}
