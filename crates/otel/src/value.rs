//! AnyValue encoding and numeric reads.
//!
//! Spec 02 rules 1, 19 and 20. Field readers call the numeric helpers from N6 on.
#![allow(dead_code)]

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use llmobs_core::names::marker::MAX_DEPTH as DEPTH_MARKER;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValueList};
use serde_json::{Map, Number, Value as Json};

use crate::warn::{Warning, Warnings};

/// Containers allowed below the attribute value. The next container is replaced.
///
/// Depth 0 is the attribute value. A container at depth 32 is the 33rd level
/// (spec 02 rule 19) and becomes [`DEPTH_MARKER`].
pub(crate) const MAX_DEPTH: usize = 32;

/// A number that does not match the field's type. The caller records `invalid_number`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidNumber;

/// Compact JSON for any AnyValue. Spec 02 rule 1.
///
/// An empty value is `null`. A non-finite double is `null` and records
/// `invalid_number`. A key-value list becomes an object in first-seen order;
/// the last duplicate key wins and records `duplicate_key`.
pub(crate) fn to_json(value: &AnyValue, warnings: &mut Warnings) -> Json {
    encode(value, 0, warnings)
}

/// Rule 1 map value: a string is copied verbatim, and every other value is
/// compact JSON text.
pub(crate) fn to_map_string(value: &AnyValue, warnings: &mut Warnings) -> String {
    if let Some(text) = as_str(value) {
        return text.to_owned();
    }
    let json = to_json(value, warnings);
    // Non-finite doubles are already `null`, so this JSON serializes.
    serde_json::to_string(&json).unwrap_or_else(|_| "null".to_owned())
}

/// `stringValue`, or `None` for every other alternative including empty.
pub(crate) fn as_str(value: &AnyValue) -> Option<&str> {
    match &value.value {
        Some(Value::StringValue(text)) => Some(text.as_str()),
        _ => None,
    }
}

/// Integer field. `Ok(None)` is an empty value. Spec 02 rule 20.
pub(crate) fn as_i64(value: &AnyValue) -> Result<Option<i64>, InvalidNumber> {
    match &value.value {
        None | Some(Value::StringValueStrindex(_)) => Ok(None),
        Some(Value::IntValue(number)) => Ok(Some(*number)),
        Some(Value::DoubleValue(number)) => double_as_i64(*number).map(Some),
        Some(Value::StringValue(text)) => parse_i64(text).map(Some),
        Some(_) => Err(InvalidNumber),
    }
}

/// Float field. Non-finite doubles are invalid. Spec 02 rule 20.
pub(crate) fn as_f64(value: &AnyValue) -> Result<Option<f64>, InvalidNumber> {
    match &value.value {
        None | Some(Value::StringValueStrindex(_)) => Ok(None),
        Some(Value::IntValue(number)) => Ok(Some(*number as f64)),
        Some(Value::DoubleValue(number)) if number.is_finite() => Ok(Some(*number)),
        Some(Value::DoubleValue(_)) => Err(InvalidNumber),
        Some(Value::StringValue(text)) => parse_f64(text).map(Some),
        Some(_) => Err(InvalidNumber),
    }
}

/// A list of strings: an array of `stringValue`, or a string holding a JSON
/// array of strings. Any other shape is `None`.
pub(crate) fn as_string_list(value: &AnyValue) -> Option<Vec<String>> {
    match &value.value {
        Some(Value::ArrayValue(array)) => array
            .values
            .iter()
            .map(|item| as_str(item).map(str::to_owned))
            .collect(),
        Some(Value::StringValue(text)) => match serde_json::from_str::<Json>(text) {
            Ok(Json::Array(items)) => items
                .into_iter()
                .map(|item| match item {
                    Json::String(text) => Some(text),
                    _ => None,
                })
                .collect(),
            _ => None,
        },
        _ => None,
    }
}

