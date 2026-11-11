//! Price table and span cost.
//!
//! Spec 02 § Cost computation, D82 and D109. Reported cost wins outright.
//! Otherwise an `llm` or `embedding` span is priced from the matching row.

use llmobs_core::enums::{CostSource, SpanKind};
use llmobs_core::names::attr;
use regex::{Regex, RegexSet, RegexSetBuilder};
use serde::{Deserialize, Serialize};

use crate::bag::AttrBag;
use crate::error::PriceError;
use crate::usage::Usage;
use crate::value::{InvalidNumber, as_f64};
use crate::warn::{Warning, Warnings};

/// Compiled patterns may use this many bytes in total.
const PATTERN_SIZE_LIMIT: usize = 16 * 1024 * 1024;

const TOTAL_KEYS: &[&str] = &[
    attr::COST_TOTAL,
    "llm.cost.total",
    "gen_ai.usage.cost",
    "gen_ai.usage.total_cost",
];
const INPUT_KEYS: &[&str] = &["llm.cost.prompt", "gen_ai.usage.input_cost"];
const OUTPUT_KEYS: &[&str] = &["llm.cost.completion", "gen_ai.usage.output_cost"];

/// Who owns a price row. Project beats org, which beats built-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceScope {
    /// A row from the embedded snapshot.
    #[default]
    Builtin,
    /// A row stored for one organization.
    Org,
    /// A row stored for one project.
    Project,
}

/// One long-context price band. Null prices use the row's base price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceTier {
    /// This band applies when `input_tokens` is at least this count.
    pub min_input_tokens: i64,
    /// USD per 1M uncached input tokens.
    pub price_input: Option<f64>,
    /// USD per 1M output tokens.
    pub price_output: Option<f64>,
    /// USD per 1M cache-read tokens.
    pub price_cache_read: Option<f64>,
    /// USD per 1M cache-write tokens.
    pub price_cache_write: Option<f64>,
    /// USD per 1M reasoning tokens. Null uses the row's base reasoning price.
    pub price_reasoning: Option<f64>,
}

/// One price-table row. `scope` defaults to [`PriceScope::Builtin`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceRow {
    /// Who owns the row.
    #[serde(default)]
    pub scope: PriceScope,
    /// Display name.
    pub model_name: String,
    /// Regex matched against the span model. Anchored and case-insensitive
    /// when the table is built.
    pub match_pattern: String,
    /// When set, the row matches this provider, and also a span that names none.
    pub provider: Option<String>,
    /// Row applies at span start times at or after this instant.
    #[serde(with = "llmobs_core::time_serde")]
    pub valid_from: i64,
    /// USD per 1M uncached input tokens.
    pub price_input: Option<f64>,
    /// USD per 1M output tokens.
    pub price_output: Option<f64>,
    /// USD per 1M cache-read tokens.
    pub price_cache_read: Option<f64>,
    /// USD per 1M cache-write tokens.
    pub price_cache_write: Option<f64>,
    /// USD per 1M reasoning tokens. Null means use `price_output` at compute time.
    pub price_reasoning: Option<f64>,
    /// Long-context bands. Null when the row has none.
    pub tiers: Option<Vec<PriceTier>>,
}

/// Compiled price rows. Patterns are `^(?i:pattern)$`.
#[derive(Debug, Clone)]
pub struct PriceTable {
    rows: Vec<PriceRow>,
    set: RegexSet,
}

impl PriceTable {
    /// Compile `rows`. An invalid pattern or a set over 16 MiB is an error.
    ///
    /// # Errors
    ///
    /// [`PriceError::InvalidPattern`] carries the row index and no pattern text.
    /// [`PriceError::TooLarge`] is the compiled-size limit.
    pub fn new(rows: Vec<PriceRow>) -> Result<Self, PriceError> {
        Self::compile(rows, PATTERN_SIZE_LIMIT)
    }

    fn compile(rows: Vec<PriceRow>, size_limit: usize) -> Result<Self, PriceError> {
        let mut patterns = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let pattern = format!("^(?i:{})$", row.match_pattern);
            if Regex::new(&pattern).is_err() {
                return Err(PriceError::InvalidPattern { index });
            }
            patterns.push(pattern);
        }
        let set = RegexSetBuilder::new(patterns)
            .size_limit(size_limit)
            .build()
            .map_err(|err| match err {
                regex::Error::CompiledTooBig(_) => PriceError::TooLarge,
                _ => PriceError::InvalidPattern { index: 0 },
            })?;
        Ok(Self { rows, set })
    }
}

