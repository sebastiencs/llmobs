//! Span attributes with consumption tracking.
//!
//! Spec 02 rule 2. The normalizer reads the bag from N4 on. Indexed conventions
//! use [`AttrBag::indexed`], never a raw prefix (OpenObserve #16271).
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};

use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};

use crate::value::to_map_string;
use crate::warn::{Warning, Warnings};

/// Encoded map values at most this many bytes stay in `attributes` when consumed.
pub(crate) const SMALL_ATTRIBUTE_BYTES: usize = 256;

const EMPTY: AnyValue = AnyValue { value: None };

/// How a key was used. `Payload` is never downgraded to `Consumed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Use {
    Unread,
    Consumed,
    Payload,
}

struct Entry<'a> {
    key: &'a str,
    value: &'a AnyValue,
    state: Use,
}

/// Request attributes in first-seen order. Duplicate keys keep the last value.
pub(crate) struct AttrBag<'a> {
    entries: Vec<Entry<'a>>,
    index: HashMap<&'a str, usize>,
}

impl<'a> AttrBag<'a> {
    /// Last duplicate wins, and `duplicate_key` is recorded.
    pub(crate) fn new(attrs: &'a [KeyValue], warnings: &mut Warnings) -> Self {
        let mut entries: Vec<Entry<'a>> = Vec::with_capacity(attrs.len());
        let mut index: HashMap<&'a str, usize> = HashMap::with_capacity(attrs.len());
        for attr in attrs {
            let key = attr.key.as_str();
            let value: &'a AnyValue = attr.value.as_ref().unwrap_or(&EMPTY);
            if let Some(&existing) = index.get(key) {
                warnings.add(Warning::DuplicateKey);
                if let Some(entry) = entries.get_mut(existing) {
                    entry.value = value;
                }
                continue;
            }
            index.insert(key, entries.len());
            entries.push(Entry {
                key,
                value,
                state: Use::Unread,
            });
        }
        Self { entries, index }
    }

    /// How `key` has been used, if the bag has it.
    pub(crate) fn state(&self, key: &str) -> Option<Use> {
        let &slot = self.index.get(key)?;
        self.entries.get(slot).map(|entry| entry.state)
    }

