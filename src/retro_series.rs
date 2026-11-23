//! The two numbered list series a timing retro files on (`wsp-472`): `retro-N`
//! for defects, born planned, and `retro-ask-N` for strategic questions, born
//! init and asking Ed. A series is opened only when something is filed on it,
//! and a list that has started is never reused — the next filing opens the
//! next number.

use serde_json::Value;

use crate::choices::Choices;
use crate::message::{self, About, Kind, Party, Waiting};
use crate::model::{Group, Status, Task, Worklist, WorklistStatus};
use crate::store::Store;

pub const DEFECTS: &str = "retro";
pub const QUESTIONS: &str = "retro-ask";

/// `retro-3` of series `retro` is 3; `retro-ask-3` is not of series `retro`.
fn number_of(series: &str, id: &str) -> Option<u32> {
    id.strip_prefix(series)?.strip_prefix('-')?.parse().ok()
}

/// Whether an id is one of the retro series' (`retro-N`, `retro-ask-N`): such a
/// list is the retro machinery's own, and a retro of it would measure retros.
pub fn is_series_id(id: &str) -> bool {
    number_of(DEFECTS, id).is_some() || number_of(QUESTIONS, id).is_some()
}

/// A list of the series that nothing has started. A defect list stays open
/// once planned; an ask list that has been answered (planned) is Ed's decision
/// made, and the next question opens the next number.
fn open_in(store: &Store, series: &str) -> Option<Worklist> {
    store
        .worklists()
        .into_iter()
        .filter(|w| number_of(series, &w.id).is_some())
        .filter(|w| w.status() == WorklistStatus::Draft && !w.is_cancelled())
        .filter(|w| series == DEFECTS || !w.is_planned())
        .filter(|w| series != DEFECTS || !is_retros_list(w))
        .max_by_key(|w| number_of(series, &w.id))
}

/// The agent line of a retros list's first group (`wsp-558`).
pub const RETRO_AGENT: &str = "claude sonnet high";
const RETRO_STOP: &str = "Pass when each timing retro in this group is at review with its verifier holding: the list's effort report is read, its waits named by cause with seconds, and every defect it finds is filed as a row (on the next retro-defects list, as wsp-472 says), with no fix made here. Analysis only, no full suite.";

/// A retros list: a `retro-N` whose first group is the retro group — on the
/// retro agent line, its barrier not yet passed — and which has not finished.
/// Defects never go on one, so a manual defects group cannot block a retro.
pub fn is_retros_list(w: &Worklist) -> bool {
    if number_of(DEFECTS, &w.id).is_none() || w.is_cancelled() || w.status() == WorklistStatus::Done {
        return false;
    }
    w.groups().first().is_some_and(|g| g.agent.trim() == RETRO_AGENT && g.verdict.trim().is_empty())
}

/// Put a timing-retro row on the open retros list, filing the next `retro-N`
/// (planned, not alone, one group on the retro agent line at x2) when none is
/// open. Returns the list's id. The run starts the row like any member; nothing
/// is spawned here.
pub fn place_retro(store: &Store, row: &str, project: Option<&str>) -> Result<String, String> {
    let open = store.worklists().into_iter().filter(is_retros_list).max_by_key(|w| number_of(DEFECTS, &w.id));
    if let Some(mut w) = open {
        let mut groups = w.groups();
        if !groups[0].members.iter().any(|m| m == row) {
            groups[0].members.push(row.to_string());
        }
        w.log(&format!("{row} → group 1 · the timing retro"));
        w.set_groups(&groups);
        store.save_worklist(&w).map_err(|e| format!("write failed: {e}"))?;
        return Ok(w.id);
    }
    let next = store.worklists().iter().filter_map(|w| number_of(DEFECTS, &w.id)).max().unwrap_or(0) + 1;
    let id = format!("{DEFECTS}-{next}");
    let mut w = Worklist::new(&id, &format!("Timing retros {next}"));
    w.set_planned();
    w.alone = Some(false);
    w.no_retro = true;
    w.project = project.map(str::to_string);
    w.set_groups(&[Group { members: vec![row.to_string()], cap: Some(2), agent: RETRO_AGENT.into(), stop: RETRO_STOP.into(), ..Group::default() }]);
    w.log(&format!("{row} → group 1 · the timing retro"));
    store.save_worklist(&w).map_err(|e| format!("cannot write {id}: {e}"))?;
    store.log_event("worklist-added", serde_json::json!({ "id": id }));
    Ok(id)
}

