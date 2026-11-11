//! Remove machine-specific values from one captured export.
//!
//! Resource attributes are dropped or pinned first. Span, event, and link
//! addresses are rewritten next. String replacement then runs longest-needle
//! first. The leak check is last and names paths only.

use std::path::{Path, PathBuf};

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};

use crate::error::CaptureError;
use crate::scenario::Provider;

const CAPTURE_HOST: &str = "capture-host";
const MOCK_KEY: &str = "capture-mock-key";

/// What a capture must rewrite or refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubContext {
    /// Provider whose real host replaces the mock address.
    pub provider: Provider,
    /// Loopback port of the mock provider.
    pub mock_port: u16,
    /// Loopback port of the OTLP receiver.
    pub receiver_port: u16,
    /// Home directory of the account that ran the capture.
    pub home: PathBuf,
    /// Repository root that contained the scenario.
    pub repo_root: PathBuf,
    /// Machine hostname. Replaced only when it is at least 3 characters.
    pub hostname: String,
}

struct Replacement {
    needle: String,
    replacement: String,
}

/// Rewrites `request` in place. A leak leaves the earlier rewrites applied and
/// returns the paths that still carry a machine-specific value.
pub fn scrub(
    request: &mut ExportTraceServiceRequest,
    ctx: &ScrubContext,
) -> Result<(), CaptureError> {
    for resource_spans in &mut request.resource_spans {
        if let Some(resource) = resource_spans.resource.as_mut() {
            scrub_resource(&mut resource.attributes);
        }
    }
    rewrite_endpoints(request, ctx);
    let replacements = replacements(ctx);
    replace_strings(request, &replacements);
    let locations = leaks(request, ctx);
    if locations.is_empty() {
        Ok(())
    } else {
        Err(CaptureError::Leak { locations })
    }
}

fn scrub_resource(attributes: &mut Vec<KeyValue>) {
    attributes.retain(|attribute| !dropped_resource_key(&attribute.key));
    for attribute in attributes {
        if attribute.key == "host.name" {
            attribute.value = Some(string_value(CAPTURE_HOST));
        }
    }
}

fn dropped_resource_key(key: &str) -> bool {
    const EXACT: &[&str] = &[
        "host.id",
        "host.arch",
        "process.pid",
        "process.parent_pid",
        "process.owner",
        "process.command",
        "process.command_line",
        "process.command_args",
        "process.executable.name",
        "process.executable.path",
        "process.working_directory",
        "process.runtime.description",
        "os.description",
        "os.version",
        "os.build_id",
        "container.id",
        "service.instance.id",
    ];
    EXACT.contains(&key) || key.starts_with("host.image.")
}

fn rewrite_endpoints(request: &mut ExportTraceServiceRequest, ctx: &ScrubContext) {
    let host = ctx.provider.host();
    for resource_spans in &mut request.resource_spans {
        for scope_spans in &mut resource_spans.scope_spans {
            for span in &mut scope_spans.spans {
                rewrite_attribute_endpoints(&mut span.attributes, host, ctx.mock_port);
                for event in &mut span.events {
                    rewrite_attribute_endpoints(&mut event.attributes, host, ctx.mock_port);
                }
                for link in &mut span.links {
                    rewrite_attribute_endpoints(&mut link.attributes, host, ctx.mock_port);
                }
            }
        }
    }
}

fn rewrite_attribute_endpoints(attributes: &mut [KeyValue], host: &str, mock_port: u16) {
    for attribute in attributes {
        let Some(value) = attribute.value.as_mut() else {
            continue;
        };
        match attribute.key.as_str() {
            "server.address" => rewrite_address(value, host),
            "server.port" => rewrite_port(value, mock_port),
            _ => {}
        }
        rewrite_nested_endpoints(value, host, mock_port);
    }
}

