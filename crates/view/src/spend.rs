//! `wsp spend --json` as a pure function: the stop ledger's lines, the places
//! of the seats that spent them and the landed-row count are inputs the CLI
//! reads; the clock is an argument.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use crate::common::iso_at;

const DAY: i64 = 86_400;
const WINDOW: i64 = 30;

/// One ledger line: an ended seat and what it spent.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Line {
    pub seat: String,
    pub ended: i64,
    pub model: String,
    pub micros: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub turns: u64,
}

/// What a seat held.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Place {
    pub list: Option<String>,
    pub project: Option<String>,
}

/// The tier family a model id is billed as.
pub fn family(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    ["opus", "sonnet", "haiku"].into_iter().find(|f| m.contains(f)).unwrap_or("other")
}

/// Tokens, cost and sessions summed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sum {
    pub micros: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub sessions: u64,
    pub turns: u64,
}

impl Sum {
    fn add(&mut self, l: &Line) {
        self.micros += l.micros;
        self.input += l.input;
        self.output += l.output;
        self.cache_read += l.cache_read;
        self.cache_write += l.cache_write;
        self.sessions += 1;
        self.turns += l.turns;
    }
    fn of<'a>(lines: impl Iterator<Item = &'a Line>) -> Sum {
        let mut s = Sum::default();
        lines.for_each(|l| s.add(l));
        s
    }
    fn json(&self) -> Value {
        json!({
            "cost": usd(self.micros), "input": self.input, "output": self.output,
            "cache_read": self.cache_read, "cache_write": self.cache_write,
            "sessions": self.sessions, "turns": self.turns,
        })
    }
}

fn usd(micros: u64) -> f64 {
    (micros as f64 / 10_000.0).round() / 100.0
}

/// The report as of `now`, in the shape compound draws. `lines` are the ledger
/// lines inside the window; `places` the seats' places; `landed` the rows with
/// a `holds` decided in it.
pub fn report(lines: &[Line], places: &HashMap<String, Place>, landed: usize, now: i64) -> Value {
    let today = now - now.rem_euclid(DAY);
    let since = today - (WINDOW - 1) * DAY;

    let mut daily = Vec::new();
    for k in 0..WINDOW {
        let d0 = since + k * DAY;
        let mut by: BTreeMap<&str, Sum> = ["opus", "sonnet", "haiku", "other"].into_iter().map(|f| (f, Sum::default())).collect();
        for l in lines.iter().filter(|l| l.ended >= d0 && l.ended < d0 + DAY) {
            by.entry(family(&l.model)).or_default().add(l);
        }
        daily.push(json!({
            "date": &iso_at(d0)[..10],
            "by_model": by.iter().map(|(k, s)| (k.to_string(), s.json())).collect::<serde_json::Map<_, _>>(),
        }));
    }
    let from = |start: i64| Sum::of(lines.iter().filter(|l| l.ended >= start));
    let all = from(since);

    let mut by_project: BTreeMap<String, Sum> = BTreeMap::new();
    let mut by_list: BTreeMap<String, (Option<String>, Sum)> = BTreeMap::new();
    let mut list_today: BTreeMap<String, u64> = BTreeMap::new();
    for l in lines {
        let p = places.get(&l.seat).cloned().unwrap_or_default();
        by_project.entry(p.project.clone().unwrap_or_else(|| "unplaced".into())).or_default().add(l);
        if let Some(list) = p.list {
            if l.ended >= today {
                *list_today.entry(list.clone()).or_default() += l.micros;
            }
            by_list.entry(list).or_insert_with(|| (p.project.clone(), Sum::default())).1.add(l);
        }
    }
    let mut projects: Vec<(String, Sum)> = by_project.into_iter().collect();
    projects.sort_by(|a, b| b.1.micros.cmp(&a.1.micros));
    let mut lists: Vec<(String, (Option<String>, Sum))> = by_list.into_iter().collect();
    lists.sort_by(|a, b| b.1 .1.micros.cmp(&a.1 .1.micros));

    json!({
        "today": &iso_at(today)[..10], "currency": "USD",
        "daily": daily,
        "totals": { "today": from(today).json(), "7d": from(today - 6 * DAY).json(), "30d": all.json() },
        "projects": projects.iter().map(|(id, s)| json!({ "id": id, "cost": usd(s.micros), "output": s.output, "sessions": s.sessions })).collect::<Vec<_>>(),
        "lists": lists.iter().map(|(slug, (p, s))| json!({ "slug": slug, "project": p, "cost": usd(s.micros), "today": usd(list_today.get(slug).copied().unwrap_or(0)), "output": s.output, "sessions": s.sessions })).collect::<Vec<_>>(),
        "rows": { "landed_30d": landed, "cost_per_landed_row": (landed > 0).then(|| (usd(all.micros) / landed as f64 * 100.0).round() / 100.0) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_is_billed_as_its_family() {
        assert_eq!(family("claude-OPUS-4"), "opus");
        assert_eq!(family("claude-sonnet-5-5"), "sonnet");
        assert_eq!(family("gpt"), "other");
    }

    #[test]
    fn no_lines_is_zeros_and_a_null_cost_per_row() {
        let r = report(&[], &HashMap::new(), 0, 1_700_000_000);
        assert_eq!(r["daily"].as_array().unwrap().len(), 30);
        assert_eq!(r["rows"], json!({ "landed_30d": 0, "cost_per_landed_row": null }));
    }
}