/// Built-in price rows from the committed LiteLLM snapshot.
///
/// Every row has [`PriceScope::Builtin`]. `just prices-update` regenerates the
/// file. `tests/prices.rs` checks that it parses, so a failure here is a bug
/// in that snapshot.
#[allow(clippy::expect_used)]
pub fn builtin_price_rows() -> Vec<PriceRow> {
    // The snapshot is ours and `tests/prices.rs` checks it. A parse failure is a bug.
    serde_json::from_str(include_str!("../prices/builtin.json"))
        .expect("builtin.json parses; tests/prices.rs checks the snapshot")
}

/// Cost columns for one span.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Priced {
    /// Input cost, USD.
    pub input: Option<f64>,
    /// Output cost, USD. Null on an embedding span.
    pub output: Option<f64>,
    /// Total cost, USD.
    pub total: Option<f64>,
    /// Where the columns came from. Null when the kind does not carry cost.
    pub source: Option<CostSource>,
    /// Provider taken from the matched row when the span named none.
    pub provider: Option<String>,
}

/// Span facts cost needs, apart from the attribute bag.
pub(crate) struct Charge<'a> {
    /// Span kind.
    pub kind: SpanKind,
    /// Provider already read from attributes.
    pub provider: Option<&'a str>,
    /// `response_model`, or `request_model` when the response names none.
    pub model: Option<&'a str>,
    /// Span start, nanoseconds.
    pub start_time: i64,
    /// Token columns.
    pub usage: &'a Usage,
}

/// Fill cost for one span. On any other kind the cost keys stay unread.
pub(crate) fn apply(
    bag: &mut AttrBag<'_>,
    charge: &Charge<'_>,
    prices: &PriceTable,
    warnings: &mut Warnings,
) -> Priced {
    if !matches!(charge.kind, SpanKind::Llm | SpanKind::Embedding) {
        return Priced::default();
    }
    if let Some(reported) = reported_cost(bag, warnings) {
        return Priced {
            input: reported.input,
            output: reported.output,
            total: reported.total,
            source: Some(CostSource::Reported),
            provider: None,
        };
    }
    let Some(model) = charge.model else {
        return none();
    };
    let Some(matched) = lookup(prices, model, charge.provider, charge.start_time) else {
        return none();
    };
    let units = unit_prices(matched, charge.usage.input_tokens);
    let (input, output, total) = formula(charge.kind, charge.usage, &units);
    let inferred = match charge.provider {
        None => matched.provider.clone(),
        Some(_) => None,
    };
    Priced {
        input,
        output,
        total,
        source: Some(CostSource::Computed),
        provider: inferred,
    }
}

fn none() -> Priced {
    Priced {
        source: Some(CostSource::None),
        ..Priced::default()
    }
}

struct Reported {
    input: Option<f64>,
    output: Option<f64>,
    total: Option<f64>,
}

/// `None` when no reported-cost key is present. A present key sets the source
/// even when its value is not a number.
fn reported_cost(bag: &mut AttrBag<'_>, warnings: &mut Warnings) -> Option<Reported> {
    let (total_present, total) = first_number(bag, TOTAL_KEYS, warnings);
    let (input_present, input) = first_number(bag, INPUT_KEYS, warnings);
    let (output_present, output) = first_number(bag, OUTPUT_KEYS, warnings);
    if !total_present && !input_present && !output_present {
        return None;
    }
    let total = total.or_else(|| match (input, output) {
        (Some(input), Some(output)) => (input + output).is_finite().then_some(input + output),
        _ => None,
    });
    Some(Reported {
        input,
        output,
        total,
    })
}

/// The first finite number, and whether any key in `keys` exists.
fn first_number(
    bag: &mut AttrBag<'_>,
    keys: &[&str],
    warnings: &mut Warnings,
) -> (bool, Option<f64>) {
    let mut present = false;
    let mut found = None;
    for key in keys {
        if bag.get(key).is_none() {
            continue;
        }
        present = true;
        if found.is_some() {
            continue;
        }
        let parsed = bag.get(key).map(as_f64);
        match parsed {
            Some(Ok(Some(number))) => {
                bag.take(key);
                found = Some(number);
            }
            Some(Err(InvalidNumber)) => warnings.add(Warning::InvalidNumber),
            _ => {}
        }
    }
    (present, found)
}

