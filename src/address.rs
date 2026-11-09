//! Addresses: which seat a name means (`wsp-520`, the first part of comms).
//!
//! `wsp tell` reached a row (the pane bound to it) or a seat id, and nothing
//! else. That left out every seat that holds no row: the primary, a governor,
//! a verifier, a barrier's check. Each one was reachable only by looking its
//! pane up in `wsp wip` and typing that. An address names the *post*, and this
//! module turns it into the panes in that post now:
//!
//! - `primary`: the primary interface's seat
//! - `governor:<list>` and `governor:<project>`: that scope's governor
//! - `seat:<row>`: whoever holds the row's claim
//! - `verifier:<row>`: the seat a standing verification pass on the row is in
//! - `check:<list>`: the seat of the list's running barrier check
//! - `list:<name>`: every live seat on the list, members and governor
//!
//! **Pure over a [`Book`]**, a snapshot of the records the answer is read
//! from, so every address and every refusal can be proved without a backend.
//! "Live" here means what wsp's own records say is standing: a binding, a
//! governor record with a pane in it, a pass or a check not yet ended. Whether
//! a backend still answers for that pane is the caller's question, and `tell`
//! asks it the way it always has ([`crate::cmd_agent`]'s `locate_seat`).
//!
//! A bare word is not an address, so [`parse`] gives `None` and the caller
//! goes on reading it as a row or seat id, as before. A refusal says what is
//! there instead, so the person has a way forward rather than a "no".
//! A single-seat address that finds more than one seat is refused and names
//! them: only `list:` fans out.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::{Task, Worklist};
use crate::store::Store;

/// An address, parsed. `Bare` is never produced by [`parse`]; it is here so a
/// caller can hold either reading in one type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Primary,
    Governor(String),
    Seat(String),
    Verifier(String),
    Check(String),
    List(String),
}

impl Address {
    /// Whether this address names a group of seats rather than one.
    pub fn fans_out(&self) -> bool {
        matches!(self, Address::List(_))
    }
}

/// The address a word spells, or `None` for a bare word (a row or seat id).
///
/// Only the six spellings above: a herdr pane id (`w1:p6`) has a colon too,
/// and a prefix that is not one of these words leaves it a pane id.
pub fn parse(word: &str) -> Option<Address> {
    let word = word.trim();
    if word == crate::governors::PRIMARY {
        return Some(Address::Primary);
    }
    let (kind, name) = word.split_once(':')?;
    let name = name.trim().to_string();
    let addr = match kind {
        "governor" => Address::Governor(name),
        "seat" => Address::Seat(name),
        "verifier" => Address::Verifier(name),
        "check" => Address::Check(name),
        "list" => Address::List(name),
        _ => return None,
    };
    Some(addr)
}

/// The records an address is resolved over, read once.
#[derive(Debug, Default, Clone)]
pub struct Book {
    pub governors: BTreeMap<String, Value>,
    /// task id -> claim record
    pub claims: BTreeMap<String, Value>,
    /// pane -> binding record
    pub bindings: BTreeMap<String, Value>,
    pub lists: Vec<Worklist>,
    pub tasks: Vec<Task>,
    pub projects: Vec<String>,
}

impl Book {
    pub fn read(store: &Store) -> Book {
        Book {
            governors: store.governors(),
            claims: store.claims(),
            bindings: store.bindings(),
            lists: store.worklists(),
            tasks: store.tasks(),
            projects: store.projects().into_iter().map(|p| p.id).collect(),
        }
    }

    fn list(&self, name: &str) -> Option<&Worklist> {
        self.lists.iter().find(|w| w.id == name)
    }

    fn panes_for(&self, row: &str) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|(_, b)| b.get("task_id").and_then(Value::as_str) == Some(row))
            .map(|(pane, _)| pane.clone())
            .collect()
    }

    /// The pane in a scope's governor seat, if somebody is in it here.
    fn governor_pane(&self, scope: &str) -> Option<String> {
        crate::governors::seat_of_scope(scope, &self.governors).map(|s| s.pane).filter(|p| !p.is_empty())
    }

    /// Every scope with somebody in its seat, for a refusal to name.
    fn seated_scopes(&self) -> Vec<String> {
        self.governors.keys().filter(|s| self.governor_pane(s).is_some()).cloned().collect()
    }
}

