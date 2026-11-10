//! Owned score record written by the API and read by storage.
//!
//! Spec 01 § Score. The columnar `target`, `trace_id`, `span_id`, `data_type`, `value_num`,
//! and `value_str` columns are derived from [`ScoreTarget`] and [`ScoreValue`] (D60).

use std::collections::BTreeMap;

use serde::de::{self, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::enums::{ScoreDataType, ScoreSource, ScoreTargetKind};
use crate::ids::{ProjectId, SpanId, TraceId};

/// What a score is attached to.
///
/// JSON is internally tagged: `{"type":"span","trace_id":"…","span_id":"…"}`. Spec 01 § Score `target`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScoreTarget {
    /// One span. Both ids are required.
    Span {
        /// Trace that owns the span.
        trace_id: TraceId,
        /// The span.
        span_id: SpanId,
    },
    /// One trace.
    Trace {
        /// The trace.
        trace_id: TraceId,
    },
    /// One session.
    Session {
        /// Session id.
        session_id: String,
    },
}

impl ScoreTarget {
    /// The `target` column value. Spec 01 § Score `target`.
    pub fn kind(&self) -> ScoreTargetKind {
        match self {
            Self::Span { .. } => ScoreTargetKind::Span,
            Self::Trace { .. } => ScoreTargetKind::Trace,
            Self::Session { .. } => ScoreTargetKind::Session,
        }
    }
}

/// The value carried by a score.
///
/// JSON is adjacently tagged: `{"data_type":"numeric","value":0.8}`. A categorical value is
/// `{"data_type":"categorical","value":{"label":"correct","value":1.0}}`. Spec 01 § Score.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(tag = "data_type", content = "value", rename_all = "snake_case")]
pub enum ScoreValue {
    /// A finite number.
    Numeric(f64),
    /// True or false. Stored numerically as 1 or 0.
    Boolean(bool),
    /// A label and an optional numeric mapping.
    Categorical {
        /// Category label. Stored in `value_str`.
        label: String,
        /// Optional numeric mapping. Stored in `value_num`. Omitted from JSON when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
    },
    /// Free text.
    Text(String),
}

impl ScoreValue {
    /// The `data_type` column. Spec 01 § Score `data_type`.
    pub fn data_type(&self) -> ScoreDataType {
        match self {
            Self::Numeric(_) => ScoreDataType::Numeric,
            Self::Boolean(_) => ScoreDataType::Boolean,
            Self::Categorical { .. } => ScoreDataType::Categorical,
            Self::Text(_) => ScoreDataType::Text,
        }
    }

    /// The `value_num` column.
    ///
    /// Numeric values pass through. Booleans become 1 or 0. Categorical scores use the optional
    /// mapping. Text has no number. Spec 01 § Score `value_num`.
    pub fn value_num(&self) -> Option<f64> {
        match self {
            Self::Numeric(value) => Some(*value),
            Self::Boolean(value) => Some(if *value { 1.0 } else { 0.0 }),
            Self::Categorical { value, .. } => *value,
            Self::Text(_) => None,
        }
    }

    /// The `value_str` column: a categorical label or the text. Spec 01 § Score `value_str`.
    pub fn value_str(&self) -> Option<&str> {
        match self {
            Self::Categorical { label, .. } => Some(label),
            Self::Text(text) => Some(text),
            Self::Numeric(_) | Self::Boolean(_) => None,
        }
    }
}

