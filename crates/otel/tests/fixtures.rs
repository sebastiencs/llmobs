//! Validates every directory under `fixtures/otlp`.
//!
//! Passes with zero cases. `prices.json` and the layout are still checked.

#[allow(unreachable_pub)]
mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use llmobs_core::enums::{CostSource, PayloadFormat, SpanKind};
use llmobs_core::message::parse_messages;
use llmobs_core::span::Span;
use regex::{Regex, RegexBuilder};
use sha2::{Digest, Sha256};
use support::{
    Case, FIXTURES_DIR, GROUPS, Origin, PriceRow, canonical_json, check_otlp_json, load_all_cases,
    load_prices, sort_spans,
};

/// `input.json` size cap from `fixtures/otlp/README.md`.
const MAX_INPUT_BYTES: u64 = 256 * 1024;
/// Fixture spans sit in the 10 seconds before the case ingest time.
const SPAN_WINDOW_NS: i64 = 10_000_000_000;
/// Group directories. `_` is allowed (`google_adk`).
const GROUP_NAME: &str = r"^[a-z0-9]+(?:[-_][a-z0-9]+)*$";
/// Case directories. Kebab-case, so `_` is not allowed.
const CASE_NAME: &str = r"^[a-z0-9]+(?:-[a-z0-9]+)*$";

#[test]
fn layout() {
    finish(&layout_failures());
}

#[test]
fn case_json() {
    let loaded = load_all_cases();
    let mut failures = loaded.failures;
    for case in loaded.cases {
        let label = label(&case);
        if case.meta.description.is_empty() {
            failures.push(format!("{label}: description is empty"));
        }
        if case.meta.spec_refs.is_empty() {
            failures.push(format!("{label}: spec_refs is empty"));
        }
        let captured = case.meta.origin == Origin::Captured;
        if captured != case.meta.source.is_some() {
            failures.push(format!(
                "{label}: source must be present exactly when origin is captured"
            ));
        }
        check_media_bounds(&case, &mut failures);
        check_retention(&case, &mut failures);
    }
    finish(&failures);
}

#[test]
fn input_json() {
    let loaded = load_all_cases();
    let mut failures = loaded.failures;
    for case in loaded.cases {
        let case_label = label(&case);
        if input_spans(&case).is_empty() {
            failures.push(format!("{case_label}: input.json has no spans"));
        }
        match fs::metadata(case.dir.join("input.json")) {
            Ok(meta) => {
                if let Some(failure) = input_size_failure(&case_label, meta.len()) {
                    failures.push(failure);
                }
            }
            Err(err) => failures.push(format!("{case_label}: input.json: {err}")),
        }
        let text = match fs::read_to_string(case.dir.join("input.json")) {
            Ok(text) => text,
            Err(err) => {
                failures.push(format!("{case_label}: input.json: {err}"));
                continue;
            }
        };
        match serde_json::from_str(&text) {
            Ok(value) => failures.extend(check_otlp_json(&case_label, &value)),
            Err(err) => failures.push(format!("{case_label}: input.json: {err}")),
        }
        check_input_times(&case, &mut failures);
    }
    finish(&failures);
}

#[test]
fn expected_json() {
    let loaded = load_all_cases();
    let mut failures = loaded.failures;
    for case in loaded.cases {
        check_expected(&case, &mut failures);
    }
    finish(&failures);
}

#[test]
fn media() {
    let loaded = load_all_cases();
    let mut failures = loaded.failures;
    for case in loaded.cases {
        check_media(&case, &mut failures);
    }
    finish(&failures);
}

#[test]
fn costs() {
    let loaded = load_all_cases();
    let rows = load_prices();
    let mut failures = loaded.failures;
    for case in loaded.cases {
        for span in &case.expected.spans {
            check_cost(&label(&case), span, &rows, &mut failures);
        }
    }
    finish(&failures);
}

#[test]
fn canonical_formatting() {
    let rewrite = std::env::var_os("LLMOBS_FIXTURES_FMT").is_some_and(|value| value == "1");
    let mut failures = Vec::new();
    for path in json_files() {
        match fs::read_to_string(&path) {
            Ok(text) => check_canonical(&path, &text, rewrite, &mut failures),
            Err(err) => failures.push(format!("{}: {err}", path.display())),
        }
    }
    finish(&failures);
}

#[test]
fn secret_scan() {
    let mut failures = Vec::new();
    let patterns = secret_patterns();
    let email = must_regex(r"[A-Za-z0-9._%+\-]+@([A-Za-z0-9.-]+\.[A-Za-z]{2,})");
    for path in all_files() {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                failures.push(format!("{}: {err}", path.display()));
                continue;
            }
        };
        for pattern in &patterns {
            if pattern.is_match(&text) {
                failures.push(format!(
                    "{}: secret pattern /{}/",
                    path.display(),
                    pattern.as_str()
                ));
            }
        }
        for hit in email.captures_iter(&text) {
            let domain = hit.get(1).map(|group| group.as_str()).unwrap_or("");
            if domain != "example.com" && domain != "example.org" {
                failures.push(format!(
                    "{}: email address at a domain other than example.com or example.org",
                    path.display()
                ));
                break;
            }
        }
    }
    finish(&failures);
}