/// One seat an address named: the pane, and the post as a person says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub pane: String,
    pub what: String,
}

fn named(pane: &str, what: impl Into<String>) -> Named {
    Named { pane: pane.to_string(), what: what.into() }
}

fn or_none(v: Vec<String>) -> String {
    match v.is_empty() {
        true => "none".into(),
        false => v.join(", "),
    }
}

/// The seats `addr` names in `book`, or why it names none.
///
/// A single-seat address answers one [`Named`] or a refusal; `list:` answers
/// every seat it finds, at least one.
pub fn resolve(addr: &Address, book: &Book) -> Result<Vec<Named>, String> {
    let found = match addr {
        Address::Primary => governor(book, crate::governors::PRIMARY, "the primary")?,
        Address::Governor(scope) => {
            if book.list(scope).is_none() && !book.projects.contains(scope) && !book.governors.contains_key(scope) {
                return Err(format!(
                    "no list or project `{scope}` — seats with a governor in them: {}",
                    or_none(book.seated_scopes())
                ));
            }
            governor(book, scope, format!("the {scope} governor"))?
        }
        Address::Seat(row) => seat(book, row)?,
        Address::Verifier(row) => verifier(book, row)?,
        Address::Check(list) => check(book, list)?,
        Address::List(name) => return list(book, name),
    };
    one(addr, found)
}

fn one(addr: &Address, found: Vec<Named>) -> Result<Vec<Named>, String> {
    if found.len() > 1 && !addr.fans_out() {
        let seats: Vec<String> = found.iter().map(|n| n.pane.clone()).collect();
        return Err(format!(
            "{} is in more than one seat ({}) — name the seat",
            found[0].what,
            seats.join(", ")
        ));
    }
    Ok(found)
}

fn governor(book: &Book, scope: &str, what: impl Into<String>) -> Result<Vec<Named>, String> {
    let what = what.into();
    match book.governor_pane(scope) {
        Some(p) => Ok(vec![named(&p, what)]),
        None => Err(format!(
            "nobody is in {what}'s seat — `wsp govern {scope}` fills it; seats with a governor in them: {}",
            or_none(book.seated_scopes())
        )),
    }
}

fn seat(book: &Book, row: &str) -> Result<Vec<Named>, String> {
    if !book.tasks.iter().any(|t| t.id == row) {
        return Err(format!("no row `{row}` — `wsp ls` lists them"));
    }
    if !book.claims.contains_key(row) {
        return Err(format!("{row} has no claim — `wsp spawn {row}` puts an agent on it"));
    }
    let panes = book.panes_for(row);
    if panes.is_empty() {
        return Err(format!("{row} is claimed and nobody is in its seat — `wsp wip` says who holds what"));
    }
    Ok(panes.iter().map(|p| named(p, format!("the seat on {row}"))).collect())
}

fn verifier(book: &Book, row: &str) -> Result<Vec<Named>, String> {
    if !book.tasks.iter().any(|t| t.id == row) {
        return Err(format!("no row `{row}` — `wsp ls` lists them"));
    }
    let seats = crate::verification::seats(&book.tasks);
    let mine: Vec<Named> =
        seats.iter().filter(|(_, m)| m.as_str() == row).map(|(p, _)| named(p, format!("the verifier of {row}"))).collect();
    if mine.is_empty() {
        let others: Vec<String> = seats.iter().map(|(p, m)| format!("{m} ({p})")).collect();
        return Err(format!("nothing is verifying {row} — verifiers standing: {}", or_none(others)));
    }
    Ok(mine)
}

fn unknown_list(book: &Book, name: &str) -> String {
    let lists: Vec<String> = book.lists.iter().map(|w| w.id.clone()).collect();
    format!("no list `{name}` — lists: {}", or_none(lists))
}