fn rewrite_nested_endpoints(value: &mut AnyValue, host: &str, mock_port: u16) {
    match &mut value.value {
        Some(Value::ArrayValue(array)) => {
            for item in &mut array.values {
                rewrite_nested_endpoints(item, host, mock_port);
            }
        }
        Some(Value::KvlistValue(list)) => {
            rewrite_attribute_endpoints(&mut list.values, host, mock_port);
        }
        _ => {}
    }
}

fn rewrite_address(value: &mut AnyValue, host: &str) {
    if let Some(Value::StringValue(text)) = &mut value.value
        && (text == "127.0.0.1" || text == "localhost")
    {
        *text = host.to_owned();
    }
}

fn rewrite_port(value: &mut AnyValue, mock_port: u16) {
    let mock = mock_port.to_string();
    match &mut value.value {
        Some(Value::IntValue(port)) if *port == i64::from(mock_port) => *port = 443,
        Some(Value::StringValue(text)) if text == &mock => *text = "443".to_owned(),
        _ => {}
    }
}

fn replacements(ctx: &ScrubContext) -> Vec<Replacement> {
    let host = ctx.provider.host();
    let mut patterns = vec![
        Replacement {
            needle: format!("http://127.0.0.1:{}", ctx.mock_port),
            replacement: format!("https://{host}"),
        },
        Replacement {
            needle: format!("http://localhost:{}", ctx.mock_port),
            replacement: format!("https://{host}"),
        },
    ];
    if let Some(root) = utf8_path(&ctx.repo_root) {
        patterns.push(Replacement {
            needle: root,
            replacement: "/capture".to_owned(),
        });
    }
    if let Some(home) = utf8_path(&ctx.home) {
        patterns.push(Replacement {
            needle: home,
            replacement: "/home/capture".to_owned(),
        });
    }
    if ctx.hostname.chars().count() >= 3 {
        patterns.push(Replacement {
            needle: ctx.hostname.clone(),
            replacement: CAPTURE_HOST.to_owned(),
        });
    }
    patterns.retain(|pattern| !pattern.needle.is_empty());
    patterns.sort_by(|left, right| {
        right
            .needle
            .len()
            .cmp(&left.needle.len())
            .then_with(|| left.needle.cmp(&right.needle))
    });
    patterns
}

fn utf8_path(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

fn replace_strings(request: &mut ExportTraceServiceRequest, patterns: &[Replacement]) {
    for resource_spans in &mut request.resource_spans {
        if let Some(resource) = resource_spans.resource.as_mut() {
            replace_in_attributes(&mut resource.attributes, patterns);
        }
        for scope_spans in &mut resource_spans.scope_spans {
            if let Some(scope) = scope_spans.scope.as_mut() {
                replace_in_attributes(&mut scope.attributes, patterns);
            }
            for span in &mut scope_spans.spans {
                replace_text(&mut span.name, patterns);
                replace_in_attributes(&mut span.attributes, patterns);
                for event in &mut span.events {
                    replace_text(&mut event.name, patterns);
                    replace_in_attributes(&mut event.attributes, patterns);
                }
                for link in &mut span.links {
                    replace_in_attributes(&mut link.attributes, patterns);
                }
            }
        }
    }
}

fn replace_in_attributes(attributes: &mut [KeyValue], patterns: &[Replacement]) {
    for attribute in attributes {
        if let Some(value) = attribute.value.as_mut() {
            replace_in_value(value, patterns);
        }
    }
}

fn replace_in_value(value: &mut AnyValue, patterns: &[Replacement]) {
    match &mut value.value {
        Some(Value::StringValue(text)) => replace_text(text, patterns),
        Some(Value::ArrayValue(array)) => {
            for item in &mut array.values {
                replace_in_value(item, patterns);
            }
        }
        Some(Value::KvlistValue(list)) => replace_in_attributes(&mut list.values, patterns),
        _ => {}
    }
}

fn replace_text(text: &mut String, patterns: &[Replacement]) {
    if patterns.is_empty() {
        return;
    }
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let matched = patterns
            .iter()
            .find(|pattern| text.as_bytes()[index..].starts_with(pattern.needle.as_bytes()));
        if let Some(pattern) = matched {
            out.push_str(&pattern.replacement);
            index += pattern.needle.len();
            continue;
        }
        let Some(ch) = text[index..].chars().next() else {
            break;
        };
        out.push(ch);
        index += ch.len_utf8();
    }
    *text = out;
}

