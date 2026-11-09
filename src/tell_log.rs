//! `wsp tell --log <address>`: the conversation with a post, read off the
//! `agent-told` events (`wsp-522`, the third part of comms).
//!
//! No new store. A tell writes an `agent-told` event (who, to whom, which
//! message, which it answers, the text); a tell held for a seat on a silent
//! machine writes one with `held: true`, and the pass that later releases or
//! drops it writes an `agent-told-settled` event for the pane. This module
//! reads those and nothing else, and is **pure over the events and a
//! [`Book`]**, so threading, held-then-delivered and the reading of an address
//! are proved from a fixture.
//!
//! An address is read two ways, because a post outlives its seat: by the
//! *labels* the events carry (`from: governor:pieces`, `target:
//! governor:pieces`, `target: pieces`), which cross every seat that held the
//! post, and by the *panes* [`crate::address::resolve`] finds in it now, which
//! catches an event written before the labels existed. A bare word is a row,
//! read as `seat:<row>`, or a pane.

use serde_json::{json, Value};

use crate::address::{self, Address, Book};
use crate::store::Store;
use wsp_base::util;

/// What became of a tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
    Delivered,
    /// Held for a seat the link to which is down, not yet released.
    Held,
    /// Held, then released (at).
    HeldThenDelivered(i64),
    /// Held, then dropped because nobody was coming for it (at).
    HeldThenDropped(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub at: i64,
    pub from: String,
    pub to: String,
    pub pane: String,
    pub id: String,
    pub re: Option<String>,
    pub text: String,
    pub chars: usize,
    pub fate: Fate,
    /// The item a tell to the person landed as.
    pub item: Option<String>,
    /// Set on each seat of a `list:` fan-out (`wsp-523`): its key and size.
    pub fan: Option<(String, usize)>,
}

impl Entry {
    /// The first line, cut short: what stands in for the text without `--full`.
    pub fn caption(&self) -> String {
        let line = self.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        match (line.is_empty(), line.chars().count() > 70) {
            (true, _) => format!("({} chars)", self.chars),
            (false, true) => format!("{}…", line.chars().take(69).collect::<String>()),
            (false, false) => line.to_string(),
        }
    }
}

