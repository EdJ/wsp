//! `wsp allocate <row|list> <user>|none` — work bound to one user's agents.
//!
//! Pre-allocation (multiplayer v1): a row or a worklist in a shared workspace
//! names the user whose agents may take it, on any spoke that user is logged
//! in on. It is the `for:` of the record's front matter, written only where
//! set. A private record is only ever this spoke's, so allocating it means
//! nothing and is refused, naming `wsp publish`.
//!
//! [`allocated_to`] is the one reading of it: a row's own `for:`, else the
//! `for:` of the list that holds it, else nobody.
//!
//! The user is not yet checked against the workspace's member list: that
//! cache arrives with wsp-465, and until then any name is taken.

use serde_json::json;

use crate::base::args::Args;
use crate::model::WorklistStatus;
use crate::store::Store;

/// The user `row` is allocated to: its own `for:`, else the `for:` of a list
/// that holds it and has not finished, else `None`.
pub fn allocated_to(store: &Store, row: &str) -> Option<String> {
    if let Some(t) = store.task(row) {
        if t.for_user.is_some() {
            return t.for_user;
        }
    }
    store
        .worklists()
        .into_iter()
        .filter(|w| w.status() != WorklistStatus::Done)
        .filter(|w| w.groups().iter().any(|g| g.members.iter().any(|m| m == row)))
        .find_map(|w| w.for_user)
}

fn refuse_private(id: &str) -> i32 {
    eprintln!(
        "wsp: {id} is private, and only this spoke ever sees it — allocation means something only in a shared workspace; `wsp publish {id}` it first"
    );
    1
}

fn report(args: &Args, id: &str, user: &Option<String>, said: &str) {
    if args.json() {
        println!("{}", json!({ "id": id, "for": user }));
    } else {
        println!("{id} {said}");
    }
}

pub fn allocate(store: &Store, args: &Args) -> i32 {
    let (Some(needle), Some(who)) = (args.rest.first().cloned(), args.rest.get(1).cloned()) else {
        eprintln!("usage: wsp allocate <row|list> <user>|none");
        return 2;
    };
    let who = who.trim().to_string();
    if who.is_empty() {
        eprintln!("usage: wsp allocate <row|list> <user>|none");
        return 2;
    }
    let want: Option<String> = if who == "none" { None } else { Some(who) };
    let said = match &want {
        Some(u) => format!("allocated to {u}"),
        None => "allocation cleared".to_string(),
    };

    if let Some(mut t) = store.find_task(&needle) {
        if t.workspace.is_private() {
            return refuse_private(&t.id);
        }
        if t.for_user == want {
            report(args, &t.id, &want, &format!("already {}", state(&want)));
            return 0;
        }
        t.log(&said);
        t.for_user = want.clone();
        t.touch();
        if let Err(e) = store.save_task(&t) {
            eprintln!("wsp: write failed: {e}");
            return 1;
        }
        store.log_event("task-allocate", json!({ "id": t.id, "for": want, "project": t.project, "title": t.title }));
        store.git_commit(&format!("wsp: allocate {} — {}", t.id, t.title));
        report(args, &t.id, &want, &said);
        return 0;
    }

    if let Some(mut w) = crate::cmd_worklist::find(store, &needle) {
        if w.workspace.is_private() {
            return refuse_private(&w.id);
        }
        if w.for_user == want {
            report(args, &w.id, &want, &format!("already {}", state(&want)));
            return 0;
        }
        w.log(&said);
        w.for_user = want.clone();
        if let Err(e) = store.save_worklist(&w) {
            eprintln!("wsp: write failed: {e}");
            return 1;
        }
        store.log_event("worklist-allocate", json!({ "id": w.id, "for": want }));
        store.git_commit(&format!("wsp: allocate {} — {}", w.id, w.title));
        report(args, &w.id, &want, &said);
        return 0;
    }

    eprintln!("wsp: no row or list matching `{needle}`");
    1
}

fn state(u: &Option<String>) -> String {
    match u {
        Some(u) => format!("allocated to {u}"),
        None => "unallocated".to_string(),
    }
}

