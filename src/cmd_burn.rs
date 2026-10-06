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
//!
//! **The fleet is both backends' seats.** A hook tallies into the directory of
//! whichever backend opened the seat, and there are two of those, so a report
//! that read one described a fraction of the machine while looking like the
//! whole of it — eight seats holding real records, and a table that said
//! nothing had been spent (`wsp-116`). [`fleet`] is where that fold lives and
//! why it is two calls.

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

/// When [`PRICES`] was last checked against the first-party price reference.
/// Printed under the report, so the reader sees the age of the quote beside
/// the money it produced.
const PRICES_READ: &str = "2026-08-26";

/// **List prices, first-party Anthropic API, read on [`PRICES_READ`].** This is
/// the thing in wsp that goes stale without saying so, which is why it is a
/// table with a date on it rather than a constant buried in an expression — the
/// same bargain the cache multipliers below make, and the reason `core-049`'s
/// `CACHE_DISCOUNT` was written down instead of inlined.
///
/// **A reading date does not protect an introductory price.** A list price
/// goes stale unpredictably; an introductory one goes stale on a date that is
/// known the day the row is typed. `core-056`: Sonnet 5 was entered at its
/// introductory $2/$10, which ended 2026-08-31, and every sonnet seat was
/// under-billed by a third from the next day. A row priced on a promotion says
/// so and carries its end date beside it; a row with no such note is a
/// standing rate.
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
    // Sonnet 4.x and Sonnet 5 were once not the same price, and the fleet runs
    // both: `d5f0956` starts work under a seat on sonnet while seats stay on
    // the settings tier, so this is the row most likely to be read. Sonnet 5's
    // introductory $2/$10 ended 2026-08-31; the standing rate is $3/$15, the
    // same as 4.x, and the 4.x row stays so the two can diverge again.
    ("claude-sonnet-4", Price { input: 3_000_000, output: 15_000_000 }),
    ("claude-sonnet", Price { input: 3_000_000, output: 15_000_000 }),
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
    // Every backend, not one — the fold is the fix and the reason is in this
    // file's docs; asking a single one is `wsp-116`.
    let seats = fleet();
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
        // Says what it means rather than how it found out. "No seated agent has
        // spoken through a hook" was both narrower than the truth — the fold
        // above now looks at every backend, so an empty table is the whole
        // machine — and a claim about a mechanism, in a report whose one job is
        // to be read as an amount.
        println!(
            "{}",
            p.dim("no burn recorded — no seat on this machine has spent anything this can see")
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
        p.dim(&format!(
            "cost at Anthropic list prices read {PRICES_READ}, per request on the model that served it"
        ))
    );
    println!(
        "{}",
        p.dim("MODEL is the tier the seat is on now · an unrecognised one prices at the dearest rate")
    );
    0
}

/// Every seat on this machine that has a burn record, whichever backend opened
/// it, by seat id.
///
/// **Two sources, because a seat's cost is tallied into the directory of
/// whichever backend opened it and there are two of those** — a report that
/// read one of them described a fraction of the machine while looking like the
/// whole of it (`wsp-116`). This is the same fold `sync.rs`, `cmd_watch` and
/// `cmd_checkout` already make over herdr and compound, for the same reason: a
/// source nobody folds is a source nobody is looking at.
///
/// The concatenation is sound rather than lucky because seat ids are disjoint
/// by construction — each backend mints under its own prefix, `sup-` and `cpd-`
/// — so no row can be counted twice, and the SEAT column says which backend a
/// spend came from.
///
/// Split out from [`burn`] so a test can plant seats in an isolated store and
/// ask the question the bug was about — *is the fleet here?* — without standing
/// a terminal up to read a table off stdout.
fn fleet() -> Vec<(String, serde_json::Value)> {
    let mut seats = Supervisor::new().burn();
    seats.extend(crate::place_compound::Compound::new().burn());
    seats.sort_by(|a, b| a.0.cmp(&b.0));
    seats
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Plant a burn record where a backend would have written one.
    fn seat(state: &std::path::Path, dir: &str, id: &str, turns: u64, output: u64) {
        let d = state.join(dir).join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("burn.json"),
            json!({ "turns": turns, "output": output, "cost": output * 100 }).to_string(),
        )
        .unwrap();
    }

    /// `core-056`: Sonnet 5's introductory rate ended 2026-08-31. The standing
    /// rate is $3/$15, and a sonnet request must not price below it.
    #[test]
    fn sonnet_five_is_priced_at_its_standing_rate() {
        for model in ["claude-sonnet-5", "claude-sonnet-5-5", "claude-sonnet-4-5"] {
            assert_eq!(cost(model, 1_000_000, 1_000_000, 0, 0), 18_000_000, "{model}");
        }
    }

    /// **The bug, as a sentence.** A machine whose seats are all compound seats
    /// has no `seats/` directory at all — that is not a corruption, it is what
    /// a machine that has only ever run compound looks like — and the report
    /// used to read that one directory and say, in the present tense, that
    /// nothing had been spent. Eight seats held real records while it said so.
    #[test]
    fn a_fleet_of_only_compound_seats_is_still_a_fleet() {
        let iso = crate::util::isolated("burn-compound-only");
        assert!(
            !iso.state().join("seats").exists(),
            "the premise: the headless root is not merely empty, it is absent"
        );
        seat(&iso.state(), "compound-seats", "cpd-62", 148, 121_519);
        seat(&iso.state(), "compound-seats", "cpd-67", 16, 4_677);

        let read = fleet();
        assert_eq!(
            read.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["cpd-62", "cpd-67"],
            "both compound seats, ranked by what they cost"
        );
    }

    /// Both backends' seats, in one table, and neither counted twice. The
    /// prefixes are what make the concatenation sound, so this is the test that
    /// says so rather than leaving it to a comment.
    #[test]
    fn both_backends_seats_are_in_one_report_without_double_counting() {
        let iso = crate::util::isolated("burn-both-backends");
        seat(&iso.state(), "seats", "sup-1", 5, 500);
        seat(&iso.state(), "compound-seats", "cpd-2", 7, 700);

        let read = fleet();
        assert_eq!(
            read.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["cpd-2", "sup-1"],
            "one row per seat, and neither backend's seat shadowing the other's"
        );
        // A seat id is the join key between a report row and the seat it is
        // about, so two rows claiming one id would be a report that cannot be
        // acted on rather than merely a wrong total.
        let mut ids: Vec<&str> = read.iter().map(|(id, _)| id.as_str()).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "every seat id in the report is distinct");
    }

    /// A machine with no seats at all is the case the empty message is for, and
    /// it must not be reachable by a report that simply failed to look. So: the
    /// fold finds nothing *because there is nothing*, which is asserted by
    /// planting both roots and emptying both.
    #[test]
    fn an_empty_report_means_both_roots_were_read_and_were_empty() {
        let iso = crate::util::isolated("burn-nothing");
        for dir in ["seats", "compound-seats"] {
            std::fs::create_dir_all(iso.state().join(dir)).unwrap();
        }
        assert!(
            fleet().is_empty(),
            "both roots present and empty is the only route to an empty report"
        );
        // The near-miss that produced the bug: one root existing and the other
        // absent is indistinguishable from *both* absent unless the reader is
        // the one naming the root.
        seat(&iso.state(), "compound-seats", "cpd-1", 1, 1);
        assert_eq!(fleet().len(), 1, "and the root that does exist is still read");
    }
}
