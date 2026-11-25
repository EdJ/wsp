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

/// Carry a renumbering into every list's retro record (`wsp-351`).
///
/// [`Store::rename_tasks`] rewrites the records and the state files, but
/// `retro.json` lives under the artefacts root, which the store does not know:
/// a retro row filed out of the inbox kept its old id in `s["row"]` while the
/// list's `retro:` line said the new one, and `retro_run::wrong_seat` reads
/// `s["row"]` first, so every retro verb refused the seat. The filed rows and
/// notes name rows by id too and follow the same way, as do the links in
/// `retro.html`. Both are rewritten whole-token and written through
/// [`crate::artefact::put_bytes`], so each keeps its versions. Here, in the
/// engine, rather than in `retro_run` (timeline) because `wsp mv` calls it.
/// Answers the lists rewritten, or the first write that failed.
pub fn follow_renames(store: &Store, map: &std::collections::BTreeMap<String, String>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    if map.is_empty() {
        return Ok(out);
    }
    let root = crate::artefact::root();
    let note = format!("renumbered: {}", map.iter().map(|(a, b)| format!("{a} → {b}")).collect::<Vec<_>>().join(", "));
    for w in store.worklists() {
        let mut touched = false;
        for file in [FILE, "retro.html"] {
            let Ok(text) = std::fs::read_to_string(root.join(&w.id).join(file)) else { continue };
            let (text, n) = crate::store::substitute_tokens(&text, map);
            if n == 0 {
                continue;
            }
            if file == FILE {
                serde_json::from_str::<Value>(&text).map_err(|e| format!("{}'s {file}: {e}", w.id))?;
            }
            let who = crate::artefact::Who { row: Some(w.id.clone()), seat: Some("wsp".to_string()) };
            crate::artefact::put_bytes(&root, &w.id, file, text.as_bytes(), &note, &who, false)?;
            touched = true;
        }
        if touched {
            out.push(w.id.clone());
        }
    }
    Ok(out)
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
