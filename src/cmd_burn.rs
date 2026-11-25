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

use crate::place_super::{cost_of, Supervisor, PRICES_READ};
use crate::util::Paint;
use crate::base::args::Args;


/// Micro-dollars as money, which is what the column is sorted on and therefore
/// what it has to be readable as. Two decimal places and no thousands
/// separator: a night that reaches four figures of dollars is a different
/// conversation than the one this report is for.
fn dollars(micros: u64) -> String {
    format!("${}.{:02}", micros / 1_000_000, (micros % 1_000_000) / 10_000)
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
/// `cmd_checkout` already make over backend and compound, for the same reason: a
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

/// What a seat spent, exactly: its live `burn.json` if its directory still has
/// one, else the newest line the ledger (`<state>/spent.jsonl`, written by
/// `stop()`) holds for it. The record has `wsp burn --json`'s fields, so a
/// figure read through here matches `burn` by construction (wsp-264).
pub(crate) fn reading(seat: &str) -> Option<serde_json::Value> {
    reading_in(&crate::store::Store::open().state, seat)
}

pub(crate) fn reading_in(state: &std::path::Path, seat: &str) -> Option<serde_json::Value> {
    for root in ["seats", "compound-seats"] {
        let rec = crate::place_super::read_json(&state.join(root).join(seat).join("burn.json"));
        if n(&rec, "turns") > 0 {
            return Some(json!({
                "seat": seat,
                "model": rec.get("model").cloned().unwrap_or(json!("")),
                "turns": n(&rec, "turns"),
                "input": n(&rec, "input"),
                "output": n(&rec, "output"),
                "cache_read": n(&rec, "cache_read"),
                "cache_write": n(&rec, "cache_write"),
                "cost": cost_of(&rec),
                "session_id": rec.get("session_id").cloned().unwrap_or(json!("")),
                "transcript": rec.get("transcript").cloned().unwrap_or(json!("")),
            }));
        }
    }
    ledger_lines(state, seat).pop()
}

/// Every line the ledger holds for a seat, oldest first: one per session the
/// seat banked at its `stop()`, each with its `session_id` and when it `ended`.
pub(crate) fn ledger_lines(state: &std::path::Path, seat: &str) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(state.join(crate::place_super::SPENT_FILE)).unwrap_or_default();
    text.lines()
        .filter(|l| l.contains(seat))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("seat").and_then(|s| s.as_str()) == Some(seat))
        .collect()
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

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["burn"],
        run: burn,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 110,
                text: r##"  wsp burn [--json]                 tokens by seat, dearest first — input,
                                    output and cache, tallied per hook from
                                    the transcript; where core-049's cuts land"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place_super::{cost, known, PRICES};

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

    /// wsp-264: a live burn.json is what `reading` returns; once the directory
    /// is gone the newest ledger line for the seat is; a seat in neither is None.
    #[test]
    fn reading_prefers_the_live_record_over_the_ledger() {
        let iso = crate::util::isolated("reading");
        let state = iso.state();
        let ledger = |seat: &str, cost: u64| {
            json!({ "seat": seat, "turns": 3, "input": 1, "output": 2, "cache_read": 3,
                "cache_write": 4, "cost": cost })
                .to_string()
        };
        std::fs::write(
            state.join("spent.jsonl"),
            format!("{}\n{}\n{}\n", ledger("cpd-1", 10), ledger("cpd-2", 20), ledger("cpd-1", 30)),
        )
        .unwrap();
        assert_eq!(reading_in(&state, "cpd-1").unwrap()["cost"], 30, "the newest line");
        assert!(reading_in(&state, "cpd-9").is_none());
        seat(&state, "compound-seats", "cpd-1", 5, 7);
        let live = reading_in(&state, "cpd-1").unwrap();
        assert_eq!((live["turns"].as_u64(), live["cost"].as_u64()), (Some(5), Some(700)));
    }

    /// `core-056`: Sonnet 5's $3/$15 step was cancelled, so $2/$10 is its
    /// standard price, and Sonnet 4.x is a separate $3/$15. Pinned apart, so
    /// neither can be moved to the other's number without a test saying so.
    #[test]
    fn sonnet_five_and_sonnet_four_are_priced_apart() {
        for model in ["claude-sonnet-5", "claude-sonnet-5-5"] {
            assert_eq!(cost(model, 1_000_000, 1_000_000, 0, 0), 12_000_000, "{model}");
        }
        for model in ["claude-sonnet-4", "claude-sonnet-4-5", "claude-sonnet-4-6"] {
            assert_eq!(cost(model, 1_000_000, 1_000_000, 0, 0), 18_000_000, "{model}");
        }
    }

    /// wsp-290: the page's Opus 5.5 row ($4/$20, reads at 0.05x), Sonnet 5.5's
    /// 0.05x, and Fable 5.1's 0.025x, each against the model it is a variant of.
    #[test]
    fn a_cache_read_bills_at_the_multiplier_its_model_is_listed_at() {
        let reads = |model: &str| cost(model, 0, 0, 1_000_000, 0);
        assert_eq!(reads("claude-opus-5-5"), 200_000, "$0.20 a million, not the 0.1x $0.40");
        assert_eq!(cost("claude-opus-5-5", 1_000_000, 1_000_000, 0, 0), 24_000_000, "$4 in, $20 out");
        assert_eq!(reads("claude-opus-5"), 500_000, "Opus 5 keeps the tenth of $5");
        assert_eq!(reads("claude-sonnet-5-5"), 100_000);
        assert_eq!(reads("claude-sonnet-5"), 200_000);
        assert_eq!(reads("claude-fable-5-1"), 250_000);
        assert_eq!(reads("claude-fable-5"), 1_000_000);
        assert_eq!(reads("gpt-something"), 1_000_000, "an unknown model is the dear row at a tenth");
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