fn check(book: &Book, name: &str) -> Result<Vec<Named>, String> {
    let w = book.list(name).ok_or_else(|| unknown_list(book, name))?;
    let found: Vec<Named> = w
        .groups()
        .iter()
        .enumerate()
        .filter_map(|(i, g)| {
            let c = crate::barrier::running(g)?;
            c.standing().map(|p| named(p, format!("the check on {name} · barrier {}", i + 1)))
        })
        .collect();
    if found.is_empty() {
        return Err(format!("no check is running on {name} — `wsp worklist show {name}` says where its barrier is"));
    }
    Ok(found)
}

fn list(book: &Book, name: &str) -> Result<Vec<Named>, String> {
    let w = book.list(name).ok_or_else(|| unknown_list(book, name))?;
    let mut out = Vec::new();
    if let Some(p) = book.governor_pane(name) {
        out.push(named(&p, format!("the {name} governor")));
    }
    for m in w.groups().iter().flat_map(|g| g.members.clone()) {
        for p in book.panes_for(&m) {
            if !out.iter().any(|n: &Named| n.pane == p) {
                out.push(named(&p, format!("the seat on {m}")));
            }
        }
    }
    if out.is_empty() {
        return Err(format!("nobody is seated on {name} — no governor and no member held; `wsp worklist show {name}`"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Status};
    use serde_json::json;
    use crate::checks::Check;

    fn row(id: &str) -> Task {
        let mut t = Task::new(&format!("row {id}"), id);
        t.set_status(Status::Doing);
        t
    }

    fn bind(b: &mut Book, pane: &str, row: &str) {
        b.bindings.insert(pane.into(), json!({ "task_id": row, "pane_id": pane }));
        b.claims.insert(row.into(), json!({ "workspace_id": "w1" }));
    }

    fn govern(b: &mut Book, scope: &str, pane: &str) {
        b.governors.insert(scope.into(), json!({ "workspace": "w9", "pane": pane, "since": "2026-11-09" }));
    }

    /// A store and seat fixture: a project `wsp`, a list `run` of m-1 and
    /// m-2 in one group, a governor on each and on the primary, m-1 held by
    /// cpd-1, m-2 claimed with nobody in it, a verifier on m-1, and the
    /// barrier's check running in cpd-8.
    fn book() -> Book {
        let mut b = Book { projects: vec!["wsp".into()], ..Book::default() };
        let mut m1 = row("m-1");
        let mut pass = crate::verification::Pass::opened(Some("abc1234".into()), None);
        pass.pane = Some("cpd-5".into());
        crate::verification::write(&mut m1, &[pass]);
        b.tasks = vec![m1, row("m-2"), row("m-3")];
        bind(&mut b, "cpd-1", "m-1");
        b.claims.insert("m-2".into(), json!({ "workspace_id": "w2" }));
        govern(&mut b, "primary", "cpd-100");
        govern(&mut b, "wsp", "cpd-101");
        govern(&mut b, "run", "cpd-102");
        let mut w = Worklist::new("run", "r");
        let mut check = Check::opened(None, None);
        check.pane = Some("cpd-8".into());
        let g = Group { members: vec!["m-1".into(), "m-2".into()], checks: vec![check], ..Group::default() };
        w.set_groups(&[g]);
        b.lists = vec![w];
        b
    }

    fn panes(r: Result<Vec<Named>, String>) -> Vec<String> {
        r.unwrap().into_iter().map(|n| n.pane).collect()
    }

    fn at(word: &str, b: &Book) -> Result<Vec<Named>, String> {
        resolve(&parse(word).unwrap_or_else(|| panic!("{word} is an address")), b)
    }

    #[test]
    fn a_bare_word_and_a_pane_id_are_not_addresses() {
        assert_eq!(parse("wsp-520"), None);
        assert_eq!(parse("cpd-3"), None);
        assert_eq!(parse("w1:p6"), None, "a herdr pane keeps its colon");
        assert_eq!(parse("primary"), Some(Address::Primary));
        assert_eq!(parse("list:run"), Some(Address::List("run".into())));
    }

    #[test]
    fn primary_is_the_primary_seat() {
        assert_eq!(panes(at("primary", &book())), vec!["cpd-100"]);
    }

    #[test]
    fn governor_names_a_list_or_a_project_seat() {
        let b = book();
        assert_eq!(panes(at("governor:run", &b)), vec!["cpd-102"]);
        assert_eq!(panes(at("governor:wsp", &b)), vec!["cpd-101"]);
        assert_eq!(at("governor:wsp", &b).unwrap()[0].what, "the wsp governor");
    }

    #[test]
    fn seat_is_whoever_holds_the_rows_claim() {
        assert_eq!(panes(at("seat:m-1", &book())), vec!["cpd-1"]);
    }

    #[test]
    fn verifier_is_the_standing_pass_on_the_row() {
        assert_eq!(panes(at("verifier:m-1", &book())), vec!["cpd-5"]);
    }

    #[test]
    fn check_is_the_running_barriers_seat() {
        let b = book();
        assert_eq!(panes(at("check:run", &b)), vec!["cpd-8"]);
        assert_eq!(at("check:run", &b).unwrap()[0].what, "the check on run · barrier 1");
    }

    #[test]
    fn list_is_every_seat_on_it_governor_and_members() {
        let mut b = book();
        bind(&mut b, "cpd-2", "m-2");
        assert_eq!(panes(at("list:run", &b)), vec!["cpd-102", "cpd-1", "cpd-2"]);
    }

    #[test]
    fn nobody_in_the_seat_is_refused_with_what_is_seated() {
        let mut b = book();
        b.governors.insert("wsp".into(), json!({ "workspace": "w9", "pane": "" }));
        let why = at("governor:wsp", &b).unwrap_err();
        assert!(why.contains("nobody is in the wsp governor's seat"), "{why}");
        assert!(why.contains("wsp govern wsp"), "{why}");
        assert!(why.contains("primary, run"), "names what is seated: {why}");
        b.governors.remove("primary");
        assert!(at("primary", &b).unwrap_err().contains("nobody is in the primary's seat"));
        let why = at("seat:m-2", &b).unwrap_err();
        assert!(why.contains("m-2 is claimed and nobody is in its seat"), "{why}");
        let why = at("verifier:m-2", &b).unwrap_err();
        assert!(why.contains("nothing is verifying m-2") && why.contains("m-1 (cpd-5)"), "{why}");
    }

    #[test]
    fn a_row_with_no_claim_is_refused() {
        let b = book();
        let why = at("seat:m-3", &b).unwrap_err();
        assert!(why.contains("m-3 has no claim") && why.contains("wsp spawn m-3"), "{why}");
        assert!(at("seat:m-404", &b).unwrap_err().contains("no row `m-404`"));
    }

    #[test]
    fn an_unknown_list_is_refused_with_the_lists_there_are() {
        let b = book();
        for word in ["list:nope", "check:nope"] {
            let why = at(word, &b).unwrap_err();
            assert!(why.contains("no list `nope`") && why.contains("lists: run"), "{word}: {why}");
        }
        let why = at("governor:nope", &b).unwrap_err();
        assert!(why.contains("no list or project `nope`"), "{why}");
    }

    #[test]
    fn a_list_with_no_running_check_or_nobody_on_it_is_refused() {
        let mut b = book();
        let mut w = b.lists[0].clone();
        let mut gs = w.groups();
        gs[0].checks.clear();
        w.set_groups(&gs);
        b.lists = vec![w];
        assert!(at("check:run", &b).unwrap_err().contains("no check is running on run"));
        b.governors.remove("run");
        b.bindings.clear();
        assert!(at("list:run", &b).unwrap_err().contains("nobody is seated on run"));
    }

    #[test]
    fn a_single_seat_address_in_two_seats_is_refused_and_a_list_fans_out() {
        let mut b = book();
        b.bindings.insert("cpd-9".into(), json!({ "task_id": "m-1" }));
        let why = at("seat:m-1", &b).unwrap_err();
        assert!(why.contains("more than one seat (cpd-1, cpd-9)"), "{why}");
        assert_eq!(panes(at("list:run", &b)), vec!["cpd-102", "cpd-1", "cpd-9"]);
    }
}