fn leaks(request: &ExportTraceServiceRequest, ctx: &ScrubContext) -> Vec<String> {
    let needles = leak_needles(ctx);
    let mut locations = Vec::new();
    for (resource_index, resource_spans) in request.resource_spans.iter().enumerate() {
        let resource_path = format!("resourceSpans[{resource_index}]");
        note(
            &mut locations,
            &format!("{resource_path}.schemaUrl"),
            &resource_spans.schema_url,
            &needles,
        );
        if let Some(resource) = &resource_spans.resource {
            inspect_attributes(
                &resource.attributes,
                &format!("{resource_path}.resource.attributes"),
                ctx,
                &needles,
                &mut locations,
            );
        }
        for (scope_index, scope_spans) in resource_spans.scope_spans.iter().enumerate() {
            let scope_path = format!("{resource_path}.scopeSpans[{scope_index}]");
            note(
                &mut locations,
                &format!("{scope_path}.schemaUrl"),
                &scope_spans.schema_url,
                &needles,
            );
            if let Some(scope) = &scope_spans.scope {
                note(
                    &mut locations,
                    &format!("{scope_path}.scope.name"),
                    &scope.name,
                    &needles,
                );
                note(
                    &mut locations,
                    &format!("{scope_path}.scope.version"),
                    &scope.version,
                    &needles,
                );
                inspect_attributes(
                    &scope.attributes,
                    &format!("{scope_path}.scope.attributes"),
                    ctx,
                    &needles,
                    &mut locations,
                );
            }
            for (span_index, span) in scope_spans.spans.iter().enumerate() {
                let span_path = format!("{scope_path}.spans[{span_index}]");
                note(
                    &mut locations,
                    &format!("{span_path}.name"),
                    &span.name,
                    &needles,
                );
                note(
                    &mut locations,
                    &format!("{span_path}.traceState"),
                    &span.trace_state,
                    &needles,
                );
                if let Some(status) = &span.status {
                    note(
                        &mut locations,
                        &format!("{span_path}.status.message"),
                        &status.message,
                        &needles,
                    );
                }
                inspect_attributes(
                    &span.attributes,
                    &format!("{span_path}.attributes"),
                    ctx,
                    &needles,
                    &mut locations,
                );
                for (event_index, event) in span.events.iter().enumerate() {
                    let event_path = format!("{span_path}.events[{event_index}]");
                    note(
                        &mut locations,
                        &format!("{event_path}.name"),
                        &event.name,
                        &needles,
                    );
                    inspect_attributes(
                        &event.attributes,
                        &format!("{event_path}.attributes"),
                        ctx,
                        &needles,
                        &mut locations,
                    );
                }
                for (link_index, link) in span.links.iter().enumerate() {
                    let link_path = format!("{span_path}.links[{link_index}]");
                    note(
                        &mut locations,
                        &format!("{link_path}.traceState"),
                        &link.trace_state,
                        &needles,
                    );
                    inspect_attributes(
                        &link.attributes,
                        &format!("{link_path}.attributes"),
                        ctx,
                        &needles,
                        &mut locations,
                    );
                }
            }
        }
    }
    locations
}

