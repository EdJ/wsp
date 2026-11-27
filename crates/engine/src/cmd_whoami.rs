//! `wsp whoami` — who this process writes as (wsp-165).
//!
//! Outside a pane a caller is a person, and with an engineer's token a person
//! with a name: the hub the token is for answers `whoami` with the principal
//! the token names (`person:jesper`), and every record this process makes
//! then says which engineer made it. Without such a token the caller is the
//! literal `person`, as before, and nothing is dialled: [`engineer`] reads the
//! credential first, and a spoke with no login never opens a connection.
//!
//! The reading is made once per process and kept ([`engineer`]); a hub that
//! does not answer reads as no engineer, so a write is never held up by it.

use std::sync::OnceLock;

use wsp_store_contract::Principal;

use crate::base::args::Args;
use crate::store::Store;

/// The engineer this process is logged in as, if its credential is an
/// engineer token the hub knows: resolved once, through `whoami`.
pub fn engineer() -> Option<String> {
    static ENGINEER: OnceLock<Option<String>> = OnceLock::new();
    ENGINEER
        .get_or_init(|| match resolve() {
            crate::addon::WhoAmI::Known(by, _) => engineer_of(&by),
            _ => None,
        })
        .clone()
}

/// The name a principal gives a person: an engineer's, and nobody else's.
pub fn engineer_of(by: &Principal) -> Option<String> {
    match by {
        Principal::Person(name) if !name.is_empty() => Some(name.clone()),
        _ => None,
    }
}

/// The token a record's `by` names for a person at a shell: `person:<engineer>`, else `person`.
pub fn person() -> String {
    person_named(engineer().as_deref())
}

pub fn person_named(engineer: Option<&str>) -> String {
    match engineer {
        Some(name) => format!("person:{name}"),
        None => "person".into(),
    }
}

/// `line` as a person at a shell signs it into a log: `· <engineer>` at its end when one is logged in.
pub fn sign(line: String) -> String {
    match (crate::place::seat_from_env().is_none(), engineer()) {
        (true, Some(name)) => format!("{line} · {name}"),
        _ => line,
    }
}

/// Who the shared-work add-on says this process is, or why it cannot say.
fn resolve() -> crate::addon::WhoAmI {
    crate::addon::get().whoami()
}

/// What `wsp whoami` prints: `person:jesper (core: engineer)`.
pub fn describe(by: &Principal, server: &str) -> String {
    let kind = match by {
        Principal::Person(_) => "engineer",
        Principal::Seat(_) => "seat",
        Principal::Manager(_) => "manager",
        Principal::Executor(_) => "executor",
        Principal::Wsp(_) => "instance",
        Principal::Agent(_) => "agent",
    };
    format!("{} ({server}: {kind})", wsp_api::principal::text(by))
}

/// What `wsp whoami` prints, and its exit code: 0 for a principal the hub
/// named or no login at all; 1 for a login the hub refused or did not
/// answer, which says so rather than reading as no login.
pub fn whoami_said(said: &crate::addon::WhoAmI, json: bool) -> (String, i32) {
    use crate::addon::WhoAmI;
    if json {
        let v = match said {
            WhoAmI::Known(by, server) => serde_json::json!({ "by": wsp_api::principal::text(by), "server": server }),
            WhoAmI::NotLoggedIn => serde_json::json!({ "by": "person", "server": null }),
            WhoAmI::Refused(why) => serde_json::json!({ "by": "person", "server": null, "refused": why }),
            WhoAmI::Unanswered(why) => serde_json::json!({ "by": "person", "server": null, "unanswered": why }),
        };
        let code = if matches!(said, WhoAmI::Known(..) | WhoAmI::NotLoggedIn) { 0 } else { 1 };
        return (v.to_string(), code);
    }
    match said {
        WhoAmI::Known(by, server) => (describe(by, server), 0),
        WhoAmI::NotLoggedIn => ("person (not logged in to a hub)".into(), 0),
        WhoAmI::Refused(why) => (format!("person: {why}"), 1),
        WhoAmI::Unanswered(why) => (format!("person: logged in, but the hub did not answer: {why}"), 1),
    }
}

/// `wsp whoami`: the principal the hub holds this process as, `person` when it
/// is not logged in, and why not when a login is there and was not answered.
pub fn whoami(_store: &Store, args: &Args) -> i32 {
    let (text, code) = whoami_said(&resolve(), args.json());
    match code {
        0 => println!("{text}"),
        _ => eprintln!("wsp: whoami: {text}"),
    }
    code
}

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["whoami"],
    run: whoami,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Tasks,
        order: 366,
        text: r##"  wsp whoami                        who the hub says you are (person:<engineer>)"##,
    }],
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_the_hub_names_is_an_engineer_and_no_other_principal_is() {
        assert_eq!(engineer_of(&Principal::Person("jesper".into())).as_deref(), Some("jesper"));
        assert_eq!(engineer_of(&Principal::Person(String::new())), None);
        assert_eq!(engineer_of(&Principal::Wsp("hub".into())), None);
    }

    #[test]
    fn the_token_is_person_alone_without_an_engineer_and_person_colon_name_with_one() {
        assert_eq!(person_named(None), "person");
        assert_eq!(person_named(Some("jesper")), "person:jesper");
        assert_eq!(engineer(), None, "a test never dials");
        assert_eq!(person(), "person");
    }

    /// A login the hub refused, or did not answer, is not "not logged in":
    /// it says so and exits 1.
    #[test]
    fn whoami_says_a_refused_or_unanswered_login_and_exits_one() {
        use crate::addon::WhoAmI;
        assert_eq!(whoami_said(&WhoAmI::NotLoggedIn, false), ("person (not logged in to a hub)".to_string(), 0));
        let (text, code) = whoami_said(&WhoAmI::Refused("the hub refused this login: revoked — `wsp login` again".into()), false);
        assert_eq!(code, 1);
        assert!(text.contains("refused this login") && text.contains("wsp login") && !text.contains("not logged in"), "{text}");
        let (text, code) = whoami_said(&WhoAmI::Unanswered("nothing answered".into()), true);
        assert_eq!(code, 1);
        assert!(text.contains("\"unanswered\""), "{text}");
        let (text, code) = whoami_said(&WhoAmI::Known(Principal::Person("ann".into()), "hub".into()), false);
        assert_eq!((text.as_str(), code), ("person:ann (hub: engineer)", 0));
    }

    #[test]
    fn whoami_reads_as_the_principal_and_what_it_is() {
        assert_eq!(describe(&Principal::Person("jesper".into()), "core"), "person:jesper (core: engineer)");
    }
}
