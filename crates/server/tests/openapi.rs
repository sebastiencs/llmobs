//! The committed OpenAPI contract and the checks that keep it honest.
//!
//! Spec 09. Regenerate with `just openapi`.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use serde_json::Value;

use llmobs_server::api::openapi::openapi;

const OPERATIONS: &[(&str, &str, &str)] = &[
    ("get", "/api/auth/config", "get_auth_config"),
    ("get", "/api/auth/me", "get_me"),
    ("get", "/api/v1/orgs/{org}/projects", "list_projects"),
    ("get", "/api/v1/projects/{project}", "get_project"),
    ("get", "/api/v1/projects/{project}/traces", "list_traces"),
    (
        "get",
        "/api/v1/projects/{project}/traces/stream",
        "stream_traces",
    ),
    (
        "get",
        "/api/v1/projects/{project}/traces/{trace_id}",
        "get_trace",
    ),
    (
        "get",
        "/api/v1/projects/{project}/traces/{trace_id}/stream",
        "stream_trace",
    ),
    (
        "get",
        "/api/v1/projects/{project}/traces/{trace_id}/spans/{span_id}",
        "get_span",
    ),
    ("get", "/api/v1/projects/{project}/spans", "list_spans"),
    (
        "get",
        "/api/v1/projects/{project}/sessions",
        "list_sessions",
    ),
    (
        "get",
        "/api/v1/projects/{project}/sessions/{session_id}",
        "get_session",
    ),
    ("get", "/api/v1/projects/{project}/users", "list_users"),
    (
        "get",
        "/api/v1/projects/{project}/users/{user_id}",
        "get_user",
    ),
    ("get", "/api/v1/projects/{project}/scores", "list_scores"),
    ("get", "/api/v1/projects/{project}/schema", "get_schema"),
    (
        "get",
        "/api/v1/projects/{project}/saved-views",
        "list_saved_views",
    ),
    (
        "post",
        "/api/v1/projects/{project}/saved-views",
        "create_saved_view",
    ),
    (
        "patch",
        "/api/v1/projects/{project}/saved-views/{id}",
        "update_saved_view",
    ),
    (
        "delete",
        "/api/v1/projects/{project}/saved-views/{id}",
        "delete_saved_view",
    ),
    ("get", "/api/v1/projects/{project}/metrics", "get_metrics"),
    (
        "get",
        "/api/v1/projects/{project}/media/{sha256}",
        "get_media",
    ),
];

const QUERY_OR_BODY: &[&str] = &[
    "list_projects",
    "list_traces",
    "stream_traces",
    "get_trace",
    "list_spans",
    "list_sessions",
    "get_session",
    "list_users",
    "get_user",
    "list_scores",
    "list_saved_views",
    "create_saved_view",
    "update_saved_view",
    "get_metrics",
];

const TAGS: &[&str] = &[
    "auth",
    "projects",
    "traces",
    "spans",
    "sessions",
    "users",
    "scores",
    "schema",
    "saved_views",
    "metrics",
    "media",
];

const METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace", "query",
];

#[test]
fn contract_file_is_up_to_date() {
    let rendered = rendered_document();
    let path = contract_path();
    if openapi_write_requested() {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        existing == rendered,
        "crates/server/openapi.json is out of date. Run `just openapi`."
    );
}