fn s(d: &Value, k: &str) -> String {
    d.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// Every tell the events record, oldest first, with held ones settled.
pub fn entries(told: &[Value], settled: &[Value]) -> Vec<Entry> {
    let mut out: Vec<Entry> = told
        .iter()
        .map(|d| Entry {
            at: d.get("at").and_then(Value::as_i64).unwrap_or(0),
            from: s(d, "from"),
            to: s(d, "target"),
            pane: s(d, "pane"),
            id: s(d, "id"),
            re: d.get("re").and_then(Value::as_str).filter(|r| !r.is_empty()).map(str::to_string),
            text: s(d, "text"),
            chars: d.get("chars").and_then(Value::as_u64).unwrap_or(0) as usize,
            fate: match d.get("held").and_then(Value::as_bool) {
                Some(true) => Fate::Held,
                _ => Fate::Delivered,
            },
            item: d.get("item").and_then(Value::as_str).map(str::to_string),
            fan: d
                .get("fan")
                .and_then(Value::as_str)
                .map(|k| (k.to_string(), d.get("fan_of").and_then(Value::as_u64).unwrap_or(0) as usize)),
        })
        .collect();
    out.sort_by_key(|e| e.at);
    // The k-th settling on a pane is the k-th tell held for it: the spool is
    // released oldest first.
    let mut settles: Vec<(&str, &str, i64)> = settled
        .iter()
        .map(|d| {
            (d.get("pane").and_then(Value::as_str).unwrap_or(""), d.get("how").and_then(Value::as_str).unwrap_or(""), d.get("at").and_then(Value::as_i64).unwrap_or(0))
        })
        .collect();
    settles.sort_by_key(|(_, _, at)| *at);
    let mut used = vec![false; settles.len()];
    for e in out.iter_mut().filter(|e| e.fate == Fate::Held) {
        if let Some(i) = (0..settles.len()).find(|i| !used[*i] && settles[*i].0 == e.pane && settles[*i].2 >= e.at) {
            used[i] = true;
            e.fate = match settles[i].1 {
                "dropped" => Fate::HeldThenDropped(settles[i].2),
                _ => Fate::HeldThenDelivered(settles[i].2),
            };
        }
    }
    out
}

/// What an address is known by in the events, and the panes it holds now.
pub struct Reading {
    labels: Vec<String>,
    panes: Vec<String>,
}

pub fn reading(word: &str, book: &Book) -> Reading {
    let word = word.trim();
    let addr = address::parse(word).or_else(|| match book.tasks.iter().any(|t| t.id == word) {
        true => Some(Address::Seat(word.to_string())),
        false => None,
    });
    let mut labels = vec![word.to_string()];
    match &addr {
        Some(Address::Primary) => labels.push(crate::governors::PRIMARY.to_string()),
        Some(Address::Governor(scope)) => {
            labels.push(format!("governor:{scope}"));
            labels.push(scope.clone());
        }
        Some(Address::Seat(row)) => {
            labels.push(format!("seat:{row}"));
            labels.push(row.clone());
        }
        _ => {}
    }
    let panes = match &addr {
        Some(a) => address::resolve(a, book).map(|v| v.into_iter().map(|n| n.pane).collect()).unwrap_or_default(),
        None => vec![],
    };
    labels.sort();
    labels.dedup();
    Reading { labels, panes }
}

impl Reading {
    fn has(&self, e: &Entry) -> bool {
        let said = |x: &str| !x.is_empty() && (self.labels.iter().any(|l| l == x) || self.panes.iter().any(|p| p == x));
        said(&e.from) || said(&e.to) || said(&e.pane)
    }
}

/// The entries `reading` names, then the replies placed under what they answer.
/// Returns `(depth, entry)` in reading order.
pub fn thread<'a>(all: &'a [Entry], reading: &Reading) -> Vec<(usize, &'a Entry)> {
    let mine: Vec<&Entry> = all.iter().filter(|e| reading.has(e)).collect();
    // The first of a given id is the parent a `--re` names; a reply is only
    // placed under something that came before it.
    let parent = |i: usize| -> Option<usize> {
        let re = mine[i].re.as_ref()?;
        (0..i).find(|j| &mine[*j].id == re)
    };
    let mut out = Vec::new();
    fn walk<'a>(mine: &[&'a Entry], parent: &dyn Fn(usize) -> Option<usize>, i: usize, depth: usize, out: &mut Vec<(usize, &'a Entry)>) {
        out.push((depth, mine[i]));
        for c in (i + 1..mine.len()).filter(|c| parent(*c) == Some(i)) {
            walk(mine, parent, c, depth + 1, out);
        }
    }
    for i in (0..mine.len()).filter(|i| parent(*i).is_none()) {
        walk(&mine, &parent, i, 0, &mut out);
    }
    out
}

fn fate_word(f: &Fate) -> String {
    match f {
        Fate::Delivered => "delivered".into(),
        Fate::Held => "held".into(),
        Fate::HeldThenDelivered(at) => format!("held, then delivered {}", util::local_hm(*at)),
        Fate::HeldThenDropped(at) => format!("held, then dropped {}", util::local_hm(*at)),
    }
}