/// The newest list of `series` that has not started, or the next number's.
pub fn open_list(store: &Store, series: &str) -> Result<Worklist, String> {
    if let Some(w) = open_in(store, series) {
        return Ok(w);
    }
    let next = store.worklists().iter().filter_map(|w| number_of(series, &w.id)).max().unwrap_or(0) + 1;
    let id = format!("{series}-{next}");
    let title = if series == DEFECTS { format!("Timing retro defects {next}") } else { format!("Timing retro questions {next}") };
    let mut w = Worklist::new(&id, &title);
    w.no_retro = true;
    if series == DEFECTS {
        w.set_planned();
    }
    store.save_worklist(&w).map_err(|e| format!("cannot write {id}: {e}"))?;
    store.log_event("worklist-added", serde_json::json!({ "id": id }));
    Ok(w)
}

fn new_row(store: &Store, title: &str, sections: &[(&str, &str)], project: Option<&str>) -> Result<Task, String> {
    let id = store.alloc_task_id(project).map_err(|e| format!("cannot allocate id: {e}"))?;
    let mut t = Task::new(title.trim(), &id);
    t.project = project.map(str::to_string);
    if project.is_none() {
        t.status_raw = "inbox".into();
    }
    for (name, text) in sections {
        crate::model::set_section_in(&mut t.body, name, text.trim());
    }
    store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
    store.log_event("task-added", serde_json::json!({ "id": t.id, "project": t.project, "title": t.title }));
    Ok(t)
}

fn join(store: &Store, mut w: Worklist, row: &str, project: Option<&str>) -> Result<(), String> {
    let mut groups = w.groups();
    if groups.is_empty() {
        groups.push(Group { members: vec![row.to_string()], ..Group::default() });
    } else {
        groups[0].members.push(row.to_string());
    }
    if w.project.is_none() {
        w.project = project.map(str::to_string);
    }
    w.log(&format!("{row} → group 1 · filed by the timing retro"));
    w.set_groups(&groups);
    store.save_worklist(&w).map_err(|e| format!("write failed: {e}"))
}

/// A defect: a task row with its evidence, fix and done-when, a member of the
/// open `retro-N`.
pub fn file_defect(store: &Store, title: &str, evidence: &str, fix: &str, done_when: &str, project: Option<&str>) -> Result<String, String> {
    let w = open_list(store, DEFECTS)?;
    let t = new_row(store, title, &[("Evidence", evidence), ("Fix", fix), ("Done when", done_when)], project)?;
    join(store, w, &t.id, project)?;
    Ok(t.id)
}

/// A strategic question: a row on the open `retro-ask-N`, carrying a `wsp ask`
/// to the person (kind ask, 2-4 options, the recommendation first).
pub fn file_question(store: &Store, title: &str, evidence: &str, ask_json: &Value, project: Option<&str>) -> Result<String, String> {
    let choices = Choices::parse(ask_json)?;
    let options = ask_json["questions"].as_array().map(|qs| qs.iter().all(|q| q["options"].as_array().is_some_and(|o| (2..=4).contains(&o.len())))).unwrap_or(false);
    if !options {
        return Err("a retro question offers 2-4 options, the recommendation first".into());
    }
    let w = open_list(store, QUESTIONS)?;
    let t = new_row(store, title, &[("Evidence", evidence)], project)?;
    let mut q = message::Message::question(Party::wsp(), Kind::Note, &choices.sentence(), Waiting::new("", &t.id))
        .about(About::Task(t.id.clone()))
        .for_the_person();
    q.choices = Some(choices);
    message::raise(store, &q).map_err(|e| e.to_string())?;
    join(store, w, &t.id, project)?;
    Ok(t.id)
}

