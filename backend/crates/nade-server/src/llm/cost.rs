//! What a model call costs, in exact integers.
//!
//! # Why there is not a single float in this file
//!
//! The spend ceiling's entire job is to be correct at its boundary: a ledger
//! standing at exactly $1.00 must block, and one at $0.999999999 must not.
//! Dollars-as-`f64` cannot promise that — `0.1 + 0.2 != 0.3` is the same
//! arithmetic — and the failure mode is silent, intermittent, and only ever
//! shows up as "the ceiling let one more run through sometimes".
//!
//! So money here is `i64` **nano-USD** (10^-9 USD) end to end, and it happens
//! to be exact for another reason too: every published Anthropic price is a
//! whole number of dollars per million tokens, so a per-token rate is a whole
//! number of nano-USD. $1.00/MTok is exactly 1 000 nano-USD per token.
//!
//! `i64` nano-USD tops out around $9.2 billion. The daily ceiling is $1.

/// Nano-USD in one USD.
pub const NANO_PER_USD: i64 = 1_000_000_000;

/// Per-token prices for one model, in nano-USD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    /// Ordinary input tokens.
    pub input: i64,
    /// Generated tokens.
    pub output: i64,
    /// Writing an entry to the prompt cache. Anthropic prices this at 1.25x
    /// base input. Unused at P4 — caching is off — but priced so that turning
    /// it on later is a one-line change and not a silent under-count.
    pub cache_write: i64,
    /// Reading a cache hit: 0.1x base input.
    pub cache_read: i64,
}

impl Rates {
    /// Build from a dollars-per-million-tokens pair.
    ///
    /// `const` so the table below is computed at compile time and cannot drift
    /// from the multipliers.
    const fn per_mtok(input_usd: i64, output_usd: i64) -> Self {
        // $1 per 1e6 tokens = 1e9 nano / 1e6 tokens = 1000 nano per token.
        let input = input_usd * 1_000;
        Self {
            input,
            output: output_usd * 1_000,
            // 1.25x, kept integral: x * 5 / 4 is exact for every rate here.
            cache_write: input * 5 / 4,
            // 0.1x, exact for the same reason.
            cache_read: input / 10,
        }
    }
}

/// Model id prefix -> rates. Longest match wins, so a dated snapshot picks up
/// its family's price without an entry of its own.
///
/// Ordered most-expensive-first only for readability; `rates_for` does not
/// depend on the order.
const TABLE: &[(&str, Rates)] = &[
    ("claude-fable-5", Rates::per_mtok(10, 50)),
    ("claude-mythos-5", Rates::per_mtok(10, 50)),
    ("claude-opus-", Rates::per_mtok(5, 25)),
    // Sonnet 5 carried a lower introductory price for a while. The standard
    // rate is used deliberately: over-charging the ledger is safe, and a price
    // that silently expires would under-charge exactly when the intro ends.
    ("claude-sonnet-", Rates::per_mtok(3, 15)),
    ("claude-haiku-", Rates::per_mtok(1, 5)),
];

/// What an unpriced model is charged.
///
/// **Deliberately the most expensive rate in the table, not zero and not an
/// error.** A model we do not recognise still costs real money, and the two
/// alternatives are both worse: priced at zero, the ceiling stops binding and
/// the account can spend without limit; treated as an error, a model rename by
/// the provider takes the whole agent runtime down. Over-charging stops runs
/// early, which is recoverable and visible. Under-charging is neither.
pub const UNKNOWN_MODEL_RATES: Rates = Rates::per_mtok(10, 50);

/// The rates for `model`, or [`UNKNOWN_MODEL_RATES`] if it is not in the table.
///
/// Returns whether the model was recognised, so the caller can log the miss
/// once rather than this function logging on every call.
#[must_use]
pub fn rates_for(model: &str) -> (Rates, bool) {
    let mut best: Option<(usize, Rates)> = None;
    for (prefix, rates) in TABLE {
        if model.starts_with(prefix) {
            // EDGE: two prefixes could both match a future id. Longest wins,
            // so a specific entry always beats a family entry.
            if best.is_none_or(|(len, _)| prefix.len() > len) {
                best = Some((prefix.len(), *rates));
            }
        }
    }
    match best {
        Some((_, rates)) => (rates, true),
        None => (UNKNOWN_MODEL_RATES, false),
    }
}

/// Token counts for one call, as the provider reported them.
///
/// Anthropic reports cache reads and writes **outside** `input_tokens`, which
/// is why this type exists at all: the SDK's `TokenUsage` has two counters by
/// design (`message.rs` — "Cache-read/cache-write breakdowns ... belong in the
/// adapter"), so the adapter is the only place the full breakdown is visible,
/// and therefore the only place that can price a call correctly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u32,
    pub output: u32,
    pub cache_write: u32,
    pub cache_read: u32,
}

/// The largest cost `llm_calls.cost_usd` can hold.
///
/// The column is `numeric(14, 9)`: fourteen digits, nine after the point, so
/// five before it. `i64::MAX` nano-USD renders as `9223372036.854775807` -
/// **ten** integer digits - and the insert raises a numeric field overflow,
/// the row is dropped, and the call is counted as **zero**.
///
/// That is the opposite of what saturating was for. Clamping here keeps the
/// safe direction actually safe: an absurd usage report lands as the largest
/// storable charge instead of vanishing.
pub const MAX_STORABLE_NANO_USD: i64 = 99_999_999_999_999;

