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
//! **The ranking is in money, and it has to be.** `core-049` shipped this
//! sorted on a token count summed across models, at a moment when the fleet was
//! one tier; `d5f0956` made it deliberately heterogeneous — work under a seat
//! starts on sonnet, seats stay on the settings tier — and there is roughly an
//! order of magnitude between the tiers per token. A cheap seat with eight
//! times the volume outranked an expensive one that had cost more, which is the
//! one answer this report exists to prevent: `core-049`'s own acceptance is
//! that tier choice becomes measurable rather than argued, and a token count is
//! precisely what cannot measure it.
//!
//! Read-only, like everything else that only reports.

use serde_json::json;

use crate::place_super::Supervisor;
use crate::util::Paint;
use crate::Args;

/// What a million tokens costs, in micro-dollars, on one tier.
///
/// Micro-dollars because the unit has to divide exactly: every rate below is a
/// whole number of dollars per million tokens, and the two cache multipliers
/// are a tenth and a quarter, so integer arithmetic in millionths loses
/// nothing. Floating point in a ranking would be a rounding argument nobody
/// asked for.
struct Price {
    input: u64,
    output: u64,
}

/// **List prices, first-party Anthropic API, read on 2026-08-26.** This is the
/// thing in wsp that goes stale without saying so, which is why it is a table
/// with a date on it rather than a constant buried in an expression — the same
/// bargain the cache multipliers below make, and the reason `core-049`'s
/// `CACHE_DISCOUNT` was written down instead of inlined.
///
/// Matched as a prefix, longest-specific first, because a model id carries a
/// date or a variant wsp has no business parsing (`claude-opus-5`,
/// `claude-haiku-4-5-20251001`). A model that matches nothing is priced at the
/// **first row**, which is the dearest: a report answering *where did the money
/// go* must never make an unrecognised spender look cheap, and the model column
/// shows the name that was not recognised.
const PRICES: &[(&str, Price)] = &[
    ("claude-fable", Price { input: 10_000_000, output: 50_000_000 }),
    ("claude-mythos", Price { input: 10_000_000, output: 50_000_000 }),
    ("claude-opus", Price { input: 5_000_000, output: 25_000_000 }),
    // Sonnet 4.x and Sonnet 5 are not the same price, and the fleet runs both:
    // `d5f0956` starts work under a seat on sonnet while seats stay on the
    // settings tier, so this is the row most likely to be read.
    ("claude-sonnet-4", Price { input: 3_000_000, output: 15_000_000 }),
    ("claude-sonnet", Price { input: 2_000_000, output: 10_000_000 }),
    ("claude-haiku", Price { input: 1_000_000, output: 5_000_000 }),
];

/// Cache reads bill at a tenth of the input rate, cache writes at a quarter
/// above it. Stated rather than hidden, exactly as `core-049` stated its
/// tenth — the difference is that these are quoted rates rather than an
/// estimate, so the column can say dollars.
///
/// The write multiplier is the **five-minute** TTL. A one-hour entry writes at
/// twice the input rate, and Claude Code uses the hour: the split is in the
/// transcript (`usage.cache_creation.ephemeral_1h_input_tokens`) but not in the
/// `cache_creation_input_tokens` total the tally reads, so a seat that writes
/// its cache hourly is undercounted on that one line item. Named here rather
/// than guessed at; splitting the tally by TTL is worth doing the day cache
/// writes are the answer to *where did the money go*, and they are not.
const CACHE_READ: (u64, u64) = (1, 10);
const CACHE_WRITE: (u64, u64) = (5, 4);

/// What one request cost, in micro-dollars, on the model that served it.
///
/// Called from the tally rather than from the report ([`crate::place_super`]),
/// because a session that changed tier partway through has no single model to
/// price its totals against — the request does. The report reads the
/// accumulated number and does not re-derive it.
pub(crate) fn cost(model: &str, input: u64, output: u64, cache_read: u64, cache_write: u64) -> u64 {
    let p = PRICES
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, price)| price)
        .unwrap_or(&PRICES[0].1);
    let at = |tokens: u64, per_mtok: u64| tokens.saturating_mul(per_mtok) / 1_000_000;
    at(input, p.input)
        + at(output, p.output)
        + at(cache_read, p.input * CACHE_READ.0 / CACHE_READ.1)
        + at(cache_write, p.input * CACHE_WRITE.0 / CACHE_WRITE.1)
}

/// Micro-dollars as money, which is what the column is sorted on and therefore
/// what it has to be readable as. Two decimal places and no thousands
/// separator: a night that reaches four figures of dollars is a different
/// conversation than the one this report is for.
fn dollars(micros: u64) -> String {
    format!("${}.{:02}", micros / 1_000_000, (micros % 1_000_000) / 10_000)
}

/// What a seat has cost: its own tally where it has one, and the totals priced
/// at its current model where it does not.
///
/// The fallback is for a record written before the tally priced anything. Read
/// by the report *and* by the tally, which is the half that matters: a seat
/// that has been running all night when this lands carries totals and no cost,
/// and a tally that started its cost at zero would report the night as costing
/// whatever the next few turns did. Priced at the stored model, which for a
/// pre-existing record is the only model it remembers.
pub(crate) fn cost_of(r: &serde_json::Value) -> u64 {
    match r.get("cost").and_then(|v| v.as_u64()) {
        Some(c) => c,
        None => cost(
            r.get("model").and_then(|v| v.as_str()).unwrap_or(""),
            n(r, "input"),
            n(r, "output"),
            n(r, "cache_read"),
            n(r, "cache_write"),
        ),
    }
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
                    // Micro-dollars, so a reader doing arithmetic on it is
                    // doing it in integers. The text column rounds; this does
                    // not.
                    "cost": cost_of(r),
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
                cost_of(r),
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
        "SEAT", "MODEL", "TURNS", "COST", "INPUT", "OUTPUT", "CACHE-READ", "CACHE-WRITE"
    );
    println!("{}", p.dim(&header));
    for (cost, id, model, turns, i, o, cr, cw) in &rows {
        println!(
            "{:<8}  {:<28} {:>5}  {:>9}  {:>9}  {:>8}  {:>10}  {:>10}",
            id,
            truncate(model, 28),
            turns,
            dollars(*cost),
            thousands(*i),
            thousands(*o),
            thousands(*cr),
            thousands(*cw)
        );
    }
    // The provenance, because a price table is the one number here that rots
    // without any of its readers noticing: the columns are measurements and
    // this one is a quote with a date on it.
    println!(
        "{}",
        p.dim("cost at Anthropic list prices read 2026-08-26, per request on the model that served it")
    );
    println!(
        "{}",
        p.dim("MODEL is the tier the seat is on now · an unrecognised one prices at the dearest rate")
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
