//! What `wsp yours` says of a finished timing retro (`wsp-472`), and the
//! `retro.json` it reads. Apart from `retro_run` so that the engine can ask it
//! without reaching up to the report: it reads the artefacts directory itself.

use serde_json::Value;

use crate::store::Store;
use crate::util;

pub const FILE: &str = "retro.json";

/// The retro state of a list, or null when there is none.
pub fn state(slug: &str) -> Value {
    std::fs::read_to_string(crate::artefact::root().join(slug).join(FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

pub fn arr<'a>(s: &'a Value, k: &str) -> &'a [Value] {
    s[k].as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// The lines `wsp yours` adds: one per list whose retro finished in the last
/// two days.
pub fn yours_lines(store: &Store) -> Vec<String> {
    let mut out = Vec::new();
    for w in store.worklists() {
        if w.retro.is_none() {
            continue;
        }
        let s = state(&w.id);
        if s["done"] == false && util::since(s["started"].as_str().unwrap_or("")) <= 2 * 86400 {
            if let (Some(row), Some(on)) = (s["row"].as_str(), s["retros"].as_str()) {
                out.push(format!("{} cleared; its retro {row} is on {on}", w.id));
            }
            continue;
        }
        if s["done"] != true || util::since(s["finished"].as_str().unwrap_or("")) > 2 * 86400 {
            continue;
        }
        let of = |k: &str| -> (usize, String) {
            let mine: Vec<&Value> = arr(&s, "filed").iter().filter(|f| f["kind"] == k).collect();
            (mine.len(), mine.first().and_then(|f| f["list"].as_str()).unwrap_or("—").to_string())
        };
        let (k, dl) = of("defect");
        let (q, ql) = of("question");
        let n = k + q + arr(&s, "notes").len();
        out.push(format!(
            "{} cleared in {}; {n} causes; {k} defects on {dl}; {q} questions on {ql}",
            w.id,
            util::duration_human(s["wall"].as_i64().unwrap_or(0))
        ));
    }
    out
}