#[test]
fn prices() {
    let rows = load_prices();
    let mut failures = Vec::new();
    let mut compiled = Vec::new();
    for row in &rows {
        match compile_anchored(&row.match_pattern) {
            Ok(pattern) => compiled.push(pattern),
            Err(err) => failures.push(format!(
                "prices.json: {}: pattern does not compile: {err}",
                row.model_name
            )),
        }
    }
    if compiled.len() != rows.len() {
        finish(&failures);
        return;
    }

    let loaded = load_all_cases();
    failures.extend(loaded.failures);
    let cases = loaded.cases;
    for case in &cases {
        for span in &case.expected.spans {
            if span.cost_source != Some(CostSource::Computed) {
                continue;
            }
            let Some(model) = model_of(span) else {
                failures.push(format!(
                    "{}: computed cost span {} has no model",
                    label(case),
                    span.span_id
                ));
                continue;
            };
            let matched = rows.iter().zip(compiled.iter()).any(|(row, pattern)| {
                price_row_matches(
                    row,
                    pattern,
                    model,
                    span.provider.as_deref(),
                    span.start_time,
                )
            });
            if !matched {
                failures.push(format!(
                    "{}: no price row matches model {model}",
                    label(case)
                ));
            }
        }
    }

    let mut models = BTreeSet::new();
    for case in &cases {
        for span in &case.expected.spans {
            if let Some(model) = &span.request_model {
                models.insert(model.clone());
            }
            if let Some(model) = &span.response_model {
                models.insert(model.clone());
            }
        }
    }
    for model in &models {
        let mut hits: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for (row, pattern) in rows.iter().zip(compiled.iter()) {
            if pattern.is_match(model) {
                let key = row.provider.clone().unwrap_or_default();
                hits.entry(key).or_default().push(row.model_name.as_str());
            }
        }
        for (provider, names) in hits {
            if names.len() > 1 {
                let provider = if provider.is_empty() {
                    "null".to_owned()
                } else {
                    provider
                };
                failures.push(format!(
                    "prices.json: provider {provider} has {} rows matching model {model}: {}",
                    names.len(),
                    names.join(", ")
                ));
            }
        }
    }
    finish(&failures);
}

fn layout_failures() -> Vec<String> {
    let root = PathBuf::from(FIXTURES_DIR);
    let mut failures = Vec::new();
    if !root.join("README.md").is_file() {
        failures.push(format!("{}: missing README.md", root.display()));
    }
    if !root.join("prices.json").is_file() {
        failures.push(format!("{}: missing prices.json", root.display()));
    }
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) => {
            failures.push(format!("{}: {err}", root.display()));
            return failures;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                failures.push(format!("{}: {err}", root.display()));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_file() {
            if name != "README.md" && name != "prices.json" {
                failures.push(format!("unexpected top-level file {name}"));
            }
            continue;
        }
        if !GROUPS.contains(&name.as_str()) {
            failures.push(format!("unknown fixture group {name}"));
            continue;
        }
        check_group(&entry.path(), &name, &mut failures);
    }
    failures
}

fn check_group(dir: &Path, group: &str, failures: &mut Vec<String>) {
    if !name_matches(GROUP_NAME, group) {
        failures.push(format!(
            "{group}: group directory name must match {GROUP_NAME} (lowercase, '_' or '-' allowed)"
        ));
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            failures.push(format!("{group}: {err}"));
            return;
        }
    };
    let mut case_directories = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                failures.push(format!("{group}: {err}"));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() {
            failures.push(format!("{group}/{name} is not a case directory"));
            continue;
        }
        case_directories += 1;
        if !name_matches(CASE_NAME, &name) {
            failures.push(format!(
                "{group}/{name}: case directory name must match {CASE_NAME} (kebab-case)"
            ));
        }
        let mut files = Vec::new();
        let children = match fs::read_dir(entry.path()) {
            Ok(children) => children,
            Err(err) => {
                failures.push(format!("{group}/{name}: {err}"));
                continue;
            }
        };
        for child in children {
            let child = match child {
                Ok(child) => child,
                Err(err) => {
                    failures.push(format!("{group}/{name}: {err}"));
                    continue;
                }
            };
            let child_name = child.file_name().to_string_lossy().into_owned();
            if child.path().is_dir() {
                failures.push(format!("{group}/{name}/{child_name} is a nested directory"));
            }
            files.push(child_name);
        }
        files.sort();
        let expected = vec![
            "case.json".to_owned(),
            "expected.json".to_owned(),
            "input.json".to_owned(),
        ];
        if files != expected {
            failures.push(format!(
                "{group}/{name}: expected case.json, input.json, expected.json; found {}",
                files.join(", ")
            ));
        }
    }
    if case_directories == 0 {
        failures.push(format!(
            "{group}: create a group directory only when it contains a case"
        ));
    }
}