    /// Peek. Does not change [`Use`].
    pub(crate) fn get(&self, key: &str) -> Option<&'a AnyValue> {
        let &slot = self.index.get(key)?;
        self.entries.get(slot).map(|entry| entry.value)
    }

    /// Mark `Consumed` unless the key is already `Payload`.
    pub(crate) fn take(&mut self, key: &str) -> Option<&'a AnyValue> {
        let &slot = self.index.get(key)?;
        let entry = self.entries.get_mut(slot)?;
        if entry.state != Use::Payload {
            entry.state = Use::Consumed;
        }
        Some(entry.value)
    }

    /// Mark `Payload`.
    pub(crate) fn take_payload(&mut self, key: &str) -> Option<&'a AnyValue> {
        self.mark_payload(key)
    }

    /// Mark `Payload` without reading. Missing keys are ignored.
    pub(crate) fn drop_payload(&mut self, key: &str) {
        self.mark_payload(key);
    }

    /// Every key, in first-seen order.
    pub(crate) fn keys(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.entries.iter().map(|entry| entry.key)
    }

    /// Keys that start with `prefix`, in first-seen order.
    ///
    /// For sweeps such as `ai.settings.` and `metadata.`. Numbered keys use
    /// [`Self::indexed`].
    pub(crate) fn keys_with_prefix<'b>(
        &'b self,
        prefix: &'b str,
    ) -> impl Iterator<Item = &'a str> + 'b {
        self.entries
            .iter()
            .filter(move |entry| entry.key.starts_with(prefix))
            .map(|entry| entry.key)
    }

    /// `prefix` + decimal index + `.` + rest, grouped by index.
    ///
    /// `0` is an index. `01` and `name` are not. The rest keeps later dots,
    /// so `tool_calls.0.function.name` stays intact. Keys inside one index
    /// keep request order.
    pub(crate) fn indexed(&self, prefix: &str) -> BTreeMap<usize, Vec<(&'a str, &'a AnyValue)>> {
        let mut grouped = BTreeMap::new();
        for entry in &self.entries {
            let Some(rest) = entry.key.strip_prefix(prefix) else {
                continue;
            };
            let Some((index_text, after)) = rest.split_once('.') else {
                continue;
            };
            if !is_decimal_index(index_text) {
                continue;
            }
            let Ok(index) = index_text.parse() else {
                continue;
            };
            grouped
                .entry(index)
                .or_insert_with(Vec::new)
                .push((after, entry.value));
        }
        grouped
    }

    /// Rule 2. Unread values are kept. Consumed values are kept when their
    /// rule 1 map string is at most [`SMALL_ATTRIBUTE_BYTES`]. Payload values
    /// are removed. Encoding still records warnings, and a value past the
    /// depth limit adds its key to `truncated`.
    pub(crate) fn into_attributes(
        self,
        warnings: &mut Warnings,
        truncated: &mut Vec<String>,
    ) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for entry in self.entries {
            let mut local = Warnings::default();
            let text = to_map_string(entry.value, &mut local);
            if local.contains(Warning::MaxDepth) {
                truncated.push(entry.key.to_owned());
            }
            let keep = match entry.state {
                Use::Unread => true,
                Use::Consumed => text.len() <= SMALL_ATTRIBUTE_BYTES,
                Use::Payload => false,
            };
            if keep {
                out.insert(entry.key.to_owned(), text);
            }
            warnings.append(local);
        }
        out
    }

    fn mark_payload(&mut self, key: &str) -> Option<&'a AnyValue> {
        let &slot = self.index.get(key)?;
        let entry = self.entries.get_mut(slot)?;
        entry.state = Use::Payload;
        Some(entry.value)
    }
}