/// The log as lines: one per tell, indented under what it answers.
pub fn lines(rows: &[(usize, &Entry)], full: bool) -> Vec<String> {
    let mut out = Vec::new();
    // A fan-out is one line: the first seat's entry stands for the lot, and the
    // others of the same key are folded into its count.
    let mut seen: Vec<&str> = Vec::new();
    for (depth, e) in rows {
        if let Some((key, _)) = &e.fan {
            if seen.contains(&key.as_str()) {
                continue;
            }
            seen.push(key);
        }
        let pad = "  ".repeat(*depth);
        let arrow = if *depth > 0 { "↳ " } else { "" };
        let re = match (&e.re, depth) {
            (Some(r), 0) => format!(" (re {r})"),
            _ => String::new(),
        };
        let to = match &e.item {
            Some(i) => format!("{} (item {i})", e.to),
            None => e.to.clone(),
        };
        let state = match &e.fan {
            Some((_, n)) => format!("fan-out to {n} seats"),
            None => fate_word(&e.fate),
        };
        let id = match &e.fan {
            Some(_) => String::new(),
            None => e.id.clone(),
        };
        out.push(format!(
            "{pad}{arrow}{}  {} → {}  {}  [{}] {}{re}",
            util::local_hm(e.at),
            if e.from.is_empty() { "?" } else { &e.from },
            to,
            e.caption(),
            state,
            id,
        ));
        if full && !e.text.is_empty() {
            for l in e.text.lines() {
                out.push(format!("{pad}    {l}"));
            }
        }
    }
    out
}