fn check_expected(case: &Case, failures: &mut Vec<String>) {
    let label = label(case);
    check_spans_are_exact(case, &label, failures);
    let mut ids = BTreeSet::new();
    for span in &case.expected.spans {
        if !ids.insert((span.trace_id, span.span_id)) {
            failures.push(format!("{label}: span {} appears twice", span.span_id));
        }
    }
    let mut sorted = case.expected.spans.clone();
    sort_spans(&mut sorted);
    if case.expected.spans != sorted {
        failures.push(format!(
            "{label}: spans are not sorted by (trace_id, start_time, span_id)"
        ));
    }
    let input_ids = input_spans(case);
    if case.expected.spans.len() + case.expected.rejected_spans as usize != input_ids.len() {
        failures.push(format!(
            "{label}: spans ({}) + rejected_spans ({}) != input spans ({})",
            case.expected.spans.len(),
            case.expected.rejected_spans,
            input_ids.len()
        ));
    }
    for span in &case.expected.spans {
        if span.project_id != case.meta.context.project_id {
            failures.push(format!(
                "{label}: {} project_id != case context",
                span.span_id
            ));
        }
        if span.ingest_time != case.meta.context.ingest_time {
            failures.push(format!(
                "{label}: {} ingest_time != case context",
                span.span_id
            ));
        }
        check_span_window(
            &label,
            &span.span_id.to_string(),
            span.start_time,
            span.end_time,
            case.meta.context.ingest_time,
            failures,
        );
        check_kind_costs(&label, span, failures);
        let id = (span.trace_id.to_string(), span.span_id.to_string());
        if !input_ids.contains(&id) {
            failures.push(format!(
                "{label}: expected span {}/{} is not in input.json",
                id.0, id.1
            ));
        }
        check_payload(&label, span, failures);
    }
}

fn check_payload(label: &str, span: &Span, failures: &mut Vec<String>) {
    check_pair(
        label,
        span,
        "input",
        &span.input,
        &span.input_format,
        failures,
    );
    check_pair(
        label,
        span,
        "output",
        &span.output,
        &span.output_format,
        failures,
    );
    if span.input_format == Some(PayloadFormat::Messages)
        && let Some(input) = &span.input
        && parse_messages(input).is_err()
    {
        failures.push(format!(
            "{label}: {} input_format is messages but input does not parse",
            span.span_id
        ));
    }
    if span.output_format == Some(PayloadFormat::Messages)
        && let Some(output) = &span.output
        && parse_messages(output).is_err()
    {
        failures.push(format!(
            "{label}: {} output_format is messages but output does not parse",
            span.span_id
        ));
    }
}

fn check_pair(
    label: &str,
    span: &Span,
    field: &str,
    value: &Option<String>,
    format: &Option<PayloadFormat>,
    failures: &mut Vec<String>,
) {
    if value.is_some() != format.is_some() {
        failures.push(format!(
            "{label}: {} {field} and {field}_format must both be set or both be absent",
            span.span_id
        ));
    }
}

/// Each expected span must be exactly what `Span` serializes back to.
///
/// `Span` ignores unknown keys, so a misspelled optional key (`cache_write_token`) would
/// otherwise parse as `None` and pass. Comparing text also pins key order and the
/// 9-digit timestamps (D64), which parsing alone accepts.
fn check_spans_are_exact(case: &Case, label: &str, failures: &mut Vec<String>) {
    let path = case.dir.join("expected.json");
    let raw: serde_json::Value = match fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|err| err.to_string()))
    {
        Ok(value) => value,
        Err(err) => {
            failures.push(format!("{label}: expected.json: {err}"));
            return;
        }
    };
    let Some(raw_spans) = raw.get("spans").and_then(serde_json::Value::as_array) else {
        failures.push(format!("{label}: expected.json has no spans array"));
        return;
    };
    for (raw_span, span) in raw_spans.iter().zip(&case.expected.spans) {
        let reserialized = match serde_json::to_value(span) {
            Ok(value) => value,
            Err(err) => {
                failures.push(format!("{label}: span {}: {err}", span.span_id));
                continue;
            }
        };
        if canonical_json(raw_span) != canonical_json(&reserialized) {
            failures.push(format!(
                "{label}: span {} differs from its `Span` serialization (unknown or misspelled \
                 key, key order, or timestamp precision)",
                span.span_id
            ));
        }
    }
}