impl<'de> Deserialize<'de> for ScoreValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ScoreValueVisitor;

        impl<'de> Visitor<'de> for ScoreValueVisitor {
            type Value = ScoreValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a score value tagged by data_type")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut data_type = None;
                let mut value = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "data_type" => data_type = Some(map.next_value::<String>()?),
                        "value" => value = Some(map.next_value::<serde_json::Value>()?),
                        _ => {
                            let _ = map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                let data_type = data_type.ok_or_else(|| de::Error::missing_field("data_type"))?;
                let value = value.ok_or_else(|| de::Error::missing_field("value"))?;
                match data_type.as_str() {
                    "numeric" => {
                        let number = value.as_f64().ok_or_else(|| {
                            de::Error::custom("numeric score value must be a finite number")
                        })?;
                        finite_f64(number)
                            .map(ScoreValue::Numeric)
                            .map_err(de::Error::custom)
                    }
                    "boolean" => value.as_bool().map(ScoreValue::Boolean).ok_or_else(|| {
                        de::Error::custom("boolean score value must be true or false")
                    }),
                    "categorical" => {
                        let label = value
                            .get("label")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| de::Error::custom("categorical score requires a label"))?
                            .to_owned();
                        let mapped = match value.get("value") {
                            None | Some(serde_json::Value::Null) => None,
                            Some(number) => Some(number.as_f64().ok_or_else(|| {
                                de::Error::custom("categorical value must be a number")
                            })?),
                        };
                        Ok(ScoreValue::Categorical {
                            label,
                            value: mapped,
                        })
                    }
                    "text" => value
                        .as_str()
                        .map(|text| ScoreValue::Text(text.to_owned()))
                        .ok_or_else(|| de::Error::custom("text score value must be a string")),
                    _ => Err(de::Error::custom("unknown score data_type")),
                }
            }
        }

        deserializer.deserialize_map(ScoreValueVisitor)
    }
}

fn finite_f64(value: f64) -> Result<f64, &'static str> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err("numeric score must be finite")
    }
}

