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
use std::time::Duration;

use wsp_client::{Client, Credential};
use wsp_store_contract::Principal;

use crate::base::args::Args;
use crate::store::Store;

/// How long a dial waits for the hub: a verb writing one line does not wait on it for long.
const WAIT: Duration = Duration::from_secs(3);

/// The engineer this process is logged in as, if its credential is an
/// engineer token the hub knows: resolved once, through `whoami`.
pub fn engineer() -> Option<String> {
    static ENGINEER: OnceLock<Option<String>> = OnceLock::new();
    ENGINEER.get_or_init(|| resolve().and_then(|(by, _)| engineer_of(&by))).clone()
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

/// Who the hub says this process is, and its name: none when there is no
/// login, no hub URL or no answer. Never reads `api.token`.
fn resolve() -> Option<(Principal, String)> {
    // A test never dials: a developer's own login must not change what it reads.
    if crate::base::under_test() {
        return None;
    }
    let credential = Credential::for_hub().ok()?;
    let url = wsp_client::hub_url().ok()?;
    let ca = wsp_client::hub_ca().ok()?;
    let mut client = Client::dial_hub_ca(&url, credential, ca.as_deref(), WAIT).ok()?;
    client.whoami().ok()
}

/// What `wsp whoami` prints: `person:jesper (core: engineer)`.
pub fn describe(by: &Principal, server: &str) -> String {
    let kind = match by {
        Principal::Person(_) => "engineer",
        Principal::Seat(_) => "seat",
        Principal::Manager(_) => "manager",
        Principal::Executor(_) => "executor",
        Principal::Wsp(_) => "instance",
    };
    format!("{} ({server}: {kind})", wsp_api::principal::text(by))
}

/// `wsp whoami`: the principal the hub holds this process as, or `person` when it is not logged in.
pub fn whoami(_store: &Store, args: &Args) -> i32 {
    let said = resolve();
    if args.json() {
        let v = match &said {
            Some((by, server)) => serde_json::json!({ "by": wsp_api::principal::text(by), "server": server }),
            None => serde_json::json!({ "by": "person", "server": null }),
        };
        println!("{v}");
        return 0;
    }
    match said {
        Some((by, server)) => println!("{}", describe(&by, &server)),
        None => println!("person (not logged in to a hub)"),
    }
    0
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

    #[test]
    fn whoami_reads_as_the_principal_and_what_it_is() {
        assert_eq!(describe(&Principal::Person("jesper".into()), "core"), "person:jesper (core: engineer)");
    }
}