fn check_media(case: &Case, failures: &mut Vec<String>) {
    let label = label(case);
    let media = &case.expected.media;
    let hashes: Vec<&str> = media.iter().map(|item| item.sha256.as_str()).collect();
    let mut sorted = hashes.clone();
    sorted.sort_unstable();
    if hashes != sorted {
        failures.push(format!("{label}: media is not sorted by sha256"));
    }
    let mut seen = BTreeSet::new();
    for hash in &hashes {
        if !seen.insert(*hash) {
            failures.push(format!("{label}: media sha256 {hash} is duplicated"));
        }
    }
    for item in media {
        if !is_sha256(&item.sha256) {
            failures.push(format!(
                "{label}: media sha256 {} is not 64 lowercase hex characters",
                item.sha256
            ));
        }
        if item.size_bytes < case.meta.settings.media_min_bytes
            || item.size_bytes > case.meta.settings.media_max_bytes
        {
            failures.push(format!(
                "{label}: media {} size {} is outside [{}, {}]",
                item.sha256,
                item.size_bytes,
                case.meta.settings.media_min_bytes,
                case.meta.settings.media_max_bytes
            ));
        }
    }
    let listed: BTreeSet<&str> = hashes.iter().copied().collect();
    let mut referenced = BTreeSet::new();
    for span in &case.expected.spans {
        for hash in &span.media_refs {
            referenced.insert(hash.as_str());
        }
        for hash in media_uris(span) {
            if !listed.contains(hash.as_str()) {
                failures.push(format!(
                    "{label}: {} contains media://{hash} which is not listed",
                    span.span_id
                ));
            }
        }
    }
    if listed != referenced {
        failures.push(format!(
            "{label}: media hashes {listed:?} != media_refs {referenced:?}"
        ));
    }
    if media.is_empty() {
        return;
    }
    let input_path = case.dir.join("input.json");
    let text = match fs::read_to_string(&input_path) {
        Ok(text) => text,
        Err(err) => {
            failures.push(format!("{}: {err}", input_path.display()));
            return;
        }
    };
    let found = decode_base64_runs(&text, case.meta.settings.media_min_bytes);
    for item in media {
        let size = item.size_bytes;
        if !found
            .iter()
            .any(|(hash, len)| hash == &item.sha256 && *len == size)
        {
            failures.push(format!(
                "{label}: media {} ({size} bytes) is not backed by base64 in input.json",
                item.sha256
            ));
        }
    }
}

fn check_cost(label: &str, span: &Span, rows: &[PriceRow], failures: &mut Vec<String>) {
    match span.cost_source {
        Some(CostSource::Computed) => check_computed(label, span, rows, failures),
        Some(CostSource::None) => {
            let priced = matches!(span.span_kind, SpanKind::Llm | SpanKind::Embedding);
            let matched = model_of(span).is_some_and(|model| {
                rows.iter().any(|row| {
                    let Ok(pattern) = compile_anchored(&row.match_pattern) else {
                        return false;
                    };
                    price_row_matches(
                        row,
                        &pattern,
                        model,
                        span.provider.as_deref(),
                        span.start_time,
                    )
                })
            });
            if priced && matched {
                failures.push(format!(
                    "{label}: {} has cost_source none but a price row matches",
                    span.span_id
                ));
            }
            if span.cost_input.is_some() || span.cost_output.is_some() || span.cost_total.is_some()
            {
                failures.push(format!(
                    "{label}: {} cost_source none must have null cost fields",
                    span.span_id
                ));
            }
        }
        Some(CostSource::Reported) | None => {}
    }
}

fn check_computed(label: &str, span: &Span, rows: &[PriceRow], failures: &mut Vec<String>) {
    let Some(model) = model_of(span) else {
        failures.push(format!(
            "{label}: {} cost_source computed but model is absent",
            span.span_id
        ));
        return;
    };
    let Some(row) = pick_price(rows, model, span.provider.as_deref(), span.start_time) else {
        failures.push(format!(
            "{label}: {} no price row for model {model}",
            span.span_id
        ));
        return;
    };
    let (input, output, total) = recompute(span, row);
    compare_cost(label, span, "cost_input", span.cost_input, input, failures);
    compare_cost(
        label,
        span,
        "cost_output",
        span.cost_output,
        output,
        failures,
    );
    compare_cost(label, span, "cost_total", span.cost_total, total, failures);
}

fn compare_cost(
    label: &str,
    span: &Span,
    field: &str,
    actual: Option<f64>,
    expected: Option<f64>,
    failures: &mut Vec<String>,
) {
    match (actual, expected) {
        (None, None) => {}
        (None, Some(expected)) => failures.push(format!(
            "{label}: {} {field} is absent; recomputed {expected}",
            span.span_id
        )),
        (Some(_), None) => failures.push(format!(
            "{label}: {} {field} is present; recomputed absent",
            span.span_id
        )),
        (Some(actual), Some(expected)) if (actual - expected).abs() > 1e-12 => {
            failures.push(format!(
                "{label}: {} {field} is {actual}, recomputed {expected}",
                span.span_id
            ));
        }
        (Some(_), Some(_)) => {}
    }
}