/// One score row.
///
/// Spec 01 § Score. JSON omits empty `Option` and `BTreeMap` fields. Instants are RFC 3339 (D64).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Score {
    /// Partition key. Spec 01 `project_id`.
    pub project_id: ProjectId,
    /// Last-writer-wins key. Spec 01 `score_id`.
    pub score_id: Uuid,
    /// Span, trace, or session this score is attached to. Spec 01 `target`.
    pub target: ScoreTarget,
    /// Session of a span or trace target, when known. Ignored for session targets. Spec 01 `session_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// End-user id, when the writer knows it. Spec 01 `user_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Score name, for example `hallucination`. Spec 01 `name`.
    pub name: String,
    /// The score value. Spec 01 `data_type`, `value_num`, `value_str`.
    pub value: ScoreValue,
    /// Explanation. Spec 01 `reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Who wrote the score. Spec 01 `source`.
    pub source: ScoreSource,
    /// Evaluator that produced the score. Spec 01 `evaluator_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_id: Option<Uuid>,
    /// Online rule that produced the score. Spec 01 `rule_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<Uuid>,
    /// Experiment the score belongs to. Spec 01 `experiment_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<Uuid>,
    /// Dataset item the score belongs to. Spec 01 `dataset_item_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_item_id: Option<Uuid>,
    /// Person who annotated, for `human` scores. Spec 01 `author_user_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_user_id: Option<Uuid>,
    /// Evaluator metadata. Values are strings. Spec 01 `metadata` and spec 06.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Creation time: `i64` nanoseconds in Rust, an RFC 3339 string in JSON (D64). Spec 01 `created_at`.
    #[serde(with = "crate::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// Last-writer-wins version: `i64` nanoseconds in Rust, an RFC 3339 string in JSON (D64). Spec 01 `updated_at`.
    #[serde(with = "crate::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: i64,
    /// Tombstone. Spec 01 `deleted`.
    pub deleted: bool,
    /// Server receive time: `i64` nanoseconds in Rust, an RFC 3339 string in JSON (D64). Spec 01 `ingest_time`.
    #[serde(with = "crate::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub ingest_time: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn trace() -> TraceId {
        "4bf92f3577b34da6a3ce929d0e0e4736".parse().unwrap()
    }

    fn span() -> SpanId {
        "00f067aa0ba902b7".parse().unwrap()
    }

    fn targets() -> Vec<ScoreTarget> {
        vec![
            ScoreTarget::Span {
                trace_id: trace(),
                span_id: span(),
            },
            ScoreTarget::Trace { trace_id: trace() },
            ScoreTarget::Session {
                session_id: "chat-42".to_owned(),
            },
        ]
    }

    fn values() -> Vec<ScoreValue> {
        vec![
            ScoreValue::Numeric(0.8),
            ScoreValue::Boolean(true),
            ScoreValue::Categorical {
                label: "correct".to_owned(),
                value: Some(1.0),
            },
            ScoreValue::Categorical {
                label: "unsure".to_owned(),
                value: None,
            },
            ScoreValue::Text("thumbs up".to_owned()),
        ]
    }

    fn sample(target: ScoreTarget, value: ScoreValue) -> Score {
        let mut metadata = BTreeMap::new();
        metadata.insert("judge.model".to_owned(), "gpt-5-mini".to_owned());
        Score {
            project_id: project(),
            score_id: "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap(),
            target,
            session_id: Some("chat-42".to_owned()),
            user_id: Some("u-17".to_owned()),
            name: "accuracy".to_owned(),
            value,
            reason: Some("looks right".to_owned()),
            source: ScoreSource::LlmJudge,
            evaluator_id: Some("018f6b4e-7c1a-7c1a-8c1a-000000000003".parse().unwrap()),
            rule_id: Some("018f6b4e-7c1a-7c1a-8c1a-000000000004".parse().unwrap()),
            experiment_id: Some("018f6b4e-7c1a-7c1a-8c1a-000000000005".parse().unwrap()),
            dataset_item_id: Some("018f6b4e-7c1a-7c1a-8c1a-000000000006".parse().unwrap()),
            author_user_id: Some("018f6b4e-7c1a-7c1a-8c1a-000000000007".parse().unwrap()),
            metadata,
            created_at: crate::time_serde::parse_rfc3339("2026-11-09T10:12:03.123456789Z").unwrap(),
            updated_at: crate::time_serde::parse_rfc3339("2026-11-09T10:12:04.000000000Z").unwrap(),
            deleted: false,
            ingest_time: crate::time_serde::parse_rfc3339("2026-11-09T10:12:05.000000000Z")
                .unwrap(),
        }
    }

    #[test]
    fn each_target_and_value_round_trips() {
        for target in targets() {
            for value in values() {
                let score = sample(target.clone(), value);
                let json = serde_json::to_string(&score).unwrap();
                assert_eq!(serde_json::from_str::<Score>(&json).unwrap(), score);
            }
        }
    }

    #[test]
    fn value_columns_follow_the_data_type() {
        let cases = [
            (
                ScoreValue::Numeric(0.8),
                ScoreDataType::Numeric,
                Some(0.8),
                None,
            ),
            (
                ScoreValue::Boolean(true),
                ScoreDataType::Boolean,
                Some(1.0),
                None,
            ),
            (
                ScoreValue::Boolean(false),
                ScoreDataType::Boolean,
                Some(0.0),
                None,
            ),
            (
                ScoreValue::Categorical {
                    label: "correct".to_owned(),
                    value: Some(1.0),
                },
                ScoreDataType::Categorical,
                Some(1.0),
                Some("correct"),
            ),
            (
                ScoreValue::Categorical {
                    label: "unsure".to_owned(),
                    value: None,
                },
                ScoreDataType::Categorical,
                None,
                Some("unsure"),
            ),
            (
                ScoreValue::Text("note".to_owned()),
                ScoreDataType::Text,
                None,
                Some("note"),
            ),
        ];
        for (value, data_type, value_num, value_str) in cases {
            assert_eq!(value.data_type(), data_type);
            assert_eq!(value.value_num(), value_num);
            assert_eq!(value.value_str(), value_str);
        }
    }

    #[test]
    fn target_kind_matches_the_variant() {
        assert_eq!(targets()[0].kind(), ScoreTargetKind::Span);
        assert_eq!(targets()[1].kind(), ScoreTargetKind::Trace);
        assert_eq!(targets()[2].kind(), ScoreTargetKind::Session);
    }

    #[test]
    fn non_finite_numeric_rejected() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(finite_f64(bad), Err("numeric score must be finite"));
        }
        assert!(finite_f64(0.0).is_ok());
        let err = serde_json::from_str::<ScoreValue>(r#"{"data_type":"numeric","value":null}"#);
        assert!(err.is_err());
    }

    #[test]
    fn categorical_without_number_omits_it() {
        let value = ScoreValue::Categorical {
            label: "unsure".to_owned(),
            value: None,
        };
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"data_type":"categorical","value":{"label":"unsure"}}"#
        );
    }

    fn schema_pretty<T: utoipa::PartialSchema>() -> String {
        serde_json::to_string_pretty(&T::schema()).unwrap()
    }

    #[test]
    fn score_schemas_snapshot() {
        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("score_target_schema", schema_pretty::<ScoreTarget>());
            insta::assert_snapshot!("score_value_schema", schema_pretty::<ScoreValue>());
            insta::assert_snapshot!("score_schema", schema_pretty::<Score>());
        });
    }
}