/// `0`, or a decimal that does not start with `0`.
pub(crate) fn is_decimal_index(text: &str) -> bool {
    let mut bytes = text.bytes();
    match bytes.next() {
        Some(b'0') => bytes.next().is_none(),
        Some(byte) if byte.is_ascii_digit() => bytes.all(|byte| byte.is_ascii_digit()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;

    fn string_attr(key: &str, text: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(text.to_owned())),
            }),
            key_strindex: 0,
        }
    }

    fn string_value(text: &str) -> AnyValue {
        AnyValue {
            value: Some(Value::StringValue(text.to_owned())),
        }
    }

    #[test]
    fn duplicate_keys_keep_the_last_value_and_warn_once() {
        let attrs = [
            string_attr("model", "first"),
            string_attr("model", "second"),
            string_attr("model", "last"),
            string_attr("other", "kept"),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        assert_eq!(bag.get("model"), Some(&string_value("last")));
        assert_eq!(bag.get("missing"), None);
        assert_eq!(
            warnings.to_attribute().as_deref(),
            Some("[\"duplicate_key\"]")
        );
        let map = bag.into_attributes(&mut warnings, &mut Vec::new());
        assert_eq!(map.get("model").map(String::as_str), Some("last"));
        assert_eq!(map.get("other").map(String::as_str), Some("kept"));
    }

    #[test]
    fn rule_2_keeps_small_consumed_and_unread_values_only() {
        let small = "x".repeat(SMALL_ATTRIBUTE_BYTES);
        let big = "y".repeat(SMALL_ATTRIBUTE_BYTES + 1);
        let attrs = [
            string_attr("small", &small),
            string_attr("big", &big),
            string_attr("payload", "x"),
            string_attr("unread", &big),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        bag.take("small");
        bag.take("big");
        bag.take_payload("payload");
        let map = bag.into_attributes(&mut warnings, &mut Vec::new());
        assert_eq!(map.get("small").map(String::as_str), Some(small.as_str()));
        assert!(!map.contains_key("big"));
        assert!(!map.contains_key("payload"));
        assert_eq!(map.get("unread").map(String::as_str), Some(big.as_str()));
        assert!(warnings.to_attribute().is_none());
    }

    #[test]
    fn payload_is_not_downgraded_and_drop_marks_unread_keys() {
        let attrs = [string_attr("body", "x"), string_attr("other", "y")];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        assert!(bag.take_payload("body").is_some());
        assert!(bag.take("body").is_some());
        bag.drop_payload("other");
        bag.drop_payload("missing");
        assert!(bag.get("other").is_some());
        let map = bag.into_attributes(&mut warnings, &mut Vec::new());
        assert!(map.is_empty());
    }

    #[test]
    fn get_does_not_consume() {
        let big = "z".repeat(SMALL_ATTRIBUTE_BYTES + 1);
        let attrs = [string_attr("big", &big)];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        assert_eq!(bag.get("big"), Some(&string_value(&big)));
        let map = bag.into_attributes(&mut warnings, &mut Vec::new());
        assert_eq!(map.get("big").map(String::as_str), Some(big.as_str()));
    }

    #[test]
    fn prefix_sweep_does_not_parse_indexes() {
        let attrs = [
            string_attr("ai.settings.maxRetries", "2"),
            string_attr("ai.settings", "no"),
            string_attr("ai.settingsExtra", "no"),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        let keys: Vec<&str> = bag.keys_with_prefix("ai.settings.").collect();
        assert_eq!(keys, ["ai.settings.maxRetries"]);
    }

    #[test]
    fn indexed_keys_require_a_decimal_index() {
        let attrs = [
            string_attr("gen_ai.prompt.0.role", "user"),
            string_attr("gen_ai.prompt.2.content", "later"),
            string_attr("gen_ai.prompt.name", "ignored"),
            string_attr("gen_ai.prompt.01.role", "ignored"),
            string_attr("gen_ai.prompt.0", "ignored"),
            string_attr("gen_ai.prompt.0.tool_calls.0.function.name", "lookup"),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        let grouped = bag.indexed("gen_ai.prompt.");
        assert_eq!(grouped.keys().copied().collect::<Vec<_>>(), [0, 2]);
        let zero = &grouped[&0];
        assert_eq!(zero.len(), 2);
        assert_eq!(zero[0].0, "role");
        assert_eq!(as_text(zero[0].1), "user");
        assert_eq!(zero[1].0, "tool_calls.0.function.name");
        assert_eq!(as_text(zero[1].1), "lookup");
        assert_eq!(grouped[&2][0].0, "content");
        assert!(!grouped.contains_key(&1));
    }

    #[test]
    fn into_attributes_reports_depth_and_invalid_numbers() {
        let deep = nest(33);
        let attrs = [
            KeyValue {
                key: "nan".to_owned(),
                value: Some(AnyValue {
                    value: Some(Value::DoubleValue(f64::NAN)),
                }),
                key_strindex: 0,
            },
            KeyValue {
                key: "deep".to_owned(),
                value: Some(deep),
                key_strindex: 0,
            },
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        // A removed payload is still encoded, so its name is still truncated.
        bag.drop_payload("deep");
        let mut truncated = Vec::new();
        let map = bag.into_attributes(&mut warnings, &mut truncated);
        assert_eq!(map.get("nan").map(String::as_str), Some("null"));
        assert!(!map.contains_key("deep"));
        assert_eq!(truncated, ["deep"]);
        assert_eq!(
            warnings.to_attribute().as_deref(),
            Some("[\"invalid_number\",\"max_depth\"]")
        );
    }

    fn nest(levels: usize) -> AnyValue {
        let mut value = AnyValue {
            value: Some(Value::StringValue("leaf".to_owned())),
        };
        for _ in 0..levels {
            value = AnyValue {
                value: Some(Value::ArrayValue(
                    opentelemetry_proto::tonic::common::v1::ArrayValue {
                        values: vec![value],
                    },
                )),
            };
        }
        value
    }

    fn as_text(value: &AnyValue) -> &str {
        match &value.value {
            Some(Value::StringValue(text)) => text.as_str(),
            other => panic!("expected a string, got {other:?}"),
        }
    }
}