/// A note on an existing row, in its log.
pub fn add_note(store: &Store, row: &str, text: &str) -> Result<(), String> {
    let mut t = store.task(row).ok_or_else(|| format!("no such task `{row}`"))?;
    t.log(text);
    store.save_task(&t).map_err(|e| format!("write failed: {e}"))
}

/// Whether an answer turned the row down.
fn declined(m: &message::Message) -> bool {
    let Some(h) = m.closing() else { return false };
    if h.act != message::Act::Answered {
        return true;
    }
    let note = h.note.trim().to_ascii_lowercase();
    // The picked label when the answer carries one; the note leads with the header.
    let picked = h.choice.as_ref().and_then(|c| c["answers"].as_object()).and_then(|a| a.values().next()).and_then(|q| q["pick"][0].as_str()).map(str::to_ascii_lowercase);
    let note = picked.unwrap_or(note);
    let first = note.split(|c: char| !c.is_alphanumeric() && c != '\'').find(|w| !w.is_empty()).unwrap_or("");
    ["decline", "declined", "no", "reject", "rejected", "drop", "skip", "don't", "dont", "cancel"].contains(&first)
}

/// Every `retro-ask-N` whose asks are all answered: the approved rows stay
/// with Ed's decision in the row log, the declined are closed and leave the
/// list, and the list goes to planned. Idempotent — a planned list is done.
/// Returns the lists it settled.
pub fn sweep(store: &Store) -> Vec<String> {
    let questions = store.messages();
    let mut settled = Vec::new();
    for mut w in store.worklists() {
        if number_of(QUESTIONS, &w.id).is_none() || w.status() != WorklistStatus::Draft || w.is_planned() || w.is_cancelled() {
            continue;
        }
        let mut groups = w.groups();
        let members: Vec<String> = groups.iter().flat_map(|g| g.members.clone()).collect();
        if members.is_empty() {
            continue;
        }
        let asks: Vec<Option<&message::Message>> = members
            .iter()
            .map(|r| questions.values().filter(|m| m.shape() == Some(message::Shape::Question) && m.about.task() == Some(r.as_str())).max_by(|a, b| a.at.cmp(&b.at)))
            .collect();
        if asks.iter().any(|m| m.is_none_or(|m| m.is_open())) {
            continue;
        }
        let mut gone = Vec::new();
        for (row, m) in members.iter().zip(asks.into_iter().flatten()) {
            let said = m.closing().map(|h| h.note.clone()).unwrap_or_default();
            let Some(mut t) = store.task(row) else { continue };
            if declined(m) {
                t.log(&format!("retro question declined by Ed: {said} — closed, cancelled"));
                t.set_status(Status::Done);
                gone.push(row.clone());
            } else {
                t.log(&format!("retro question approved by Ed: {said}"));
            }
            let _ = store.save_task(&t);
        }
        for g in &mut groups {
            g.members.retain(|r| !gone.contains(r));
        }
        groups.retain(|g| !g.members.is_empty());
        w.log(&format!("every question answered — {} declined and closed", gone.len()));
        w.set_groups(&groups);
        w.set_planned();
        if store.save_worklist(&w).is_ok() {
            settled.push(w.id.clone());
        }
    }
    settled
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask() -> Value {
        json!({"kind": "ask", "questions": [{"question": "Shorten the barrier?", "header": "Barrier",
            "options": [{"label": "Yes, shorten", "description": "recommended"}, {"label": "No, keep", "description": "as is"}]}]})
    }

    fn start(store: &Store, id: &str) {
        let mut w = store.worklist(id).unwrap();
        w.set_status(WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
    }

    fn answer(store: &Store, row: &str, text: &str) {
        let q = store.messages().into_values().find(|m| m.about.task() == Some(row)).unwrap();
        let choice = json!({"answers": [{"label": text}]});
        let _ = message::answer_choice(store, &q.id, &Party::Human(None), text, &choice).or_else(|_| message::answer(store, &q.id, &Party::Human(None), text));
    }

    #[test]
    fn a_second_defect_after_the_first_list_started_opens_the_next() {
        let (_env, store) = crate::testkit::isolated_store("retro-series-defects");
        let a = file_defect(&store, "a", "e", "f", "d", None).unwrap();
        assert!(store.worklist("retro-1").unwrap().is_planned());
        assert_eq!(file_defect(&store, "b", "e", "f", "d", None).map(|_| ()), Ok(()));
        assert_eq!(store.worklist("retro-1").unwrap().groups()[0].members.len(), 2);
        start(&store, "retro-1");
        let before = store.worklist("retro-1").unwrap();
        let c = file_defect(&store, "c", "e", "f", "d", None).unwrap();
        assert_eq!(store.worklist("retro-1").unwrap().body, before.body);
        assert_eq!(store.worklist("retro-2").unwrap().groups()[0].members, vec![c]);
        assert!(a != "");
        assert!(store.worklist("retro-ask-1").is_none());
    }

    #[test]
    fn a_question_alone_opens_only_the_ask_series_and_the_next_one_after_a_start() {
        let (_env, store) = crate::testkit::isolated_store("retro-series-asks");
        let q = file_question(&store, "barrier?", "ev", &ask(), None).unwrap();
        assert!(store.worklist("retro").is_none() && store.worklist("retro-1").is_none());
        let w = store.worklist("retro-ask-1").unwrap();
        assert!(!w.is_planned());
        assert_eq!(w.groups()[0].members, vec![q]);
        start(&store, "retro-ask-1");
        file_question(&store, "again?", "ev", &ask(), None).unwrap();
        assert!(store.worklist("retro-ask-2").is_some());
        assert_eq!(store.worklist("retro-ask-1").unwrap().groups()[0].members.len(), 1);
    }

    #[test]
    fn answering_plans_the_list_keeps_the_approved_and_closes_the_declined() {
        let (_env, store) = crate::testkit::isolated_store("retro-series-sweep");
        let yes = file_question(&store, "keep?", "ev", &ask(), None).unwrap();
        let no = file_question(&store, "drop?", "ev", &ask(), None).unwrap();
        assert!(sweep(&store).is_empty());
        answer(&store, &yes, "Yes, shorten");
        assert!(sweep(&store).is_empty(), "one ask is still open");
        answer(&store, &no, "No, keep");
        assert_eq!(sweep(&store), vec!["retro-ask-1".to_string()]);
        let w = store.worklist("retro-ask-1").unwrap();
        assert!(w.is_planned());
        assert_eq!(w.groups()[0].members, vec![yes.clone()]);
        assert!(store.task(&yes).unwrap().body.contains("approved by Ed"));
        let t = store.task(&no).unwrap();
        assert_eq!(t.status(), Status::Done);
        assert!(t.body.contains("declined by Ed"));
        assert!(sweep(&store).is_empty(), "idempotent");
    }

    #[test]
    fn a_list_made_by_each_creator_gets_no_retro() {
        let (_env, store) = crate::testkit::isolated_store("retro-series-no-retro");
        place_retro(&store, "a-1", None).unwrap();
        assert!(store.worklist("retro-1").unwrap().no_retro, "the retros list");
        file_defect(&store, "d", "e", "f", "d", None).unwrap();
        let defects = store.worklists().into_iter().filter(|w| w.title.starts_with("Timing retro defects")).collect::<Vec<_>>();
        assert!(!defects.is_empty() && defects.iter().all(|w| w.no_retro), "the defects list");
        file_question(&store, "q?", "ev", &ask(), None).unwrap();
        assert!(store.worklist("retro-ask-1").unwrap().no_retro, "the questions list");
    }

    #[test]
    fn a_note_goes_in_the_rows_log() {
        let (_env, store) = crate::testkit::isolated_store("retro-series-note");
        let r = file_defect(&store, "a", "e", "f", "d", None).unwrap();
        add_note(&store, &r, "seen again in wsp-9").unwrap();
        assert!(store.task(&r).unwrap().body.contains("seen again in wsp-9"));
        assert!(add_note(&store, "nope-1", "x").is_err());
    }
}
