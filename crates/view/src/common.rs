//! What every projection shares: a task's record read off a contract [`Row`],
//! the keys the mirror writes into `Row::fields`, and the pure helpers the
//! projections need that live in `wsp-base` and `wsp-store`, copied here
//! because this crate links neither.
//!
//! # The conversion contract
//!
//! A [`Row`] is what `src/mirror.rs`'s `rows` makes of a task file, and
//! `mirror::task_of` is the way back. The CLI builds its input with those same
//! projections (`mirror::rows`, `lists`, `claims`, `machines`) and web-chrome
//! receives them from the daemon's fold, so both hand this crate one shape.
//! What `rows` writes, per row, and what each reads as here:
//!
//! | key            | holds                                                  | read by            |
//! |----------------|--------------------------------------------------------|--------------------|
//! | `status`       | the status word (`Status::as_str`)                     | [`status`]         |
//! | `prio`         | the priority word (`Priority::as_str`)                 | [`priority`]       |
//! | `project`      | the file's own project, `""` for none (inbox)          | [`project`]        |
//! | `tags`         | the tags, joined by `,`                                | [`list`]           |
//! | `caption`      | `caption::of_task`'s text                              | [`caption`]        |
//! | `caption_from` | `caption::of_task`'s `from`: `authored` or `title`     | [`caption`]        |
//! | `parent`       | the parent's id, `""` for none                         | [`opt`]            |
//! | `after`        | the ids it rests on, joined by `,`                     | [`list`]           |
//! | `refs`         | the refs, joined by `,`                                | [`list`]           |
//! | `created`      | the stamp                                              | [`field`]          |
//! | `updated`      | the stamp                                              | [`field`]          |
//! | `claim`        | the `claims.json` record, JSON text, while claimed     | [`claim`]          |
//! | `body`         | the whole task file, for a row `wip` reads in full     | [`prose`]          |
//! | `head`         | `TaskHead::to_json`, for the rest                      | (wip's reading)    |
//! | `prose`        | the body after the front matter, beside `head`         | [`prose`]          |
//! | `stage`        | `stage::of_member`'s caption with no agent looked at, `Told::json` text | [`crate::stage::stage_of`] |
//! | `stage_steps`  | `stage::history`'s steps before the live word settles them, JSON array | [`crate::stage::stage_of`] |
//! | `legs`         | on a staged row, `legs::json`: the legs `show --json` gives, JSON text | [`crate::task::ls_entry`] |
//! | `stage_live`   | on a `doing` row whose word its seat decides: `{"said": …}`, what its agent last said | [`crate::stage::stage_of`] |
//!
//! And on a [`wsp_store_contract::Seat`], what `src/mirror.rs`'s
//! `seat_records` writes (wsp-076) and the stage overlay reads:
//!
//! | key            | holds                                                  | read by            |
//! |----------------|--------------------------------------------------------|--------------------|
//! | `binding`      | the seat's `bindings.json` record, JSON text           | [`crate::stage::bound_row`] |
//! | `census`       | the census state word (`place::State::as_str`)         | [`crate::stage::census_word`] |
//! | `governs`      | the scope the seat governs (`place::governs`), absent for none | [`crate::stage::agent_of`] |
//!
//! And on an [`wsp_store_contract::Ask`] (`mirror::ask_facts`), read by [`crate::asks`]:
//! `raised`, `person`, `wants`, `person_q` (`"true"` or absent), `item_project`,
//! `project_chain` (the project and its ancestors, joined by `,`; a review row
//! carries its own too), `unblocks`; and on the machine named `local`
//! `yours_unseen` (a count) and `yours_retro` (a JSON array).
//!
//! `Row::project` is not the file's project: it is `model::project_of`, which
//! names an inbox row's project after its id. [`project`] reads the file's.
//! A comma inside a tag, an `after` id or a ref is split by [`list`]; none is
//! written today.

use serde_json::{json, Value};
use wsp_store_contract::Row;

/// `Row::fields` keys, as `src/mirror.rs`'s `rows` writes them.
pub mod key {
    pub const STATUS: &str = "status";
    pub const PRIO: &str = "prio";
    pub const PROJECT: &str = "project";
    pub const TAGS: &str = "tags";
    pub const CAPTION: &str = "caption";
    pub const CAPTION_FROM: &str = "caption_from";
    pub const PARENT: &str = "parent";
    pub const AFTER: &str = "after";
    pub const REFS: &str = "refs";
    pub const CREATED: &str = "created";
    pub const UPDATED: &str = "updated";
    pub const CLAIM: &str = "claim";
    pub const BODY: &str = "body";
    pub const HEAD: &str = "head";
    pub const PROSE: &str = "prose";
    pub const STAGE: &str = "stage";
    pub const STAGE_STEPS: &str = "stage_steps";
    pub const STAGE_LIVE: &str = "stage_live";
    pub const LEGS: &str = "legs";
    /// Seat keys.
    pub const BINDING: &str = "binding";
    pub const CENSUS: &str = "census";
    pub const GOVERNS: &str = "governs";
}

