use serde_json::{Map, Value};
use utoipa::{PartialSchema, ToSchema};

use crate::message::{Modality, Part};
use crate::score::ScoreValue;

fn registered<T: ToSchema>() -> Map<String, Value> {
    let mut schemas = Vec::new();
    T::schemas(&mut schemas);
    schemas
        .into_iter()
        .map(|(name, schema)| (name, serde_json::to_value(&schema).unwrap()))
        .collect()
}

fn resolve<'a>(value: &'a Value, components: &'a Map<String, Value>) -> &'a Value {
    let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
        return value;
    };
    let name = reference
        .rsplit('/')
        .next()
        .expect("schema $ref has a name");
    components
        .get(name)
        .unwrap_or_else(|| panic!("missing schema component {name}"))
}

fn property_enum(schema: &Value, property: &str, components: &Map<String, Value>) -> Vec<String> {
    let schema = resolve(schema, components);
    let Some(property) = schema
        .get("properties")
        .and_then(|props| props.get(property))
    else {
        return Vec::new();
    };
    let property = resolve(property, components);
    property
        .get("enum")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn branches(schema: &Value, components: &Map<String, Value>) -> Vec<Value> {
    let schema = resolve(schema, components);
    schema
        .get("oneOf")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| resolve(item, components).clone())
                .collect()
        })
        .unwrap_or_else(|| vec![schema.clone()])
}

#[test]
fn part_and_score_value_tags_match_their_schemas() {
    let part_components = registered::<Part>();
    let part_schema = serde_json::to_value(Part::schema()).unwrap();
    let parts = [
        Part::Text {
            content: "hi".to_owned(),
            extra: Map::new(),
        },
        Part::Reasoning {
            content: "because".to_owned(),
            extra: Map::new(),
        },
        Part::ToolCall {
            id: Some("c1".to_owned()),
            name: "lookup".to_owned(),
            arguments: Some(serde_json::json!({"q": 1})),
            extra: Map::new(),
        },
        Part::ToolCallResponse {
            id: None,
            response: serde_json::json!("ok"),
            extra: Map::new(),
        },
        Part::Uri {
            modality: Some(Modality::Image),
            mime_type: Some("image/png".to_owned()),
            uri: "media://abc".to_owned(),
            extra: Map::new(),
        },
        Part::Blob {
            modality: None,
            mime_type: None,
            content: "AAAA".to_owned(),
            extra: Map::new(),
        },
        Part::Unknown(serde_json::json!({"type": "image_url"})),
    ];
    for part in &parts {
        let json = serde_json::to_value(part).unwrap();
        if matches!(part, Part::Unknown(_)) {
            let unknown = part_components
                .get("UnknownPart")
                .expect("UnknownPart component");
            assert!(unknown["properties"]["type"].get("enum").is_none());
            let pattern = unknown["properties"]["type"]["pattern"]
                .as_str()
                .expect("UnknownPart type has a pattern");
            for known in [
                "text",
                "reasoning",
                "tool_call",
                "tool_call_response",
                "uri",
                "blob",
            ] {
                assert!(
                    pattern.contains(&format!("|{known}|"))
                        || pattern.contains(&format!("({known}|"))
                        || pattern.contains(&format!("|{known})")),
                    "UnknownPart pattern does not exclude {known}"
                );
            }
            continue;
        }
        let tag = json["type"].as_str().expect("known part has type");
        let matched = branches(&part_schema, &part_components)
            .iter()
            .any(|branch| property_enum(branch, "type", &part_components) == [tag]);
        assert!(matched, "no part schema enum contains {tag}");
    }

    let value_components = registered::<ScoreValue>();
    let value_schema = serde_json::to_value(ScoreValue::schema()).unwrap();
    let values = [
        ScoreValue::Numeric(0.5),
        ScoreValue::Boolean(true),
        ScoreValue::Categorical {
            label: "friendly".to_owned(),
            value: Some(1.0),
        },
        ScoreValue::Text("note".to_owned()),
    ];
    for value in &values {
        let json = serde_json::to_value(value).unwrap();
        let tag = json["data_type"]
            .as_str()
            .expect("score value has data_type");
        let matched = branches(&value_schema, &value_components)
            .iter()
            .any(|branch| {
                property_enum(branch, "data_type", &value_components)
                    .iter()
                    .any(|item| item == tag)
            });
        assert!(matched, "no score value schema enum contains {tag}");
    }
}