/// Price `tokens` at `model`'s rates, in nano-USD.
///
/// Saturating **and clamped**: an absurd token count from a misbehaving
/// provider must not wrap the ledger negative, and must not overflow the column
/// either - both hand the account unlimited spend, the second one silently.
#[must_use]
pub fn cost_nano_usd(model: &str, tokens: Tokens) -> i64 {
    let (rates, _) = rates_for(model);
    let part = |count: u32, rate: i64| i64::from(count).saturating_mul(rate);
    part(tokens.input, rates.input)
        .saturating_add(part(tokens.output, rates.output))
        .saturating_add(part(tokens.cache_write, rates.cache_write))
        .saturating_add(part(tokens.cache_read, rates.cache_read))
        .min(MAX_STORABLE_NANO_USD)
}

/// Render nano-USD as a `numeric(14,9)` literal for PostgreSQL.
///
/// Text, not `f64`: binding a float would undo in one line everything this
/// module exists to guarantee. `sqlx` has no native i128/decimal type here, so
/// the value is bound as a string and cast by the statement.
#[must_use]
pub fn nano_to_numeric(nano: i64) -> String {
    let sign = if nano < 0 { "-" } else { "" };
    // EDGE: i64::MIN has no positive counterpart, so widen before negating.
    let abs = i128::from(nano).unsigned_abs();
    let whole = abs / 1_000_000_000;
    let frac = abs % 1_000_000_000;
    format!("{sign}{whole}.{frac:09}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dollar_per_mtok_is_a_thousand_nano_per_token() {
        let (rates, known) = rates_for("claude-haiku-4-5-20251001");
        assert!(
            known,
            "a dated Haiku snapshot must price off its family prefix"
        );
        assert_eq!(rates.input, 1_000);
        assert_eq!(rates.output, 5_000);
    }

    #[test]
    fn cache_multipliers_are_exact() {
        let (rates, _) = rates_for("claude-haiku-4-5");
        // 1.25x and 0.1x of 1_000, with no rounding anywhere.
        assert_eq!(rates.cache_write, 1_250);
        assert_eq!(rates.cache_read, 100);
    }

    #[test]
    fn every_table_entry_prices_its_own_prefix() {
        for (prefix, expected) in TABLE {
            let (got, known) = rates_for(prefix);
            assert!(known, "{prefix} does not match itself");
            assert_eq!(got, *expected, "{prefix} priced wrong");
        }
    }

    #[test]
    fn an_unknown_model_is_charged_the_most_expensive_rate_not_zero() {
        let (rates, known) = rates_for("some-model-released-after-this-build");
        assert!(!known);
        assert_eq!(rates, UNKNOWN_MODEL_RATES);
        // The property that actually matters: it is not free, and it is not
        // cheaper than anything we do know about.
        assert!(rates.input > 0 && rates.output > 0);
        for (_, known_rates) in TABLE {
            assert!(rates.input >= known_rates.input);
            assert!(rates.output >= known_rates.output);
        }
    }

    #[test]
    fn a_longer_prefix_wins() {
        // `claude-opus-` and a hypothetical longer entry must not race.
        let (opus, _) = rates_for("claude-opus-5");
        assert_eq!(opus, Rates::per_mtok(5, 25));
    }

    #[test]
    fn one_haiku_run_at_the_dev_cap_costs_what_the_plan_says() {
        // The per-run token budget is 50k. Even spent entirely on output -
        // impossible, but the worst case - one run is a quarter of a dollar.
        let worst = cost_nano_usd(
            "claude-haiku-4-5",
            Tokens {
                output: 50_000,
                ..Tokens::default()
            },
        );
        assert_eq!(worst, 250_000_000, "50k output tokens at $5/MTok is $0.25");
        assert!(worst < NANO_PER_USD, "one run cannot exhaust a $1 ceiling");
    }

    #[test]
    fn empty_usage_is_free() {
        assert_eq!(cost_nano_usd("claude-haiku-4-5", Tokens::default()), 0);
    }

    #[test]
    fn an_absurd_token_count_saturates_high_rather_than_wrapping_negative() {
        let cost = cost_nano_usd(
            "claude-fable-5",
            Tokens {
                input: u32::MAX,
                output: u32::MAX,
                cache_write: u32::MAX,
                cache_read: u32::MAX,
            },
        );
        // The direction is the point: a provider reporting nonsense must trip
        // the ceiling, never credit the account.
        assert!(cost > 0, "cost must never go negative");
    }

    #[test]
    fn nano_renders_as_a_fixed_scale_decimal() {
        assert_eq!(nano_to_numeric(0), "0.000000000");
        assert_eq!(nano_to_numeric(1), "0.000000001");
        assert_eq!(nano_to_numeric(NANO_PER_USD), "1.000000000");
        assert_eq!(nano_to_numeric(250_000_000), "0.250000000");
        assert_eq!(nano_to_numeric(1_234_567_891), "1.234567891");
    }

    #[test]
    fn rendering_the_extremes_does_not_panic() {
        // EDGE: i64::MIN cannot be negated in i64. Nothing should ever produce
        // it, which is exactly why it is worth proving it does not panic.
        assert_eq!(nano_to_numeric(i64::MIN), "-9223372036.854775808");
        assert_eq!(nano_to_numeric(i64::MAX), "9223372036.854775807");
    }
}