fn recompute(span: &Span, row: &PriceRow) -> (Option<f64>, Option<f64>, Option<f64>) {
    let input = span.input_tokens.unwrap_or(0) as f64;
    let output = span.output_tokens.unwrap_or(0) as f64;
    let cache_read = span.cache_read_tokens.unwrap_or(0) as f64;
    let cache_write = span.cache_write_tokens.unwrap_or(0) as f64;
    let reasoning = span.reasoning_tokens.unwrap_or(0) as f64;
    let uncached = (input - cache_read - cache_write).max(0.0);
    let non_reasoning = (output - reasoning).max(0.0);
    let cost_input = priced_part(&[
        charge(uncached, row.price_input),
        charge(cache_read, row.price_cache_read),
        charge(cache_write, row.price_cache_write),
    ])
    .map(|cost| cost / 1e6);
    if span.span_kind == SpanKind::Embedding {
        return (cost_input, None, cost_input);
    }
    let reasoning_price = row.price_reasoning.or(row.price_output);
    let cost_output = priced_part(&[
        charge(non_reasoning, row.price_output),
        charge(reasoning, reasoning_price),
    ])
    .map(|cost| cost / 1e6);
    let cost_total = match (cost_input, cost_output) {
        (Some(input), Some(output)) => Some(input + output),
        _ => None,
    };
    (cost_input, cost_output, cost_total)
}

/// `None` when any term is `None`. A zero token count is `Some(0.0)` from [`charge`].
fn priced_part(terms: &[Option<f64>]) -> Option<f64> {
    let mut total = 0.0;
    for term in terms {
        total += (*term)?;
    }
    Some(total)
}

fn charge(tokens: f64, price: Option<f64>) -> Option<f64> {
    if tokens == 0.0 {
        Some(0.0)
    } else {
        price.map(|price| tokens * price)
    }
}

fn pick_price<'a>(
    rows: &'a [PriceRow],
    model: &str,
    provider: Option<&str>,
    start_time: i64,
) -> Option<&'a PriceRow> {
    let mut matches: Vec<&PriceRow> = rows
        .iter()
        .filter(|row| {
            let Ok(pattern) = compile_anchored(&row.match_pattern) else {
                return false;
            };
            price_row_matches(row, &pattern, model, provider, start_time)
        })
        .collect();
    matches.sort_by(|left, right| {
        right
            .valid_from
            .cmp(&left.valid_from)
            .then(right.match_pattern.len().cmp(&left.match_pattern.len()))
    });
    matches.first().copied()
}

fn price_row_matches(
    row: &PriceRow,
    pattern: &Regex,
    model: &str,
    provider: Option<&str>,
    start_time: i64,
) -> bool {
    if row.valid_from > start_time {
        return false;
    }
    if let Some(row_provider) = &row.provider
        && Some(row_provider.as_str()) != provider
    {
        return false;
    }
    pattern.is_match(model)
}

fn model_of(span: &Span) -> Option<&str> {
    span.response_model
        .as_deref()
        .or(span.request_model.as_deref())
}

fn check_media_bounds(case: &Case, failures: &mut Vec<String>) {
    if case.meta.settings.media_min_bytes > case.meta.settings.media_max_bytes {
        failures.push(format!(
            "{}: media_min_bytes {} is greater than media_max_bytes {}",
            label(case),
            case.meta.settings.media_min_bytes,
            case.meta.settings.media_max_bytes
        ));
    }
}

fn input_size_failure(case_label: &str, len: u64) -> Option<String> {
    if len > MAX_INPUT_BYTES {
        Some(format!(
            "{case_label}: input.json is {len} bytes, above 256 KiB"
        ))
    } else {
        None
    }
}

fn check_input_times(case: &Case, failures: &mut Vec<String>) {
    let case_label = label(case);
    let ingest = case.meta.context.ingest_time;
    // Rejected spans are absent from expected.json. Only accepted spans must
    // sit in the 10-second window (fixtures README).
    let accepted = case
        .expected
        .spans
        .iter()
        .map(|span| (span.trace_id.to_hex(), span.span_id.to_hex()))
        .collect::<BTreeSet<_>>();
    for resource in &case.input.resource_spans {
        for scope in &resource.scope_spans {
            for span in &scope.spans {
                let trace_id = hex_encode(&span.trace_id);
                let span_id = hex_encode(&span.span_id);
                let (Some(start), Some(end)) = (
                    i64::try_from(span.start_time_unix_nano).ok(),
                    i64::try_from(span.end_time_unix_nano).ok(),
                ) else {
                    failures.push(format!(
                        "{case_label}: span {span_id} has a timestamp that does not fit in i64"
                    ));
                    continue;
                };
                if !accepted.contains(&(trace_id, span_id.clone())) {
                    continue;
                }
                check_span_window(&case_label, &span_id, start, end, ingest, failures);
            }
        }
    }
}

