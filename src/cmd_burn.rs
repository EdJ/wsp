//! `wsp burn` — where the tokens went, seat by seat.
//!
//! Every request an agent makes re-reads its whole context, so a night's bill
//! is decided less by how much an agent *printed* than by what rode along in
//! its window and for how long — the arithmetic [`crate::cmd_brief::Depth`]
//! records and `core-049` was filed on. The per-seat totals are tallied
//! already: every hook a seated agent fires adds the new transcript slice to
//! its running count, so nothing here parses anything. What is left is the
//! ranking, because a number nobody can compare is a number nobody acts on.
//!
//! Read-only, like everything else that only reports.

use serde_json::json;

use crate::place_super::Supervisor;
use crate::util::Paint;
use crate::Args;

/// Cache reads are billed at roughly a tenth of a fresh input token; the column
/// says billed, not raw, and this is the ratio between them. Stated rather
/// than hidden — it is a pricing judgement, not a measurement.
const CACHE_DISCOUNT: u64 = 10;

pub(crate) fn billed(input: u64, output: u64, cache_read: u64, cache_write: u64) -> u64 {
    input + output + cache_write + cache_read / CACHE_DISCOUNT
}

fn n(v: &serde_json::Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

pub fn burn(_store: &crate::store::Store, args: &Args) -> i32 {
    let seats = Supervisor::new().burn();
    if args.json() {
        let rows: Vec<serde_json::Value> = seats
            .iter()
            .map(|(id, r)| {
                json!({
                    "seat": id,
                    "model": r.get("model").cloned().unwrap_or(json!("")),
                    "turns": n(r, "turns"),
                    "input": n(r, "input"),
                    "output": n(r, "output"),
                    "cache_read": n(r, "cache_read"),
                    "cache_write": n(r, "cache_write"),
                    "billed": billed(n(r, "input"), n(r, "output"), n(r, "cache_read"), n(r, "cache_write")),
                    "updated": r.get("updated").cloned().unwrap_or(json!("")),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    let mut rows: Vec<(u64, String, String, u64, u64, u64, u64, u64)> = seats
        .iter()
        .map(|(id, r)| {
            let (i, o, cr, cw) = (
                n(r, "input"),
                n(r, "output"),
                n(r, "cache_read"),
                n(r, "cache_write"),
            );
            (
                billed(i, o, cr, cw),
                id.clone(),
                str_or_dash(r, "model"),
                n(r, "turns"),
                i,
                o,
                cr,
                cw,
            )
        })
        .collect();
    // The expensive seat first: the question this answers is *where*, and the
    // answer is whatever is at the top.
    rows.sort_by(|a, b| b.0.cmp(&a.0));

    if rows.is_empty() {
        println!(
            "{}",
            p.dim("no burn recorded — no seated agent has spoken through a hook yet")
        );
        return 0;
    }
    let header = format!(
        "{:<8}  {:<28} {:>5}  {:>9}  {:>9}  {:>8}  {:>10}  {:>10}",
        "SEAT", "MODEL", "TURNS", "BILLED", "INPUT", "OUTPUT", "CACHE-READ", "CACHE-WRITE"
    );
    println!("{}", p.dim(&header));
    for (billed, id, model, turns, i, o, cr, cw) in &rows {
        println!(
            "{:<8}  {:<28} {:>5}  {:>9}  {:>9}  {:>8}  {:>10}  {:>10}",
            id,
            truncate(model, 28),
            turns,
            thousands(*billed),
            thousands(*i),
            thousands(*o),
            thousands(*cr),
            thousands(*cw)
        );
    }
    println!(
        "{}",
        p.dim("billed ≈ input + output + cache-write + cache-read/10 · wsp burn --json for the counts")
    );
    0
}

fn str_or_dash(v: &serde_json::Value, key: &str) -> String {
    match v.get(key).and_then(|x| x.as_str()).unwrap_or("") {
        "" => "—".to_string(),
        s => s.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