#[test]
fn document_lint() {
    let document: Value = serde_json::from_str(&rendered_document()).unwrap();
    let schemas = document["components"]["schemas"]
        .as_object()
        .expect("components.schemas");
    let tag_names: HashSet<&str> = document["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .map(|tag| tag["name"].as_str().unwrap())
        .collect();
    assert_eq!(tag_names, TAGS.iter().copied().collect());

    let found = operations(&document);
    let expected: BTreeSet<_> = OPERATIONS
        .iter()
        .map(|(method, path, id)| (*method, *path, *id))
        .collect();
    assert_eq!(found, expected);

    let security = document["security"].as_array().expect("security");
    assert_eq!(security.len(), 2);
    assert!(
        security
            .iter()
            .any(|item| item.get("session_cookie").is_some())
    );
    assert!(security.iter().any(|item| item.get("bearer").is_some()));

    for (method, path, id) in &found {
        let operation = operation(&document, method, path);
        let tags = operation["tags"].as_array().expect(id);
        assert_eq!(tags.len(), 1, "{id}");
        let tag = tags[0].as_str().unwrap();
        assert!(tag_names.contains(tag), "{id} tag {tag}");
        assert!(
            !operation["summary"].as_str().unwrap_or("").is_empty(),
            "{id}"
        );

        let operation_security = operation.get("security").and_then(Value::as_array);
        if *id == "get_auth_config" {
            assert_eq!(operation_security.map(Vec::len), Some(0), "{id}");
        } else {
            assert!(operation_security.is_none(), "{id} overrides security");
        }

        let errors: BTreeSet<&str> = operation["responses"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|code| code.starts_with('4'))
            .map(String::as_str)
            .collect();
        let mut expected_errors = BTreeSet::new();
        if *id == "get_me" {
            expected_errors.extend(["401", "429"]);
        } else if *id == "get_media" {
            expected_errors.extend(["401", "403", "404", "416", "429"]);
        } else if *id != "get_auth_config" && QUERY_OR_BODY.contains(id) {
            expected_errors.extend(["400", "401", "403", "404", "429"]);
        } else if *id != "get_auth_config" {
            expected_errors.extend(["401", "403", "404", "429"]);
        }
        assert_eq!(errors, expected_errors, "{id}");
        for code in &errors {
            let response = &operation["responses"][code];
            let content = response["content"]["application/problem+json"]["schema"]["$ref"]
                .as_str()
                .unwrap_or("");
            assert_eq!(content, "#/components/schemas/Problem", "{id} {code}");
        }

        let mut declared = path_parameters(operation);
        if let Some(shared) = document["paths"][path]["parameters"].as_array() {
            declared.extend(parameter_names(shared, "path"));
        }
        for name in brace_names(path) {
            assert!(
                declared.contains(name),
                "{path} missing {name}: {}",
                operation["parameters"]
            );
        }
        if let Some(parameters) = operation["parameters"].as_array() {
            for parameter in parameters {
                if parameter["in"] == "path" {
                    assert_eq!(parameter["required"], true, "{path} {}", parameter["name"]);
                }
                if parameter["in"] == "query" {
                    assert!(
                        !schema_is_nullable(&parameter["schema"]),
                        "{id} query {} is nullable",
                        parameter["name"]
                    );
                }
            }
        }
        if let Some(parameters) = document["paths"][path]["parameters"].as_array() {
            for parameter in parameters {
                if parameter["in"] == "query" {
                    assert!(
                        !schema_is_nullable(&parameter["schema"]),
                        "{path} query {} is nullable",
                        parameter["name"]
                    );
                }
            }
        }
    }

    walk_refs(&document, &mut |reference| {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .unwrap_or_else(|| panic!("unresolved $ref {reference}"));
        assert!(schemas.contains_key(name), "missing schema {name}");
    });

    for (name, schema) in schemas {
        assert!(is_pascal(name), "component {name}");
        check_property_names(schema, name);
    }

    let pages = [
        ("TracePage", "TraceSummary"),
        ("SpanPage", "SpanSummary"),
        ("SessionPage", "SessionSummary"),
        ("EndUserPage", "EndUserSummary"),
        ("ScorePage", "Score"),
        ("ProjectPage", "Project"),
        ("SavedViewPage", "SavedView"),
    ];
    for (page, item) in pages {
        let schema = &schemas[page];
        let reference = schema["properties"]["data"]["items"]["$ref"]
            .as_str()
            .unwrap_or_else(|| panic!("{page} data items"));
        assert_eq!(reference, format!("#/components/schemas/{item}"));
        let required = schema["required"].as_array().unwrap();
        assert!(
            required.iter().any(|field| field == "next_cursor"),
            "{page}"
        );
        let cursor_types = &schema["properties"]["next_cursor"]["type"];
        assert!(
            cursor_types
                .as_array()
                .unwrap()
                .iter()
                .any(|ty| ty == "null")
        );
    }

    assert!(
        schemas["SpanSummary"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "tags")
    );
    assert_ne!(schemas["MessageRole"], schemas["OrgRole"]);
    assert!(schemas.contains_key("MessageRole"));
    assert!(schemas.contains_key("OrgRole"));
    assert_nullability(schemas);
    assert_exclusions(&document);
}

fn openapi_write_requested() -> bool {
    write_requested("LLMOBS_OPENAPI_WRITE")
}

fn write_requested(name: &str) -> bool {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => false,
        Ok(value) if value == "1" => true,
        Ok(value) if value.is_empty() => false,
        Ok(_) | Err(_) => panic!("set {name}=1 to write, or unset it"),
    }
}