fn check_retention(case: &Case, failures: &mut Vec<String>) {
    if case.meta.settings.retention_days == Some(0) {
        failures.push(format!(
            "{}: retention_days must be an integer >= 1",
            label(case)
        ));
    }
}

fn check_span_window(
    label: &str,
    span_id: &str,
    start: i64,
    end: i64,
    ingest: i64,
    failures: &mut Vec<String>,
) {
    if start < ingest - SPAN_WINDOW_NS {
        failures.push(format!(
            "{label}: span {span_id} starts before ingest_time - 10s"
        ));
    }
    if start >= ingest {
        failures.push(format!(
            "{label}: span {span_id} starts at or after ingest_time"
        ));
    }
    if end >= ingest {
        failures.push(format!(
            "{label}: span {span_id} ends at or after ingest_time"
        ));
    }
}

fn check_kind_costs(label: &str, span: &Span, failures: &mut Vec<String>) {
    let carries_usage = span.input_tokens.is_some()
        || span.output_tokens.is_some()
        || span.cache_read_tokens.is_some()
        || span.cache_write_tokens.is_some()
        || span.reasoning_tokens.is_some()
        || span.total_tokens.is_some()
        || span.cost_input.is_some()
        || span.cost_output.is_some()
        || span.cost_total.is_some()
        || span.cost_source.is_some();
    if !matches!(span.span_kind, SpanKind::Llm | SpanKind::Embedding) && carries_usage {
        failures.push(format!(
            "{label}: {} is {} so token fields, cost fields and cost_source must be absent",
            span.span_id,
            span.span_kind.as_str()
        ));
    }
    if span.cost_source.is_none()
        && (span.cost_input.is_some() || span.cost_output.is_some() || span.cost_total.is_some())
    {
        failures.push(format!(
            "{label}: {} has no cost_source but a cost field is set",
            span.span_id
        ));
    }
}

fn name_matches(pattern: &str, name: &str) -> bool {
    must_regex(pattern).is_match(name)
}

fn input_spans(case: &Case) -> Vec<(String, String)> {
    let mut ids = Vec::new();
    for resource in &case.input.resource_spans {
        for scope in &resource.scope_spans {
            for span in &scope.spans {
                ids.push((hex_encode(&span.trace_id), hex_encode(&span.span_id)));
            }
        }
    }
    ids
}

fn media_uris(span: &Span) -> Vec<String> {
    let pattern = must_regex(r"media://([0-9a-f]{64})");
    let mut hashes = Vec::new();
    for text in [&span.input, &span.output, &span.system_instructions]
        .into_iter()
        .flatten()
    {
        for hit in pattern.captures_iter(text) {
            if let Some(hash) = hit.get(1) {
                hashes.push(hash.as_str().to_owned());
            }
        }
    }
    hashes
}

fn decode_base64_runs(text: &str, media_min_bytes: u64) -> Vec<(String, u64)> {
    let min_chars = media_min_bytes.saturating_mul(4).div_ceil(3);
    let Ok(pattern) = Regex::new(&format!(r"[A-Za-z0-9+/]{{{min_chars},}}={{0,2}}")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for hit in pattern.find_iter(text) {
        let Ok(bytes) = STANDARD.decode(hit.as_str()) else {
            continue;
        };
        found.push((hex_encode(&Sha256::digest(&bytes)), bytes.len() as u64));
    }
    found
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn check_canonical(path: &Path, text: &str, rewrite: bool, failures: &mut Vec<String>) {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            failures.push(format!("{}: {err}", path.display()));
            return;
        }
    };
    let canonical = canonical_json(&value);
    if text == canonical {
        return;
    }
    if rewrite {
        if let Err(err) = fs::write(path, &canonical) {
            failures.push(format!("{}: {err}", path.display()));
            return;
        }
        // `just fixtures-fmt` prints the paths it rewrote.
        #[allow(clippy::print_stdout)]
        {
            println!("rewrote {}", path.display());
        }
        return;
    }
    failures.push(format!(
        "{}: not canonical JSON; run just fixtures-fmt",
        path.display()
    ));
}