fn leak_needles(ctx: &ScrubContext) -> Vec<String> {
    let mut needles = vec![
        "127.0.0.1".to_owned(),
        "localhost".to_owned(),
        "[::1]".to_owned(),
        format!(":{}", ctx.mock_port),
        format!(":{}", ctx.receiver_port),
        MOCK_KEY.to_owned(),
    ];
    if let Some(home) = utf8_path(&ctx.home) {
        needles.push(home);
    }
    if let Some(root) = utf8_path(&ctx.repo_root) {
        needles.push(root);
    }
    // A one- or two-character hostname would match inside ordinary words.
    if ctx.hostname.chars().count() >= 3 {
        needles.push(ctx.hostname.clone());
    }
    needles.retain(|needle| !needle.is_empty());
    needles
}

fn inspect_attributes(
    attributes: &[KeyValue],
    prefix: &str,
    ctx: &ScrubContext,
    needles: &[String],
    locations: &mut Vec<String>,
) {
    for attribute in attributes {
        let path = format!("{prefix}[{}]", quoted(&attribute.key));
        if attribute.key == "server.port"
            && attribute
                .value
                .as_ref()
                .is_some_and(|value| forbidden_port(value, ctx))
        {
            push(locations, path.clone());
        }
        if let Some(value) = &attribute.value {
            inspect_value(value, &path, ctx, needles, locations);
        }
    }
}

fn inspect_value(
    value: &AnyValue,
    path: &str,
    ctx: &ScrubContext,
    needles: &[String],
    locations: &mut Vec<String>,
) {
    match &value.value {
        Some(Value::StringValue(text)) => note(locations, path, text, needles),
        Some(Value::ArrayValue(array)) => {
            for (index, item) in array.values.iter().enumerate() {
                inspect_value(
                    item,
                    &format!("{path}.arrayValue.values[{index}]"),
                    ctx,
                    needles,
                    locations,
                );
            }
        }
        Some(Value::KvlistValue(list)) => inspect_attributes(
            &list.values,
            &format!("{path}.kvlistValue.values"),
            ctx,
            needles,
            locations,
        ),
        _ => {}
    }
}

fn forbidden_port(value: &AnyValue, ctx: &ScrubContext) -> bool {
    let port = match &value.value {
        Some(Value::IntValue(port)) => u16::try_from(*port).ok(),
        Some(Value::StringValue(text)) => text.parse().ok(),
        _ => None,
    };
    port.is_some_and(|port| port == ctx.mock_port || port == ctx.receiver_port)
}

fn note(locations: &mut Vec<String>, path: &str, text: &str, needles: &[String]) {
    if needles.iter().any(|needle| text.contains(needle)) {
        push(locations, path.to_owned());
    }
}

fn push(locations: &mut Vec<String>, path: String) {
    if !locations.iter().any(|existing| existing == &path) {
        locations.push(path);
    }
}

fn quoted(key: &str) -> String {
    match serde_json::to_string(key) {
        Ok(text) => text,
        Err(_) => "\"\"".to_owned(),
    }
}