fn encode(value: &AnyValue, depth: usize, warnings: &mut Warnings) -> Json {
    match &value.value {
        None | Some(Value::StringValueStrindex(_)) => Json::Null,
        Some(Value::StringValue(text)) => Json::String(text.clone()),
        Some(Value::BoolValue(flag)) => Json::Bool(*flag),
        Some(Value::IntValue(number)) => Json::Number((*number).into()),
        Some(Value::DoubleValue(number)) => match Number::from_f64(*number) {
            Some(number) => Json::Number(number),
            None => {
                warnings.add(Warning::InvalidNumber);
                Json::Null
            }
        },
        Some(Value::BytesValue(bytes)) => Json::String(STANDARD.encode(bytes)),
        Some(Value::ArrayValue(array)) => {
            if depth >= MAX_DEPTH {
                warnings.add(Warning::MaxDepth);
                return Json::String(DEPTH_MARKER.to_owned());
            }
            Json::Array(
                array
                    .values
                    .iter()
                    .map(|item| encode(item, depth + 1, warnings))
                    .collect(),
            )
        }
        Some(Value::KvlistValue(list)) => {
            if depth >= MAX_DEPTH {
                warnings.add(Warning::MaxDepth);
                return Json::String(DEPTH_MARKER.to_owned());
            }
            encode_object(list, depth, warnings)
        }
    }
}

fn encode_object(list: &KeyValueList, depth: usize, warnings: &mut Warnings) -> Json {
    let mut object = Map::new();
    for pair in &list.values {
        if object.contains_key(&pair.key) {
            warnings.add(Warning::DuplicateKey);
        }
        let child = match &pair.value {
            Some(value) => encode(value, depth + 1, warnings),
            None => Json::Null,
        };
        object.insert(pair.key.clone(), child);
    }
    Json::Object(object)
}

/// A decimal integer: optional leading `-`, then digits. No spaces, sign `+`,
/// fraction or exponent.
fn parse_i64(text: &str) -> Result<i64, InvalidNumber> {
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err(InvalidNumber);
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(InvalidNumber);
    }
    text.parse().map_err(|_| InvalidNumber)
}

/// A numeric string. Whitespace is invalid. Non-finite results are invalid.
fn parse_f64(text: &str) -> Result<f64, InvalidNumber> {
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err(InvalidNumber);
    }
    let number: f64 = text.parse().map_err(|_| InvalidNumber)?;
    if number.is_finite() {
        Ok(number)
    } else {
        Err(InvalidNumber)
    }
}

fn double_as_i64(number: f64) -> Result<i64, InvalidNumber> {
    if !number.is_finite() || number.fract() != 0.0 {
        return Err(InvalidNumber);
    }
    // `i64::MAX as f64` is 2^63, which is not an `i64`. `i64::MIN` is exact.
    if number < i64::MIN as f64 || number >= i64::MAX as f64 {
        return Err(InvalidNumber);
    }
    Ok(number as i64)
}

#[cfg(test)]
mod tests {
    use opentelemetry_proto::tonic::common::v1::{ArrayValue, KeyValue};

    use super::*;

    fn any(value: Value) -> AnyValue {
        AnyValue { value: Some(value) }
    }

    fn string(text: &str) -> AnyValue {
        any(Value::StringValue(text.to_owned()))
    }

    fn integer(number: i64) -> AnyValue {
        any(Value::IntValue(number))
    }

    fn double(number: f64) -> AnyValue {
        any(Value::DoubleValue(number))
    }

    fn boolean(flag: bool) -> AnyValue {
        any(Value::BoolValue(flag))
    }

    fn bytes(data: &[u8]) -> AnyValue {
        any(Value::BytesValue(data.to_vec()))
    }

    fn array(values: Vec<AnyValue>) -> AnyValue {
        any(Value::ArrayValue(ArrayValue { values }))
    }

    fn kvlist(pairs: Vec<KeyValue>) -> AnyValue {
        any(Value::KvlistValue(KeyValueList { values: pairs }))
    }