/// A field's text, `""` when the row has none.
pub fn field<'a>(row: &'a Row, key: &str) -> &'a str {
    row.fields.get(key).map(String::as_str).unwrap_or("")
}

/// A field that may be absent: `None` for `""` or no key.
pub fn opt(row: &Row, key: &str) -> Option<String> {
    Some(field(row, key)).filter(|s| !s.is_empty()).map(str::to_string)
}

/// A field written joined by `,`: the empty list for `""`.
pub fn list(row: &Row, key: &str) -> Vec<String> {
    match field(row, key) {
        "" => Vec::new(),
        s => s.split(',').map(str::to_string).collect(),
    }
}

/// The status word, as `Status::as_str` says it; `todo` when unreadable, as
/// `Task::status` reads it.
pub fn status(row: &Row) -> &str {
    match field(row, key::STATUS) {
        "" => "todo",
        s => s,
    }
}

/// The priority word, as `Priority::as_str` says it; `normal` when absent.
pub fn priority(row: &Row) -> &str {
    match field(row, key::PRIO) {
        "" => "normal",
        s => s,
    }
}

/// Every status word, in `Status::rank`'s order (the order `ls` sorts by).
pub const STATUSES: [&str; 7] = ["blocked", "review", "doing", "todo", "inbox", "parked", "done"];

/// `Status::rank`: where a status word sorts; an unknown word sorts as `todo`.
pub fn status_rank(word: &str) -> u8 {
    STATUSES.iter().position(|s| *s == word).unwrap_or(3) as u8
}

/// `Status::is_open`: everything but `done`.
pub fn is_open(word: &str) -> bool {
    word != "done"
}

/// `Priority::rank`: `high` 0, `normal` 1, `low` 2.
pub fn priority_rank(word: &str) -> u8 {
    match word {
        "high" => 0,
        "low" => 2,
        _ => 1,
    }
}

/// The file's own project: `None` for an inbox row.
pub fn project(row: &Row) -> Option<String> {
    opt(row, key::PROJECT)
}

/// `caption::of_task(t).json()`: the line a glance reads and where it came
/// from. The mirror derives it; this reads it back.
pub fn caption(row: &Row) -> Value {
    let from = match field(row, key::CAPTION_FROM) {
        "" => "title",
        f => f,
    };
    json!({ "text": field(row, key::CAPTION), "from": from })
}

/// The `claims.json` record the row carries, while it is claimed.
pub fn claim(row: &Row) -> Option<Value> {
    serde_json::from_str(row.fields.get(key::CLAIM)?).ok()
}

/// `Task::json`: the record every task-shaped `--json` starts from (`ls`,
/// `find`, `show`, `wip`'s rows), byte for byte once printed.
pub fn task_json(row: &Row) -> Value {
    json!({
        "id": row.id.0,
        "title": row.title,
        "project": project(row),
        "tags": list(row, key::TAGS),
        "status": status(row),
        "priority": priority(row),
        "parent": opt(row, key::PARENT),
        "after": list(row, key::AFTER),
        "created": field(row, key::CREATED),
        "updated": field(row, key::UPDATED),
        "refs": list(row, key::REFS),
        "caption": caption(row),
    })
}

/// `Task::body`: the task's prose, after its front matter. From `prose` where
/// the mirror wrote it, else out of the whole file in `body`.
pub fn prose(row: &Row) -> String {
    if let Some(p) = row.fields.get(key::PROSE) {
        return p.clone();
    }
    after_front_matter(field(row, key::BODY)).to_string()
}

/// The text after a file's front matter: `wsp_base::fm::parse`'s split (its
/// `doc.body`), copied, leading blank lines and all taken off as it takes them.
/// A file with no `---` first line is all body, verbatim; one whose front
/// matter never closes has none.
pub fn after_front_matter(text: &str) -> &str {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---\n") else { return text };
    match rest.find("\n---") {
        Some(i) => rest[i + 4..].trim_start_matches('\n'),
        None => "",
    }
}

/// `wsp_store::model::section_of`, copied: a `## name` section's text,
/// trimmed at the end, matched without regard to case; `None` when absent or
/// blank.
pub fn section(body: &str, name: &str) -> Option<String> {
    sections(body).into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, t)| t).filter(|t| !t.trim().is_empty())
}