fn string_value(text: &str) -> AnyValue {
    AnyValue {
        value: Some(Value::StringValue(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_proto::tonic::common::v1::{
        AnyValue, ArrayValue, InstrumentationScope, KeyValue, KeyValueList,
    };

    use crate::error::CaptureError;
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::span::{Event, Link};
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};

    use super::*;

    fn ctx() -> ScrubContext {
        ScrubContext {
            provider: Provider::OpenAi,
            mock_port: 40123,
            receiver_port: 40124,
            home: PathBuf::from("/home/sebastien"),
            repo_root: PathBuf::from("/home/sebastien/travaux/llm-observer"),
            hostname: "devbox".to_owned(),
        }
    }

    fn string_attr(key: &str, text: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(string_value(text)),
            ..KeyValue::default()
        }
    }

    fn int_attr(key: &str, integer: i64) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::IntValue(integer)),
            }),
            ..KeyValue::default()
        }
    }

    fn keys(attributes: &[KeyValue]) -> Vec<&str> {
        attributes
            .iter()
            .map(|attribute| attribute.key.as_str())
            .collect()
    }

    fn text_of(attribute: &KeyValue) -> String {
        match &attribute
            .value
            .as_ref()
            .and_then(|value| value.value.as_ref())
        {
            Some(Value::StringValue(text)) => text.clone(),
            _ => panic!("string attribute"),
        }
    }

    #[test]
    fn resource_attributes_are_dropped_or_pinned_and_the_api_key_goes_with_them() {
        let mut request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![
                        string_attr("service.name", "support-bot"),
                        string_attr("host.id", "machine-1"),
                        string_attr("host.name", "laptop"),
                        string_attr("host.arch", "amd64"),
                        string_attr("host.image.name", "img"),
                        string_attr("host.image", "kept"),
                        string_attr("process.pid", "9"),
                        string_attr("process.parent_pid", "8"),
                        string_attr("process.owner", "seb"),
                        string_attr("process.command", "python"),
                        string_attr("process.command_line", "python capture-mock-key"),
                        string_attr("process.command_args", "main.py"),
                        string_attr("process.executable.name", "python"),
                        string_attr("process.executable.path", "/usr/bin/python"),
                        string_attr("process.working_directory", "/tmp"),
                        string_attr("process.runtime.description", "CPython"),
                        string_attr("process.runtime.name", "cpython"),
                        string_attr("process.runtime.version", "3.13.0"),
                        string_attr("os.description", "Fedora"),
                        string_attr("os.version", "44"),
                        string_attr("os.build_id", "build"),
                        string_attr("os.type", "linux"),
                        string_attr("container.id", "abc"),
                        string_attr("service.instance.id", "inst"),
                        string_attr("telemetry.sdk.language", "python"),
                        string_attr("custom.flag", "yes"),
                    ],
                    ..Resource::default()
                }),
                scope_spans: vec![ScopeSpans {
                    scope: Some(InstrumentationScope {
                        name: "test".to_owned(),
                        ..InstrumentationScope::default()
                    }),
                    spans: vec![Span {
                        name: "chat".to_owned(),
                        ..Span::default()
                    }],
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        };
        scrub(&mut request, &ctx()).unwrap();
        let attributes = &request.resource_spans[0]
            .resource
            .as_ref()
            .unwrap()
            .attributes;
        assert_eq!(
            keys(attributes),
            [
                "service.name",
                "host.name",
                "host.image",
                "process.runtime.name",
                "process.runtime.version",
                "os.type",
                "telemetry.sdk.language",
                "custom.flag",
            ]
        );
        assert_eq!(text_of(&attributes[1]), "capture-host");
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("capture-mock-key"), "{json}");
    }

    #[test]
    fn addresses_ports_and_strings_are_rewritten_longest_first() {
        let mut request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![string_attr("service.namespace", "devbox-prod")],
                    ..Resource::default()
                }),
                scope_spans: vec![ScopeSpans {
                    scope: Some(InstrumentationScope {
                        name: "test".to_owned(),
                        attributes: vec![string_attr(
                            "code.filepath",
                            "/home/sebastien/travaux/llm-observer/src/main.py",
                        )],
                        ..InstrumentationScope::default()
                    }),
                    spans: vec![Span {
                        name: "ask devbox".to_owned(),
                        attributes: vec![
                            string_attr("server.address", "localhost"),
                            int_attr("server.port", 40123),
                            string_attr("url.full", "http://127.0.0.1:40123/v1/chat/completions"),
                            string_attr("cache.dir", "/home/sebastien/.cache"),
                            KeyValue {
                                key: "paths".to_owned(),
                                value: Some(AnyValue {
                                    value: Some(Value::ArrayValue(ArrayValue {
                                        values: vec![string_value(
                                            "http://localhost:40123/v1/models",
                                        )],
                                    })),
                                }),
                                ..KeyValue::default()
                            },
                        ],
                        events: vec![Event {
                            name: "http://127.0.0.1:40123/stream".to_owned(),
                            attributes: vec![string_attr("server.address", "127.0.0.1")],
                            ..Event::default()
                        }],
                        links: vec![Link {
                            attributes: vec![string_attr("server.port", "40123")],
                            ..Link::default()
                        }],
                        ..Span::default()
                    }],
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        };
        scrub(&mut request, &ctx()).unwrap();
        let span = &request.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(span.name, "ask capture-host");
        assert_eq!(text_of(&span.attributes[0]), "api.openai.com");
        assert_eq!(
            span.attributes[1].value.as_ref().unwrap().value,
            Some(Value::IntValue(443))
        );
        assert_eq!(
            text_of(&span.attributes[2]),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(text_of(&span.attributes[3]), "/home/capture/.cache");
        let paths = match &span.attributes[4].value.as_ref().unwrap().value {
            Some(Value::ArrayValue(array)) => match &array.values[0].value {
                Some(Value::StringValue(text)) => text.clone(),
                _ => panic!("array string"),
            },
            _ => panic!("array"),
        };
        assert_eq!(paths, "https://api.openai.com/v1/models");
        assert_eq!(span.events[0].name, "https://api.openai.com/stream");
        assert_eq!(text_of(&span.events[0].attributes[0]), "api.openai.com");
        assert_eq!(text_of(&span.links[0].attributes[0]), "443");
        assert_eq!(
            text_of(
                &request.resource_spans[0].scope_spans[0]
                    .scope
                    .as_ref()
                    .unwrap()
                    .attributes[0]
            ),
            "/capture/src/main.py"
        );
        assert_eq!(
            text_of(
                &request.resource_spans[0]
                    .resource
                    .as_ref()
                    .unwrap()
                    .attributes[0]
            ),
            "capture-host-prod"
        );
    }

    #[test]
    fn a_nested_leak_names_its_path_and_not_its_value() {
        let mut request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: "chat".to_owned(),
                        attributes: vec![KeyValue {
                            key: "bag".to_owned(),
                            value: Some(AnyValue {
                                value: Some(Value::KvlistValue(KeyValueList {
                                    values: vec![string_attr(
                                        "url",
                                        "see SECRET-NESTED at 127.0.0.1",
                                    )],
                                })),
                            }),
                            ..KeyValue::default()
                        }],
                        links: vec![Link {
                            attributes: vec![int_attr("server.port", 40124)],
                            ..Link::default()
                        }],
                        ..Span::default()
                    }],
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        };
        let error = scrub(&mut request, &ctx()).unwrap_err();
        let CaptureError::Leak { locations } = &error else {
            panic!("{error}");
        };
        assert_eq!(
            locations,
            &[
                "resourceSpans[0].scopeSpans[0].spans[0].attributes[\"bag\"].kvlistValue.values[\"url\"]"
                    .to_owned(),
                "resourceSpans[0].scopeSpans[0].spans[0].links[0].attributes[\"server.port\"]"
                    .to_owned(),
            ]
        );
        let text = error.to_string();
        assert!(!text.contains("SECRET-NESTED"), "{text}");
        assert!(!format!("{error:?}").contains("SECRET-NESTED"));
    }

    #[test]
    fn a_schema_url_with_the_repo_path_is_a_leak() {
        let mut request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                schema_url: "/home/sebastien/travaux/llm-observer/SECRET-SCHEMA".to_owned(),
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: "chat".to_owned(),
                        ..Span::default()
                    }],
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        };
        let error = scrub(&mut request, &ctx()).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("resourceSpans[0].schemaUrl"), "{text}");
        assert!(!text.contains("SECRET-SCHEMA"), "{text}");
    }

    #[test]
    fn the_api_key_in_a_kept_string_is_refused() {
        let mut request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: "chat".to_owned(),
                        attributes: vec![string_attr("llm.token", "capture-mock-key SECRET-TAIL")],
                        ..Span::default()
                    }],
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        };
        let error = scrub(&mut request, &ctx()).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("attributes[\"llm.token\"]"), "{text}");
        assert!(!text.contains("SECRET-TAIL"), "{text}");
    }
}