fn lookup<'a>(
    prices: &'a PriceTable,
    model: &str,
    provider: Option<&str>,
    start_time: i64,
) -> Option<&'a PriceRow> {
    let mut best: Option<(usize, &PriceRow)> = None;
    for index in prices.set.matches(model) {
        let Some(row) = prices.rows.get(index) else {
            continue;
        };
        if !row_matches(row, provider, start_time) {
            continue;
        }
        let wins =
            best.is_none_or(|(best_index, best_row)| row_beats(row, index, best_row, best_index));
        if wins {
            best = Some((index, row));
        }
    }
    best.map(|(_, row)| row)
}

fn row_matches(row: &PriceRow, provider: Option<&str>, start_time: i64) -> bool {
    if row.valid_from > start_time {
        return false;
    }
    match (&row.provider, provider) {
        (Some(row_provider), Some(span_provider)) => row_provider == span_provider,
        (Some(_), None) | (None, _) => true,
    }
}

/// Scope, then latest `valid_from`, then the longest pattern, then the lowest index.
fn row_beats(row: &PriceRow, index: usize, best: &PriceRow, best_index: usize) -> bool {
    row.scope > best.scope
        || (row.scope == best.scope && row.valid_from > best.valid_from)
        || (row.scope == best.scope
            && row.valid_from == best.valid_from
            && row.match_pattern.len() > best.match_pattern.len())
        || (row.scope == best.scope
            && row.valid_from == best.valid_from
            && row.match_pattern.len() == best.match_pattern.len()
            && index < best_index)
}

struct Units {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    reasoning: Option<f64>,
}

fn unit_prices(row: &PriceRow, input_tokens: Option<i64>) -> Units {
    let tokens = input_tokens.unwrap_or(0).max(0);
    let tier = row.tiers.as_ref().and_then(|tiers| {
        tiers.iter().fold(None, |best: Option<&PriceTier>, tier| {
            if tier.min_input_tokens > tokens {
                return best;
            }
            match best {
                Some(current) if current.min_input_tokens >= tier.min_input_tokens => Some(current),
                _ => Some(tier),
            }
        })
    });
    Units {
        input: tier.and_then(|tier| tier.price_input).or(row.price_input),
        output: tier.and_then(|tier| tier.price_output).or(row.price_output),
        cache_read: tier
            .and_then(|tier| tier.price_cache_read)
            .or(row.price_cache_read),
        cache_write: tier
            .and_then(|tier| tier.price_cache_write)
            .or(row.price_cache_write),
        reasoning: tier
            .and_then(|tier| tier.price_reasoning)
            .or(row.price_reasoning),
    }
}

fn formula(
    kind: SpanKind,
    usage: &Usage,
    units: &Units,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let input_tokens = count(usage.input_tokens);
    let output_tokens = count(usage.output_tokens);
    let cache_read = count(usage.cache_read_tokens);
    let cache_write = count(usage.cache_write_tokens);
    let reasoning = count(usage.reasoning_tokens);
    let uncached = input_tokens
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    let non_reasoning = output_tokens.saturating_sub(reasoning);
    let cost_input = part(&[
        charge(uncached, units.input),
        charge(cache_read, units.cache_read),
        charge(cache_write, units.cache_write),
    ]);
    if kind == SpanKind::Embedding {
        return (cost_input, None, cost_input);
    }
    let reasoning_price = units.reasoning.or(units.output);
    let cost_output = part(&[
        charge(non_reasoning, units.output),
        charge(reasoning, reasoning_price),
    ]);
    let cost_total = match (cost_input, cost_output) {
        (Some(input), Some(output)) => (input + output).is_finite().then_some(input + output),
        _ => None,
    };
    (cost_input, cost_output, cost_total)
}

fn count(value: Option<i64>) -> i64 {
    value.unwrap_or(0).max(0)
}

fn part(terms: &[Option<f64>]) -> Option<f64> {
    let mut total = 0.0;
    for term in terms {
        total += (*term)?;
    }
    (total / 1e6).is_finite().then_some(total / 1e6)
}

