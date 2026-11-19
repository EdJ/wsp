//! What a seat costs: the prices, and the arithmetic the burn tally prices
//! each request with. Moved down from `wsp-wire`'s `place_super` (wsp-020),
//! which re-exports it, because the tally ([`crate::inproc::tally_burn`]) is
//! the in-process host's.

/// What a million tokens costs, in micro-dollars, on one tier.
///
/// Micro-dollars because the unit has to divide exactly: every rate below is a
/// whole number of dollars per million tokens, and the two cache multipliers
/// are a tenth and a quarter, so integer arithmetic in millionths loses
/// nothing. Floating point in a ranking would be a rounding argument nobody
/// asked for.
pub struct Price {
    input: u64,
    output: u64,
    /// What a cache read bills at, as a fraction of `input`. A tenth on most
    /// tiers and not on all of them: the page lists Opus 5.5 and Sonnet 5.5 at
    /// 0.05x and Fable 5.1 and Mythos 5.1 at 0.025x.
    cache_read: (u64, u64),
}

/// When [`PRICES`] was last checked against the first-party price reference.
/// Printed under the report, so the reader sees the age of the quote beside
/// the money it produced. Move it only when the page has actually been
/// re-read: the date is a claim about what the table was checked against.
pub const PRICES_READ: &str = "2026-10-18";

/// **List prices, first-party Anthropic API, read on [`PRICES_READ`].** This is
/// the thing in wsp that goes stale without saying so, which is why it is a
/// table with a date on it rather than a constant buried in an expression — the
/// same bargain the cache multipliers below make, and the reason `core-049`'s
/// `CACHE_DISCOUNT` was written down instead of inlined.
///
/// **A reading date does not protect a promotional price, and a known end date
/// is not a known outcome.** A list price goes stale unpredictably; an
/// introductory one is due to change on a date known the day the row is typed.
/// `core-056`: Sonnet 5 was entered at its introductory $2/$10, due to end
/// 2026-08-31 with a step to $3/$15, and that step was then cancelled — $2/$10
/// is the standard price. The row was right all along, and a first fix made
/// from the brief's account rather than the page over-billed sonnet by half.
/// So a row priced on a promotion carries its end date beside it, and the date
/// is a prompt to re-read the page, not a number to apply.
///
/// **The cache-read multiplier is a column of this table, because the page's
/// is not one number.** Opus 5.5 ($4/$20) and Sonnet 5.5 ($2/$10) read at 0.05x,
/// Fable 5.1 and Mythos 5.1 at 0.025x, every other model at a tenth. A seat reads
/// nearly all of its volume from cache, so a row priced at the wrong multiplier
/// is wrong by up to the whole cache line: wsp-290 found Opus 5.5 priced at
/// 2.5x its real read price and Sonnet 5.5, the tier legs run on, at 2x. Haiku
/// 5.5 is priced by prompt length ($0.10/$0.50 up to 100k, $0.50/$2.50 over) and
/// this table does not model that; it still reads as Haiku 4.5's $1/$5.
///
/// Matched as a prefix, longest-specific first, because a model id carries a
/// date or a variant wsp has no business parsing (`claude-opus-5`,
/// `claude-haiku-4-5-20251001`). A model that matches nothing is priced at the
/// **first row**, which is the dearest: a report answering *where did the money
/// go* must never make an unrecognised spender look cheap, and the model column
/// shows the name that was not recognised.
pub const PRICES: &[(&str, Price)] = &[
    ("claude-fable-5-1", Price { input: 10_000_000, output: 50_000_000, cache_read: (1, 40) }),
    ("claude-mythos-5-1", Price { input: 10_000_000, output: 50_000_000, cache_read: (1, 40) }),
    ("claude-fable", Price { input: 10_000_000, output: 50_000_000, cache_read: (1, 10) }),
    ("claude-mythos", Price { input: 10_000_000, output: 50_000_000, cache_read: (1, 10) }),
    ("claude-opus-5-5", Price { input: 4_000_000, output: 20_000_000, cache_read: (1, 20) }),
    ("claude-opus", Price { input: 5_000_000, output: 25_000_000, cache_read: (1, 10) }),
    // Sonnet 4.x, Sonnet 5 and Sonnet 5.5 are not the same price, and the fleet
    // runs all three: `d5f0956` starts work under a seat on sonnet while seats
    // stay on the settings tier, and a leg runs on Sonnet 5.5 unless its entry
    // names another. Sonnet 5's $2/$10 was introductory through 2026-08-31; the
    // step to $3/$15 that was to follow was cancelled and $2/$10 is now the
    // standard price. Sonnet 5.5 is the same $2/$10 with reads at 0.05x.
    ("claude-sonnet-4", Price { input: 3_000_000, output: 15_000_000, cache_read: (1, 10) }),
    ("claude-sonnet-5-5", Price { input: 2_000_000, output: 10_000_000, cache_read: (1, 20) }),
    ("claude-sonnet", Price { input: 2_000_000, output: 10_000_000, cache_read: (1, 10) }),
    ("claude-haiku", Price { input: 1_000_000, output: 5_000_000, cache_read: (1, 10) }),
];

/// What a model that matches nothing is priced at: the dearest row, at the
/// ordinary multiplier. A report answering *where did the money go* must never
/// make an unrecognised spender look cheap.
const UNKNOWN: Price = Price { input: 10_000_000, output: 50_000_000, cache_read: (1, 10) };

/// Whether [`PRICES`] has a row for this model. A model id that is not Claude's
/// (`opencode/x-free`) is priced by [`cost`] at [`UNKNOWN`], which is right for a
/// seat's burn and wrong for a row that should say nothing instead.
pub fn known(model: &str) -> bool {
    PRICES.iter().any(|(prefix, _)| model.starts_with(prefix))
}

/// Cache reads bill at a tenth of the input rate unless the row says otherwise,
/// cache writes at a quarter above it. Stated rather than hidden, exactly as `core-049` stated its
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
const CACHE_WRITE: (u64, u64) = (5, 4);

/// What one request cost, in micro-dollars, on the model that served it.
///
/// Called from the tally rather than from the report (`crate::place_super`),
/// because a session that changed tier partway through has no single model to
/// price its totals against — the request does. The report reads the
/// accumulated number and does not re-derive it.
pub fn cost(model: &str, input: u64, output: u64, cache_read: u64, cache_write: u64) -> u64 {
    let p = PRICES
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, price)| price)
        .unwrap_or(&UNKNOWN);
    let at = |tokens: u64, per_mtok: u64| tokens.saturating_mul(per_mtok) / 1_000_000;
    at(input, p.input)
        + at(output, p.output)
        + at(cache_read, p.input * p.cache_read.0 / p.cache_read.1)
        + at(cache_write, p.input * CACHE_WRITE.0 / CACHE_WRITE.1)
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
pub fn cost_of(r: &serde_json::Value) -> u64 {
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