/// `wsp_store::model::has_section`, copied: whether a body has the heading at
/// all, empty or not.
pub fn has_section(body: &str, name: &str) -> bool {
    sections(body).iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// `wsp_store::model::split_sections`, copied: `(heading, text)` pairs, `""`
/// for anything before the first heading.
pub fn sections(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut name = String::new();
    let mut buf = String::new();
    for line in body.lines() {
        match line.strip_prefix("## ") {
            Some(h) => {
                if !name.is_empty() || !buf.trim().is_empty() {
                    out.push((name, buf.trim_end().to_string()));
                }
                name = h.trim().to_string();
                buf = String::new();
            }
            None => {
                buf.push_str(line);
                buf.push('\n');
            }
        }
    }
    if !name.is_empty() || !buf.trim().is_empty() {
        out.push((name, buf.trim_end().to_string()));
    }
    out
}

// ---- time, copied from `wsp_base::util` -------------------------------------
// Pure arithmetic on epoch seconds; the clock itself is an input (`now`).

/// `wsp_base::util::iso_at`, copied: `2026-08-14T16:22:51Z` for epoch seconds.
pub fn iso_at(secs: i64) -> String {
    let (y, m, d, hh, mm, ss) = parts(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// `wsp_base::util::epoch_of`, copied: epoch seconds for a stamp, a bare date
/// being midnight; 0 for a stamp that cannot be read ("no time recorded").
pub fn epoch_of(iso: &str) -> i64 {
    if iso.len() < 10 {
        return 0;
    }
    let n = |a: usize, b: usize| -> i64 { iso.get(a..b).and_then(|s| s.parse().ok()).unwrap_or(0) };
    let days = days_from_civil(n(0, 4), n(5, 7).max(1), n(8, 10).max(1));
    let (hh, mm, ss) = if iso.len() >= 19 { (n(11, 13), n(14, 16), n(17, 19)) } else { (0, 0, 0) };
    days * 86_400 + hh * 3600 + mm * 60 + ss
}

/// `wsp_base::util::since` with the clock handed in: seconds from a stamp to
/// `now`, never negative, 0 for no stamp.
pub fn since(iso: &str, now: i64) -> i64 {
    match epoch_of(iso) {
        0 => 0,
        then => (now - then).max(0),
    }
}

/// `wsp_base::util::age_days` with the clock handed in: whole days from an
/// ISO date (or stamp) to `now`'s day; 0 when it cannot be read.
pub fn age_days(iso: &str, now: i64) -> i64 {
    if iso.len() < 10 {
        return 0;
    }
    let y: i64 = iso[0..4].parse().unwrap_or(0);
    let m: i64 = iso[5..7].parse().unwrap_or(1);
    let d: i64 = iso[8..10].parse().unwrap_or(1);
    now.div_euclid(86_400) - days_from_civil(y, m, d)
}

/// `wsp_base::util::civil_from_days`, copied (Howard Hinnant's).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `wsp_base::util::days_from_civil`, copied.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `wsp_base::util::parts`, copied.
fn parts(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use wsp_store_contract::{ProjectId, RowId, RowState};

    fn row(fields: &[(&str, &str)]) -> Row {
        Row {
            workspace: Default::default(),
            id: RowId("inbox-001".into()),
            project: ProjectId("inbox".into()),
            title: "A row".into(),
            state: RowState::Todo,
            before_block: None,
            seat: None,
            group: None,
            commit: None,
            fields: fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn an_inbox_row_has_no_project_whatever_its_id_says() {
        let r = row(&[("project", ""), ("caption", "A row"), ("caption_from", "title")]);
        assert_eq!(task_json(&r)["project"], Value::Null);
        assert_eq!(task_json(&r)["caption"], json!({ "text": "A row", "from": "title" }));
    }

    #[test]
    fn lists_split_on_commas_and_empty_is_none() {
        let r = row(&[("tags", "a,b"), ("after", "")]);
        assert_eq!(list(&r, key::TAGS), ["a", "b"]);
        assert!(list(&r, key::AFTER).is_empty());
    }

    #[test]
    fn prose_is_the_field_or_the_file_after_its_front_matter() {
        assert_eq!(prose(&row(&[("prose", "## Log\n")])), "## Log\n");
        assert_eq!(prose(&row(&[("body", "---\nid: x\n---\n## Overview\nhi\n")])), "## Overview\nhi\n");
        assert_eq!(after_front_matter("---\nid: x\n---\n\n\n## Overview\n"), "## Overview\n", "as fm::parse trims");
        assert_eq!(after_front_matter("\n\nno front matter"), "\n\nno front matter");
        assert_eq!(after_front_matter("no front matter"), "no front matter");
        assert_eq!(after_front_matter("---\nunclosed\n"), "");
    }

    #[test]
    fn sections_read_as_the_store_reads_them() {
        let body = "## Overview\nhi\n\n## Log\n- a\n## Empty\n";
        assert_eq!(section(body, "log").as_deref(), Some("- a"));
        assert_eq!(section(body, "Empty"), None);
        assert!(has_section(body, "Empty"));
    }

    #[test]
    fn stamps_go_both_ways() {
        let at = 1_795_000_000;
        assert_eq!(iso_at(at), "2026-11-18T11:06:40Z");
        assert_eq!(epoch_of(&iso_at(at)), at);
        assert_eq!(epoch_of("2026-11-18"), at - (11 * 3600 + 6 * 60 + 40));
        assert_eq!(since("2026-11-18T11:00:00Z", at), 400);
        assert_eq!(age_days("2026-11-16", at), 2);
        assert_eq!(epoch_of("garbage"), 0);
    }

    #[test]
    fn status_and_priority_words_rank_as_the_store_ranks_them() {
        assert_eq!(status_rank("blocked"), 0);
        assert_eq!(status_rank("done"), 6);
        assert_eq!(status_rank("nonsense"), 3);
        assert_eq!(priority_rank("high"), 0);
        assert!(!is_open("done") && is_open("parked"));
    }
}