fn charge(tokens: i64, price: Option<f64>) -> Option<f64> {
    if tokens == 0 {
        return Some(0.0);
    }
    let cost = (tokens as f64) * price?;
    cost.is_finite().then_some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        pattern: &str,
        scope: PriceScope,
        provider: Option<&str>,
        valid_from: i64,
        price_input: f64,
    ) -> PriceRow {
        PriceRow {
            scope,
            model_name: pattern.to_owned(),
            match_pattern: pattern.to_owned(),
            provider: provider.map(str::to_owned),
            valid_from,
            price_input: Some(price_input),
            price_output: Some(1.0),
            price_cache_read: None,
            price_cache_write: None,
            price_reasoning: None,
            tiers: None,
        }
    }

    fn usage_input(tokens: i64) -> Usage {
        Usage {
            input_tokens: Some(tokens),
            ..Usage::default()
        }
    }

    fn table(rows: Vec<PriceRow>) -> PriceTable {
        PriceTable::new(rows).expect("patterns")
    }

    #[test]
    fn project_beats_org_and_builtin_then_date_then_pattern_then_index() {
        let prices = table(vec![
            row("gpt", PriceScope::Builtin, None, 0, 1.0),
            row("gpt", PriceScope::Org, None, 0, 2.0),
            row("gpt-4o", PriceScope::Project, None, 10, 3.0),
            row("gpt", PriceScope::Project, None, 10, 4.0),
            row("gpt", PriceScope::Project, None, 5, 5.0),
        ]);
        let matched = lookup(&prices, "gpt", None, 10).expect("row");
        assert_eq!(matched.price_input, Some(4.0));
        let earlier = lookup(&prices, "gpt", None, 6).expect("row");
        assert_eq!(earlier.price_input, Some(5.0));
        let longer = table(vec![
            row("gpt", PriceScope::Builtin, None, 0, 1.0),
            row("gpt.*", PriceScope::Builtin, None, 0, 8.0),
        ]);
        let matched = lookup(&longer, "gpt", None, 0).expect("longer");
        assert_eq!(matched.price_input, Some(8.0));
    }

    #[test]
    fn the_pattern_is_anchored_and_case_insensitive() {
        let prices = table(vec![row("gpt-4o", PriceScope::Builtin, None, 0, 1.0)]);
        assert!(lookup(&prices, "gpt-4o", None, 0).is_some());
        assert!(lookup(&prices, "GPT-4O", None, 0).is_some());
        assert!(lookup(&prices, "gpt-4o-mini", None, 0).is_none());
        assert!(lookup(&prices, "xgpt-4o", None, 0).is_none());
    }

    #[test]
    fn an_invalid_pattern_names_only_its_index() {
        let err = PriceTable::new(vec![row("(", PriceScope::Builtin, None, 0, 1.0)])
            .expect_err("invalid");
        assert_eq!(err, PriceError::InvalidPattern { index: 0 });
        let text = err.to_string();
        assert!(!text.contains('('));
        assert!(text.contains("index 0"));
    }

    #[test]
    fn the_compiled_set_has_a_size_limit() {
        let rows = vec![row("abcdef", PriceScope::Builtin, None, 0, 1.0)];
        let err = PriceTable::compile(rows, 1).expect_err("too large");
        assert_eq!(err, PriceError::TooLarge);
    }

    fn priced_of(prices: &PriceTable, model: &str, tokens: i64) -> Priced {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&[], &mut warnings);
        apply(
            &mut bag,
            &Charge {
                kind: SpanKind::Llm,
                provider: None,
                model: Some(model),
                start_time: 0,
                usage: &usage_input(tokens),
            },
            prices,
            &mut warnings,
        )
    }

    #[test]
    fn a_token_count_near_the_integer_limit_stays_finite() {
        let prices = table(vec![row(
            "huge",
            PriceScope::Builtin,
            Some("openai"),
            0,
            1.0,
        )]);
        let priced = priced_of(&prices, "huge", i64::MAX);
        let input = priced.input.expect("input");
        assert!(input.is_finite());
        assert!(priced.output.is_some_and(f64::is_finite));
        assert!(priced.total.is_some_and(f64::is_finite));
        assert_eq!(priced.provider.as_deref(), Some("openai"));
        assert_eq!(priced.source, Some(CostSource::Computed));
    }

    #[test]
    fn a_matched_row_with_a_null_price_still_sets_the_provider() {
        let mut priced_row = row("bare", PriceScope::Builtin, Some("openai"), 0, 1.0);
        priced_row.price_input = None;
        priced_row.price_output = None;
        let prices = table(vec![priced_row]);
        let priced = priced_of(&prices, "bare", 10);
        assert_eq!(priced.input, None);
        assert_eq!(priced.source, Some(CostSource::Computed));
        assert_eq!(priced.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn the_lowest_index_wins_a_complete_tie() {
        let prices = table(vec![
            row("same", PriceScope::Builtin, None, 0, 1.0),
            row("same", PriceScope::Builtin, None, 0, 9.0),
        ]);
        let matched = lookup(&prices, "same", None, 0).expect("row");
        assert_eq!(matched.price_input, Some(1.0));
    }
}
