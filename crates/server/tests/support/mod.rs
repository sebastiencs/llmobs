//! Fixture loaders shared by the example checks.

use std::fs;
use std::path::PathBuf;

use llmobs_core::span::Span;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) fn api_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api")
}

pub(crate) fn canonical_json<T: Serialize + DeserializeOwned>(value: &T) -> String {
    // One parse-and-print pass. serde_json's shortest float form is not always a fixed
    // point of its parser, and the example files must round-trip through the typed value.
    let once = serde_json::to_string_pretty(value).unwrap_or_else(|error| panic!("{error}"));
    let parsed: T = serde_json::from_str(&once).unwrap_or_else(|error| panic!("{error}"));
    let twice = serde_json::to_string_pretty(&parsed).unwrap_or_else(|error| panic!("{error}"));
    format!("{twice}\n")
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &std::path::Path) -> T {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub(crate) fn dataset_spans() -> Vec<Span> {
    read_json(&api_dir().join("dataset/spans.json"))
}