    fn pair(key: &str, value: AnyValue) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(value),
            key_strindex: 0,
        }
    }

    fn nest_arrays(levels: usize, leaf: AnyValue) -> AnyValue {
        let mut value = leaf;
        for _ in 0..levels {
            value = array(vec![value]);
        }
        value
    }

    fn nest_objects(levels: usize, leaf: AnyValue) -> AnyValue {
        let mut value = leaf;
        for _ in 0..levels {
            value = kvlist(vec![pair("k", value)]);
        }
        value
    }

    fn json_arrays(levels: usize, leaf: Json) -> Json {
        let mut value = leaf;
        for _ in 0..levels {
            value = Json::Array(vec![value]);
        }
        value
    }

    #[test]
    fn string_bool_int_and_double_follow_rule_1() {
        let mut warnings = Warnings::default();
        assert_eq!(to_json(&string("a\"b"), &mut warnings), Json::from("a\"b"));
        assert_eq!(to_map_string(&string("a\"b"), &mut warnings), "a\"b");
        assert_eq!(to_json(&boolean(true), &mut warnings), Json::from(true));
        assert_eq!(to_map_string(&boolean(false), &mut warnings), "false");
        assert_eq!(to_json(&integer(1200), &mut warnings), Json::from(1200));
        assert_eq!(to_map_string(&integer(-3), &mut warnings), "-3");
        let finite = Number::from_f64(0.2).expect("0.2 is finite");
        assert_eq!(to_json(&double(0.2), &mut warnings), Json::Number(finite));
        assert_eq!(to_map_string(&double(0.2), &mut warnings), "0.2");
        assert!(warnings.to_attribute().is_none());
    }

    #[test]
    fn array_bytes_empty_and_profiling_index_follow_rule_1() {
        let mut warnings = Warnings::default();
        let listed = array(vec![string("stop"), AnyValue { value: None }]);
        assert_eq!(
            to_json(&listed, &mut warnings),
            Json::Array(vec![Json::from("stop"), Json::Null])
        );
        assert_eq!(to_map_string(&listed, &mut warnings), "[\"stop\",null]");

        assert_eq!(to_map_string(&bytes(b"hi"), &mut warnings), "\"aGk=\"");
        assert_eq!(
            to_json(&AnyValue { value: None }, &mut warnings),
            Json::Null
        );
        assert_eq!(
            to_map_string(&AnyValue { value: None }, &mut warnings),
            "null"
        );
        let profiling = any(Value::StringValueStrindex(7));
        assert_eq!(to_json(&profiling, &mut warnings), Json::Null);
        assert_eq!(to_map_string(&profiling, &mut warnings), "null");
        assert!(warnings.to_attribute().is_none());
    }

    #[test]
    fn kvlist_is_an_object_and_nested_values_keep_their_types() {
        let mut warnings = Warnings::default();
        let value = kvlist(vec![
            pair("b", boolean(true)),
            pair("a", integer(1)),
            pair(
                "nested",
                array(vec![kvlist(vec![pair("bytes", bytes(&[0xff]))])]),
            ),
            KeyValue {
                key: "missing".to_owned(),
                value: None,
                key_strindex: 0,
            },
        ]);
        assert_eq!(
            to_map_string(&value, &mut warnings),
            "{\"b\":true,\"a\":1,\"nested\":[{\"bytes\":\"/w==\"}],\"missing\":null}"
        );
        assert!(warnings.to_attribute().is_none());
    }

    #[test]
    fn non_finite_doubles_are_null_and_warn() {
        let mut warnings = Warnings::default();
        let value = array(vec![
            double(f64::NAN),
            double(f64::INFINITY),
            double(f64::NEG_INFINITY),
        ]);
        assert_eq!(to_map_string(&value, &mut warnings), "[null,null,null]");
        assert_eq!(
            warnings.to_attribute().as_deref(),
            Some("[\"invalid_number\"]")
        );
    }

    #[test]
    fn duplicate_kvlist_keys_keep_the_last_value() {
        let mut warnings = Warnings::default();
        let value = kvlist(vec![
            pair("a", integer(1)),
            pair("a", integer(2)),
            pair("a", string("last")),
            pair("b", integer(3)),
        ]);
        assert_eq!(
            to_map_string(&value, &mut warnings),
            "{\"a\":\"last\",\"b\":3}"
        );
        assert_eq!(
            warnings.to_attribute().as_deref(),
            Some("[\"duplicate_key\"]")
        );
    }

    #[test]
    fn depth_32_is_kept_and_depth_33_is_the_marker() {
        let leaf = string("leaf");
        let mut warnings = Warnings::default();
        assert_eq!(
            to_json(&nest_arrays(32, leaf.clone()), &mut warnings),
            json_arrays(32, Json::from("leaf"))
        );
        assert!(warnings.to_attribute().is_none());

        assert_eq!(
            to_json(&nest_arrays(33, leaf.clone()), &mut warnings),
            json_arrays(32, Json::from(DEPTH_MARKER))
        );
        assert_eq!(warnings.to_attribute().as_deref(), Some("[\"max_depth\"]"));

        let mut object_warnings = Warnings::default();
        let mut expected = Json::from(DEPTH_MARKER);
        for _ in 0..32 {
            expected = Json::Object(Map::from_iter([("k".to_owned(), expected)]));
        }
        assert_eq!(
            to_json(&nest_objects(33, leaf), &mut object_warnings),
            expected
        );
        assert_eq!(
            object_warnings.to_attribute().as_deref(),
            Some("[\"max_depth\"]")
        );
    }

    #[test]
    fn a_replaced_subtree_is_not_walked() {
        let mut warnings = Warnings::default();
        let hidden = kvlist(vec![
            pair("secret", string("SECRET")),
            pair("secret", integer(1)),
        ]);
        let value = nest_arrays(33, hidden);
        let encoded = to_map_string(&value, &mut warnings);
        assert!(!encoded.contains("SECRET"));
        assert_eq!(warnings.to_attribute().as_deref(), Some("[\"max_depth\"]"));
    }

    #[test]
    fn numbers_follow_rule_20() {
        let cases = [
            (AnyValue { value: None }, Ok(None), Ok(None)),
            (any(Value::StringValueStrindex(1)), Ok(None), Ok(None)),
            (integer(12), Ok(Some(12)), Ok(Some(12.0))),
            (integer(-4), Ok(Some(-4)), Ok(Some(-4.0))),
            (
                integer(i64::MAX),
                Ok(Some(i64::MAX)),
                Ok(Some(i64::MAX as f64)),
            ),
            (
                integer(i64::MIN),
                Ok(Some(i64::MIN)),
                Ok(Some(i64::MIN as f64)),
            ),
            (double(12.0), Ok(Some(12)), Ok(Some(12.0))),
            (double(-0.0), Ok(Some(0)), Ok(Some(-0.0))),
            (double(-1.5), Err(()), Ok(Some(-1.5))),
            (double(f64::NAN), Err(()), Err(())),
            (double(f64::INFINITY), Err(()), Err(())),
            (double(1e20), Err(()), Ok(Some(1e20))),
            (
                double(i64::MIN as f64),
                Ok(Some(i64::MIN)),
                Ok(Some(i64::MIN as f64)),
            ),
            (double(i64::MAX as f64), Err(()), Ok(Some(i64::MAX as f64))),
            (string("12"), Ok(Some(12)), Ok(Some(12.0))),
            (string("-8"), Ok(Some(-8)), Ok(Some(-8.0))),
            (string("012"), Ok(Some(12)), Ok(Some(12.0))),
            (string("12.0"), Err(()), Ok(Some(12.0))),
            (string(" 12"), Err(()), Err(())),
            (string("12 "), Err(()), Err(())),
            (string("1e3"), Err(()), Ok(Some(1000.0))),
            (string("+12"), Err(()), Ok(Some(12.0))),
            (string(""), Err(()), Err(())),
            (string("no"), Err(()), Err(())),
            (string("inf"), Err(()), Err(())),
            (string("NaN"), Err(()), Err(())),
            (boolean(true), Err(()), Err(())),
            (array(vec![integer(1)]), Err(()), Err(())),
            (bytes(b"1"), Err(()), Err(())),
        ];
        for (value, int, float) in cases {
            assert_eq!(as_i64(&value).map_err(|_| ()), int, "{value:?}");
            assert_eq!(as_f64(&value).map_err(|_| ()), float, "{value:?}");
        }
    }

    #[test]
    fn string_lists_accept_arrays_and_json_array_strings() {
        assert_eq!(
            as_string_list(&array(vec![string("a"), string("b")])).as_deref(),
            Some(["a".to_owned(), "b".to_owned()].as_slice())
        );
        assert_eq!(
            as_string_list(&array(vec![])).as_deref(),
            Some([].as_slice())
        );
        assert_eq!(
            as_string_list(&string("[\"a\", \"b\"]")).as_deref(),
            Some(["a".to_owned(), "b".to_owned()].as_slice())
        );
        assert_eq!(
            as_string_list(&string("[]")).as_deref(),
            Some([].as_slice())
        );
        assert_eq!(as_string_list(&array(vec![string("a"), integer(1)])), None);
        assert_eq!(as_string_list(&string("[\"a\",1]")), None);
        assert_eq!(as_string_list(&string("[[\"a\"]]")), None);
        assert_eq!(as_string_list(&string("not-json")), None);
        assert_eq!(as_string_list(&string("\"a\"")), None);
        assert_eq!(as_string_list(&integer(1)), None);
        assert_eq!(as_string_list(&AnyValue { value: None }), None);
        assert_eq!(as_str(&string("plain")), Some("plain"));
        assert_eq!(as_str(&integer(1)), None);
    }
}