fn json_files() -> Vec<PathBuf> {
    let root = PathBuf::from(FIXTURES_DIR);
    let mut files = vec![root.join("prices.json")];
    let Ok(groups) = fs::read_dir(&root) else {
        return files;
    };
    for group in groups.flatten() {
        if !group.path().is_dir() {
            continue;
        }
        let Ok(cases) = fs::read_dir(group.path()) else {
            continue;
        };
        for case in cases.flatten() {
            if !case.path().is_dir() {
                continue;
            }
            for name in ["case.json", "input.json", "expected.json"] {
                let path = case.path().join(name);
                if path.is_file() {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

fn all_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(&PathBuf::from(FIXTURES_DIR), &mut files);
    files.sort();
    files
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            collect_files(&entry.path(), files);
        } else if entry.path().is_file() {
            files.push(entry.path());
        }
    }
}

fn secret_patterns() -> Vec<Regex> {
    [
        r"sk-[A-Za-z0-9_-]{20,}",
        r"sk-ant-[A-Za-z0-9_-]{20,}",
        r"AKIA[0-9A-Z]{16}",
        r"ghp_[A-Za-z0-9]{36}",
        r"xox[baprs]-[A-Za-z0-9-]{10,}",
        r"Bearer [A-Za-z0-9._-]{20,}",
    ]
    .into_iter()
    .map(must_regex)
    .collect()
}

fn must_regex(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(err) => panic!("pattern {pattern} compiles: {err}"),
    }
}

fn compile_anchored(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(&format!("^(?:{pattern})$"))
        .case_insensitive(true)
        .build()
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn label(case: &Case) -> String {
    format!("{}/{}", case.group, case.name)
}

fn finish(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "fixture checks failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn non_llm_span_cannot_carry_tokens_or_cost() {
    let mut span = bare_span("tool");
    span.input_tokens = Some(1);
    let mut failures = Vec::new();
    check_kind_costs("edge/sample", &span, &mut failures);
    assert_eq!(
        failures,
        vec![
            "edge/sample: bbbbbbbbbbbbbbbb is tool so token fields, cost fields and cost_source must be absent"
                .to_owned(),
        ]
    );
}

#[test]
fn span_without_cost_source_cannot_carry_a_cost() {
    let mut span = bare_span("llm");
    span.cost_total = Some(1.0);
    let mut failures = Vec::new();
    check_kind_costs("edge/sample", &span, &mut failures);
    assert_eq!(
        failures,
        vec!["edge/sample: bbbbbbbbbbbbbbbb has no cost_source but a cost field is set".to_owned()]
    );
}

#[test]
fn duplicate_media_sha256_fails() {
    let mut case = empty_case();
    let hash = "ab".repeat(32);
    case.expected.media = vec![
        support::ExpectedMedia {
            sha256: hash.clone(),
            mime_type: "image/png".to_owned(),
            size_bytes: 1,
        },
        support::ExpectedMedia {
            sha256: hash.clone(),
            mime_type: "image/png".to_owned(),
            size_bytes: 1,
        },
    ];
    let mut failures = Vec::new();
    check_media(&case, &mut failures);
    assert!(
        failures
            .iter()
            .any(|failure| failure == &format!("edge/sample: media sha256 {hash} is duplicated")),
        "{failures:?}"
    );
}

#[test]
fn retention_days_zero_fails() {
    let mut case = empty_case();
    case.meta.settings.retention_days = Some(0);
    let mut failures = Vec::new();
    check_retention(&case, &mut failures);
    assert_eq!(
        failures,
        vec!["edge/sample: retention_days must be an integer >= 1".to_owned()]
    );
}

#[test]
fn rejected_spans_skip_the_ingest_window() {
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    let mut case = empty_case();
    case.input.resource_spans.push(ResourceSpans {
        scope_spans: vec![ScopeSpans {
            spans: vec![Span {
                trace_id: vec![0x11; 16],
                span_id: vec![0x22; 8],
                start_time_unix_nano: 0,
                end_time_unix_nano: 0,
                ..Span::default()
            }],
            ..ScopeSpans::default()
        }],
        ..ResourceSpans::default()
    });
    let mut failures = Vec::new();
    check_input_times(&case, &mut failures);
    assert!(
        failures.is_empty(),
        "a span missing from expected.json is rejected and not window-checked: {failures:?}"
    );
}

#[test]
fn media_min_bytes_above_max_fails() {
    let mut case = empty_case();
    case.meta.settings.media_min_bytes = 32;
    case.meta.settings.media_max_bytes = 8;
    let mut failures = Vec::new();
    check_media_bounds(&case, &mut failures);
    assert_eq!(
        failures,
        vec!["edge/sample: media_min_bytes 32 is greater than media_max_bytes 8".to_owned()]
    );
}

#[test]
fn input_over_256_kib_fails() {
    assert!(input_size_failure("edge/sample", MAX_INPUT_BYTES).is_none());
    assert_eq!(
        input_size_failure("edge/sample", MAX_INPUT_BYTES + 1).as_deref(),
        Some("edge/sample: input.json is 262145 bytes, above 256 KiB")
    );
}

#[test]
fn span_outside_the_ingest_window_fails() {
    let ingest = 1_000_000_000_000;
    let mut failures = Vec::new();
    check_span_window(
        "edge/sample",
        "span",
        ingest - SPAN_WINDOW_NS - 1,
        ingest - 1,
        ingest,
        &mut failures,
    );
    assert_eq!(
        failures,
        vec!["edge/sample: span span starts before ingest_time - 10s".to_owned()]
    );

    failures.clear();
    check_span_window("edge/sample", "span", ingest, ingest, ingest, &mut failures);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("starts at or after ingest_time"))
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("ends at or after ingest_time"))
    );

    failures.clear();
    check_span_window(
        "edge/sample",
        "span",
        ingest - SPAN_WINDOW_NS,
        ingest - 1,
        ingest,
        &mut failures,
    );
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn directory_names_match_the_readme() {
    assert!(name_matches(GROUP_NAME, "google_adk"));
    assert!(!name_matches(CASE_NAME, "google_adk"));
    assert!(name_matches(CASE_NAME, "chat-cache-tokens"));

    let root = std::env::temp_dir().join(format!("llmobs-otlp-names-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("Bad_Name")).unwrap_or_else(|err| panic!("{err}"));
    let mut failures = Vec::new();
    check_group(&root, "Google", &mut failures);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("group directory name"))
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("kebab-case"))
    );

    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap_or_else(|err| panic!("{err}"));
    failures.clear();
    check_group(&empty, "edge", &mut failures);
    assert_eq!(
        failures,
        vec!["edge: create a group directory only when it contains a case".to_owned()]
    );
    fs::remove_dir_all(&root).unwrap_or_else(|err| panic!("{err}"));
}