fn rendered_document() -> String {
    let json = serde_json::to_string_pretty(&openapi())
        .unwrap_or_else(|error| panic!("openapi json: {error}"));
    format!("{json}\n")
}

fn contract_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.json")
}

fn operations(document: &Value) -> BTreeSet<(&str, &str, &str)> {
    let mut found = BTreeSet::new();
    let Some(paths) = document["paths"].as_object() else {
        panic!("paths");
    };
    for (path, item) in paths {
        for method in METHODS {
            let Some(operation) = item.get(*method) else {
                continue;
            };
            let Some(id) = operation["operationId"].as_str() else {
                panic!("{method} {path} has no operationId");
            };
            found.insert((*method, path.as_str(), id));
        }
    }
    found
}

fn operation<'a>(document: &'a Value, method: &str, path: &str) -> &'a Value {
    &document["paths"][path][method]
}

fn path_parameters(operation: &Value) -> HashSet<&str> {
    operation["parameters"]
        .as_array()
        .map(|parameters| parameter_names(parameters, "path"))
        .unwrap_or_default()
}

fn parameter_names<'a>(parameters: &'a [Value], location: &str) -> HashSet<&'a str> {
    parameters
        .iter()
        .filter(|parameter| parameter["in"] == location)
        .filter_map(|parameter| parameter["name"].as_str())
        .collect()
}

fn brace_names(path: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            break;
        };
        names.push(&after[..end]);
        rest = &after[end + 1..];
    }
    names
}

fn walk_refs(value: &Value, visit: &mut impl FnMut(&str)) {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                visit(reference);
            }
            for child in map.values() {
                walk_refs(child, visit);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk_refs(child, visit);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn check_property_names(schema: &Value, owner: &str) {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            assert!(is_snake(name), "{owner}.{name}");
            check_property_names(child, name);
        }
    }
    for key in ["items", "additionalProperties", "not"] {
        if let Some(child) = schema.get(key).filter(|child| child.is_object()) {
            check_property_names(child, owner);
        }
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(items) = schema.get(key).and_then(Value::as_array) {
            for child in items {
                check_property_names(child, owner);
            }
        }
    }
}

fn assert_exclusions(document: &Value) {
    const EXPECTED: &[&str] = &[
        "get_metrics",
        "get_session",
        "get_user",
        "list_sessions",
        "list_spans",
        "list_traces",
        "list_users",
        "stream_traces",
    ];
    const TEXT: &str = "Evaluation traces (`environment = llmobs-eval`) and experiment traces are excluded unless `include_eval` or `include_experiments` is set.";
    let mut found = BTreeSet::new();
    let Some(paths) = document["paths"].as_object() else {
        panic!("paths");
    };
    for (path, item) in paths {
        let Some(operations) = item.as_object() else {
            continue;
        };
        for (method, operation) in operations {
            if method == "parameters" {
                continue;
            }
            let Some(operation) = operation.as_object() else {
                continue;
            };
            let id = operation["operationId"].as_str().unwrap_or(path);
            let names: HashSet<&str> = operation
                .get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|parameter| parameter["in"] == "query")
                .filter_map(|parameter| parameter["name"].as_str())
                .collect();
            if !names.contains("include_eval") {
                continue;
            }
            found.insert(id.to_owned());
            assert!(
                names.contains("include_experiments"),
                "{id} is missing include_experiments"
            );
            let description = operation["description"].as_str().unwrap_or("");
            assert!(
                description.starts_with(TEXT),
                "{id} description does not start with the exclusions text"
            );
        }
    }
    let expected: BTreeSet<String> = EXPECTED.iter().map(|id| (*id).to_owned()).collect();
    assert_eq!(found, expected);
}