/// `wsp tell --log <address>`.
pub fn show(store: &Store, address: &str, full: bool, json: bool) -> i32 {
    let all = entries(&store.events_of("agent-told"), &store.events_of("agent-told-settled"));
    let rd = reading(address, &Book::read(store));
    let rows = thread(&all, &rd);
    if json {
        let v: Vec<Value> = rows
            .iter()
            .map(|(depth, e)| {
                json!({
                    "at": e.at, "from": e.from, "to": e.to, "pane": e.pane, "id": e.id, "re": e.re,
                    "fan": e.fan.as_ref().map(|f| f.1), "depth": depth, "fate": fate_word(&e.fate), "caption": e.caption(),
                    "text": if full { Some(&e.text) } else { None },
                })
            })
            .collect();
        println!("{}", Value::Array(v));
        return 0;
    }
    if rows.is_empty() {
        println!("nothing has been told to or by `{address}`");
        return 0;
    }
    for l in lines(&rows, full) {
        println!("{l}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn told(at: i64, from: &str, to: &str, pane: &str, id: &str, re: Option<&str>, text: &str) -> Value {
        json!({ "at": at, "from": from, "target": to, "pane": pane, "id": id, "re": re, "text": text, "chars": text.len(), "turn": true })
    }

    fn book() -> Book {
        let mut b = Book::default();
        b.projects = vec!["wsp".into()];
        b.governors.insert("pieces".into(), json!({ "workspace": "w9", "pane": "cpd-30", "since": "2026-11-09" }));
        b
    }

    #[test]
    fn a_tell_and_its_reply_are_threaded() {
        let events = vec![
            told(100, "primary", "governor:pieces", "cpd-20", "m-1", None, "start the cut"),
            told(90, "seat:wsp-1", "primary", "cpd-1", "m-0", None, "unrelated, earlier"),
            told(200, "governor:pieces", "primary", "cpd-1", "m-2", Some("m-1"), "done"),
            told(300, "primary", "governor:pieces", "cpd-20", "m-3", None, "thanks"),
        ];
        let all = entries(&events, &[]);
        let rows = thread(&all, &reading("governor:pieces", &book()));
        let shape: Vec<(usize, &str)> = rows.iter().map(|(d, e)| (*d, e.id.as_str())).collect();
        assert_eq!(shape, vec![(0, "m-1"), (1, "m-2"), (0, "m-3")], "the reply sits under what it answers");
        let text = lines(&rows, false).join("\n");
        assert!(text.contains("↳ "), "{text}");
        assert!(text.contains("primary → governor:pieces") && text.contains("start the cut"), "{text}");
        assert!(text.contains("[delivered]"), "{text}");
        assert!(!text.contains("unrelated"), "another post's tell is not in this log");
    }

    #[test]
    fn full_shows_the_text_and_the_default_a_caption() {
        let long = format!("first line\nsecond line {}", "x".repeat(10));
        let all = entries(&[told(1, "primary", "primary", "cpd-1", "m-1", None, &long)], &[]);
        let rd = reading("primary", &book());
        let rows = thread(&all, &rd);
        assert!(!lines(&rows, false).join("\n").contains("second line"));
        assert!(lines(&rows, true).join("\n").contains("second line"));
    }

    #[test]
    fn a_held_tell_is_held_and_then_delivered() {
        let held = json!({ "at": 100, "from": "primary", "target": "seat:wsp-9", "pane": "cpd-9", "id": "m-h", "text": "carry on", "chars": 8, "held": true });
        let before = entries(std::slice::from_ref(&held), &[]);
        assert_eq!(before[0].fate, Fate::Held);
        assert!(lines(&thread(&before, &reading("seat:wsp-9", &book())), false)[0].contains("[held]"));

        let settled = json!({ "pane": "cpd-9", "how": "delivered", "at": 160 });
        let after = entries(std::slice::from_ref(&held), std::slice::from_ref(&settled));
        assert_eq!(after[0].fate, Fate::HeldThenDelivered(160));
        assert!(lines(&thread(&after, &reading("seat:wsp-9", &book())), false)[0].contains("held, then delivered"));

        let dropped = json!({ "pane": "cpd-9", "how": "dropped", "at": 170 });
        assert_eq!(entries(&[held.clone()], &[dropped])[0].fate, Fate::HeldThenDropped(170));
        let elsewhere = json!({ "pane": "cpd-8", "how": "delivered", "at": 160 });
        assert_eq!(entries(&[held], &[elsewhere])[0].fate, Fate::Held, "another pane's release settles nothing here");
    }

    #[test]
    fn a_governor_address_reads_across_the_seats_that_held_the_post() {
        let events = vec![
            told(10, "primary", "governor:pieces", "cpd-20", "m-1", None, "to the first governor"),
            told(20, "primary", "governor:pieces", "cpd-30", "m-2", None, "to the second"),
            // `govern --tell` records the scope, not the address.
            told(30, "person", "pieces", "cpd-30", "m-3", None, "from govern"),
            // An old event with no labels, at the seat there now.
            json!({ "at": 40, "target": "", "pane": "cpd-30", "id": "m-4", "chars": 3 }),
            told(50, "primary", "governor:other", "cpd-40", "m-5", None, "not this post"),
        ];
        let all = entries(&events, &[]);
        let rows = thread(&all, &reading("governor:pieces", &book()));
        let ids: Vec<&str> = rows.iter().map(|(_, e)| e.id.as_str()).collect();
        assert_eq!(ids, vec!["m-1", "m-2", "m-3", "m-4"]);
        // Nobody seated now: the labels still read it.
        let rows = thread(&all, &reading("governor:pieces", &Book::default()));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn a_reply_to_something_not_in_this_log_stands_alone() {
        let all = entries(&[told(1, "seat:a", "primary", "cpd-1", "m-2", Some("m-gone"), "ok")], &[]);
        let rows = thread(&all, &reading("primary", &book()));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 0);
        assert!(lines(&rows, false)[0].contains("(re m-gone)"));
    }

    /// `wsp-523`: the seats of one `list:` fan-out are one line with the count.
    #[test]
    fn a_fan_out_is_one_line_with_its_count() {
        let mut evs: Vec<Value> = (1..=3)
            .map(|i| {
                let mut d = told(100, "governor:pieces", "list:run", &format!("cpd-{i}"), &format!("m-{i}"), None, "stand by");
                d["fan"] = json!("k1");
                d["fan_of"] = json!(3);
                d
            })
            .collect();
        evs.push(told(200, "governor:pieces", "list:run", "cpd-1", "m-9", None, "one more"));
        let all = entries(&evs, &[]);
        let rd = reading("list:run", &Book::default());
        let rows = thread(&all, &rd);
        assert_eq!(rows.len(), 4);
        let out = lines(&rows, false);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].contains("fan-out to 3 seats"), "{out:?}");
        assert!(out[1].contains("m-9"), "{out:?}");
    }
}