fn bare_span(kind: &str) -> Span {
    serde_json::from_value(serde_json::json!({
        "project_id": "01900000-0000-7000-8000-000000000001",
        "trace_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "span_id": "bbbbbbbbbbbbbbbb",
        "name": "step",
        "span_kind": kind,
        "start_time": "2025-12-31T23:59:50.000000000Z",
        "end_time": "2025-12-31T23:59:51.000000000Z",
        "status": "unset",
        "ingest_time": "2026-01-01T00:00:00.000000000Z",
        "schema_version": 1
    }))
    .unwrap_or_else(|err| panic!("{err}"))
}

fn empty_case() -> Case {
    use support::{CaseMeta, Context, Expected, Origin, Settings};
    let project_id = support::DEFAULT_PROJECT_ID
        .parse()
        .unwrap_or_else(|err| panic!("{err}"));
    Case {
        group: "edge".to_owned(),
        name: "sample".to_owned(),
        dir: PathBuf::from("/no/such/llmobs-fixture"),
        meta: CaseMeta {
            description: "sample".to_owned(),
            origin: Origin::HandWritten,
            source: None,
            spec_refs: vec!["02".to_owned()],
            settings: Settings {
                max_attribute_bytes: 8,
                media_min_bytes: 1,
                media_max_bytes: 10,
                retention_days: None,
            },
            context: Context {
                project_id,
                ingest_time: 0,
            },
            notes: None,
        },
        input: Default::default(),
        expected: Expected {
            spans: Vec::new(),
            media: Vec::new(),
            rejected_spans: 0,
        },
    }
}

#[test]
fn null_cache_write_price_nulls_input_and_total() {
    let mut span = bare_span("llm");
    span.input_tokens = Some(100);
    span.cache_write_tokens = Some(100);
    span.output_tokens = Some(10);
    let mut row = sample_price();
    row.price_cache_write = None;
    let (input, output, total) = recompute(&span, &row);
    assert_eq!(input, None);
    assert_eq!(output, Some(10.0 * 4.0 / 1e6));
    assert_eq!(total, None);
}

#[test]
fn embedding_span_has_null_output_cost() {
    let mut span = bare_span("embedding");
    span.input_tokens = Some(1_000_000);
    let mut row = sample_price();
    row.price_input = Some(2.0);
    let (input, output, total) = recompute(&span, &row);
    assert_eq!((input, output, total), (Some(2.0), None, Some(2.0)));
}

#[test]
fn null_reasoning_price_uses_the_output_price() {
    let mut span = bare_span("llm");
    span.output_tokens = Some(5);
    span.reasoning_tokens = Some(5);
    let mut row = sample_price();
    row.price_reasoning = None;
    row.price_output = Some(4.0);
    let (_input, output, _total) = recompute(&span, &row);
    assert_eq!(output, Some(5.0 * 4.0 / 1e6));
}

fn sample_price() -> PriceRow {
    PriceRow {
        model_name: "demo".to_owned(),
        match_pattern: "demo".to_owned(),
        provider: None,
        valid_from: 0,
        price_input: Some(3.0),
        price_output: Some(4.0),
        price_cache_read: Some(0.3),
        price_cache_write: Some(3.75),
        price_reasoning: None,
        tiers: None,
    }
}