/// The verbs this module answers to.
pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["allocate"],
    run: allocate,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Tasks,
        order: 365,
        text: r##"  wsp allocate <id> <user>|none     bind shared work to one user's agents"##,
    }],
}];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Task, Worklist};

    fn scratch(tag: &str) -> (crate::util::Isolated, Store) {
        let env = crate::util::isolated(tag);
        let store = Store::open();
        store.ensure_dirs().unwrap();
        store.save_project(&crate::model::Project::new("wsp")).unwrap();
        (env, store)
    }

    fn row(store: &Store, id: &str, shared: bool) {
        let mut t = Task::new("the work", id);
        t.project = Some("wsp".into());
        t.body = String::from("## Overview\nbrief\n\n## Log\n- 2026-11-23 made\n");
        if shared {
            t.workspace = "shared:acme".parse().unwrap();
        }
        store.save_task(&t).unwrap();
    }

    fn list(store: &Store, id: &str, members: &[&str], shared: bool) {
        let mut w = Worklist::new(id, "the run");
        let groups: Vec<Group> = members.iter().map(|m| Group { members: vec![m.to_string()], ..Group::default() }).collect();
        w.set_groups(&groups);
        if shared {
            w.workspace = "shared:acme".parse().unwrap();
        }
        store.save_worklist(&w).unwrap();
    }

    fn run(store: &Store, rest: &[&str]) -> i32 {
        allocate(store, &Args::synth("allocate", rest, &[]))
    }

    #[test]
    fn allocating_a_shared_row_sets_for_and_none_clears_it() {
        let (_env, store) = scratch("allocate-row");
        row(&store, "t-1", true);
        assert_eq!(run(&store, &["t-1", "ed"]), 0);
        let t = store.task("t-1").unwrap();
        assert_eq!(t.for_user.as_deref(), Some("ed"));
        assert!(t.render().contains("for: ed"));
        assert!(t.body.contains("allocated to ed"));
        assert_eq!(allocated_to(&store, "t-1").as_deref(), Some("ed"));

        assert_eq!(run(&store, &["t-1", "none"]), 0);
        let t = store.task("t-1").unwrap();
        assert_eq!(t.for_user, None);
        assert!(!t.render().contains("for:"));
        assert_eq!(allocated_to(&store, "t-1"), None);
    }

    #[test]
    fn a_private_row_or_list_is_refused_and_unchanged() {
        let (_env, store) = scratch("allocate-private");
        row(&store, "t-1", false);
        list(&store, "wl", &["t-1"], false);
        let before = store.task("t-1").unwrap().render();
        assert_eq!(run(&store, &["t-1", "ed"]), 1);
        assert_eq!(run(&store, &["wl", "ed"]), 1);
        assert_eq!(store.task("t-1").unwrap().render(), before);
        assert_eq!(store.worklist("wl").unwrap().for_user, None);
    }

    #[test]
    fn a_list_allocates_its_members_unless_they_are_allocated_themselves() {
        let (_env, store) = scratch("allocate-list");
        row(&store, "t-1", true);
        row(&store, "t-2", true);
        row(&store, "t-3", true);
        list(&store, "wl", &["t-1", "t-2"], true);
        assert_eq!(run(&store, &["t-2", "ed"]), 0);
        assert_eq!(run(&store, &["wl", "jesper"]), 0);
        assert_eq!(store.worklist("wl").unwrap().for_user.as_deref(), Some("jesper"));
        assert_eq!(allocated_to(&store, "t-1").as_deref(), Some("jesper"));
        assert_eq!(allocated_to(&store, "t-2").as_deref(), Some("ed"));
        assert_eq!(allocated_to(&store, "t-3"), None, "a row in no list has only its own");
        assert_eq!(run(&store, &["wl", "none"]), 0);
        assert_eq!(allocated_to(&store, "t-1"), None);
        assert_eq!(allocated_to(&store, "t-2").as_deref(), Some("ed"));
    }

    #[test]
    fn a_record_without_the_field_is_unchanged_on_save() {
        let (_env, store) = scratch("allocate-bytes");
        row(&store, "t-1", false);
        list(&store, "wl", &["t-1"], false);
        let t = store.task("t-1").unwrap();
        assert!(!t.render().contains("for:"));
        store.save_task(&t).unwrap();
        assert_eq!(store.task("t-1").unwrap().render(), t.render());
        assert!(!store.worklist("wl").unwrap().render().contains("for:"));
    }

    #[test]
    fn show_json_carries_for() {
        let (_env, store) = scratch("allocate-json");
        row(&store, "t-1", true);
        assert_eq!(run(&store, &["t-1", "ed"]), 0);
        let v = crate::cmd_task::show_json(&store, "t-1").unwrap();
        assert_eq!(v["for"], "ed");
    }
}