fn assert_nullability(schemas: &serde_json::Map<String, Value>) {
    const D65: &[(&str, &[&str])] = &[
        ("Message", &["finish_reason", "name"]),
        ("ToolCallPart", &["id"]),
        ("ToolCallResponsePart", &["id"]),
        ("UriPart", &["mime_type", "modality"]),
        ("BlobPart", &["mime_type", "modality"]),
        ("TextPart", &[]),
        ("ReasoningPart", &[]),
        ("UnknownPart", &[]),
    ];
    let d65_names: HashSet<&str> = D65.iter().map(|(name, _)| *name).collect();
    let mut required_nullable = BTreeSet::new();
    for (name, schema) in schemas {
        if let Some((_, fields)) = D65.iter().find(|(component, _)| *component == name) {
            assert_eq!(
                nullable_property_names(schema),
                fields.iter().copied().collect(),
                "{name}"
            );
        }
        collect_required_nullable(
            name,
            schema,
            d65_names.contains(name.as_str()),
            &mut required_nullable,
        );
    }
    let pages = [
        "TracePage",
        "SpanPage",
        "SessionPage",
        "EndUserPage",
        "ScorePage",
        "ProjectPage",
        "SavedViewPage",
    ];
    let expected: BTreeSet<String> = pages
        .iter()
        .map(|page| format!("{page}.next_cursor"))
        .collect();
    assert_eq!(required_nullable, expected);
}

fn nullable_property_names(schema: &Value) -> BTreeSet<&str> {
    schema["properties"]
        .as_object()
        .map(|properties| {
            properties
                .iter()
                .filter(|(_, property)| schema_is_nullable(property))
                .map(|(name, _)| name.as_str())
                .collect()
        })
        .unwrap_or_default()
}

fn collect_required_nullable(
    component: &str,
    schema: &Value,
    d65: bool,
    required_nullable: &mut BTreeSet<String>,
) {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required = schema["required"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    for (name, property) in properties {
        if schema_is_nullable(property) {
            if required.contains(name.as_str()) {
                required_nullable.insert(format!("{component}.{name}"));
            } else {
                assert!(
                    d65,
                    "{component}.{name} is nullable but not required and not a D65 field"
                );
            }
        }
        collect_required_nullable(component, property, d65, required_nullable);
        if let Some(items) = property.get("items") {
            collect_required_nullable(component, items, d65, required_nullable);
        }
    }
}

fn schema_is_nullable(schema: &Value) -> bool {
    type_includes_null(schema)
        || composite_includes_null(schema, "oneOf")
        || composite_includes_null(schema, "anyOf")
}

fn type_includes_null(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(ty)) => ty == "null",
        Some(Value::Array(types)) => types.iter().any(|ty| ty.as_str() == Some("null")),
        _ => false,
    }
}

fn composite_includes_null(schema: &Value, key: &str) -> bool {
    schema
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(type_includes_null))
}

fn is_snake(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|char| char.is_ascii_lowercase() || char.is_ascii_digit() || char == '_')
}

fn is_pascal(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && chars.all(|char| char.is_ascii_alphanumeric())
        && !name.contains('_')
        && !name.contains('<')
        && !name.contains('>')
}
