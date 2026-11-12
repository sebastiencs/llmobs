//! Trace transcript rendered for a judge prompt (D163).
//!
//! One line per span, indented by depth. A cycle has no parent outside itself,
//! so spans that the root walk never reaches are started in start-time order.
//! The visited set stops the cycle and skips a repeated span id.

use std::collections::{HashMap, HashSet};

use llmobs_core::ids::SpanId;
use llmobs_core::span::Span;

use super::decode_payload;
use crate::text::render_value;

const PAYLOAD_CHARS: usize = 2_000;
const TRANSCRIPT_CHARS: usize = 60_000;

/// Renders `spans` as an indented `kind name: input → output` tree.
pub(crate) fn render(spans: &[Span]) -> String {
    let lines = walk(spans)
        .into_iter()
        .map(|(index, depth)| render_line(&spans[index], depth))
        .collect::<Vec<_>>();
    join_capped(&lines)
}

fn walk(spans: &[Span]) -> Vec<(usize, usize)> {
    let ids = spans
        .iter()
        .map(|span| span.span_id)
        .collect::<HashSet<_>>();
    let mut children: HashMap<SpanId, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        match span.parent_span_id {
            Some(parent) if ids.contains(&parent) => {
                children.entry(parent).or_default().push(index);
            }
            _ => roots.push(index),
        }
    }
    let order = |left: usize, right: usize| {
        let earlier = &spans[left];
        let later = &spans[right];
        earlier
            .start_time
            .cmp(&later.start_time)
            .then_with(|| earlier.span_id.cmp(&later.span_id))
            .then(left.cmp(&right))
    };
    roots.sort_by(|left, right| order(*left, *right));
    for kids in children.values_mut() {
        kids.sort_by(|left, right| order(*left, *right));
    }

    let mut visited = HashSet::new();
    let mut ordered = Vec::new();
    visit(spans, &children, &roots, &mut visited, &mut ordered);
    let mut leftover = spans
        .iter()
        .enumerate()
        .filter(|(_, span)| !visited.contains(&span.span_id))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    leftover.sort_by(|left, right| order(*left, *right));
    visit(spans, &children, &leftover, &mut visited, &mut ordered);
    ordered
}

fn visit(
    spans: &[Span],
    children: &HashMap<SpanId, Vec<usize>>,
    starts: &[usize],
    visited: &mut HashSet<SpanId>,
    ordered: &mut Vec<(usize, usize)>,
) {
    let mut stack = starts
        .iter()
        .rev()
        .map(|index| (*index, 0_usize))
        .collect::<Vec<_>>();
    while let Some((index, depth)) = stack.pop() {
        let id = spans[index].span_id;
        if !visited.insert(id) {
            continue;
        }
        ordered.push((index, depth));
        if let Some(kids) = children.get(&id) {
            for child in kids.iter().rev() {
                stack.push((*child, depth + 1));
            }
        }
    }
}

fn render_line(span: &Span, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let input = render_payload(span.input.as_deref(), span.input_format);
    let output = render_payload(span.output.as_deref(), span.output_format);
    format!(
        "{indent}{} {}: {input} → {output}",
        span.span_kind.as_str(),
        span.name
    )
}

fn render_payload(
    payload: Option<&str>,
    format: Option<llmobs_core::enums::PayloadFormat>,
) -> String {
    let Some(value) = decode_payload(payload, format) else {
        return "-".to_owned();
    };
    cut_chars(&collapse_whitespace(&render_value(&value)), PAYLOAD_CHARS)
}

/// Collapses every Unicode whitespace run, including a run at either end, to
/// one ASCII space.
fn collapse_whitespace(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut pending = false;
    for character in text.chars() {
        if character.is_whitespace() {
            pending = true;
        } else {
            if pending {
                collapsed.push(' ');
                pending = false;
            }
            collapsed.push(character);
        }
    }
    if pending {
        collapsed.push(' ');
    }
    collapsed
}

fn cut_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut cut = text.chars().take(max_chars).collect::<String>();
    cut.push('…');
    cut
}

fn join_capped(lines: &[String]) -> String {
    let mut transcript = String::new();
    let mut used = 0_usize;
    let mut written = 0_usize;
    for line in lines {
        let separator = usize::from(written > 0);
        let needed = separator + line.chars().count();
        if used + needed > TRANSCRIPT_CHARS {
            break;
        }
        if written > 0 {
            transcript.push('\n');
        }
        transcript.push_str(line);
        used += needed;
        written += 1;
    }
    let remaining = lines.len() - written;
    if remaining > 0 {
        if written > 0 {
            transcript.push('\n');
        }
        transcript.push_str(&format!("[… {remaining} more spans]"));
    }
    transcript
}

#[cfg(test)]
mod tests {
    use llmobs_core::enums::{PayloadFormat, SpanKind, SpanStatus};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::span::Span;

    use super::render;

    fn span_id(byte: u8) -> SpanId {
        SpanId::from_bytes([byte, 0, 0, 0, 0, 0, 0, 1]).unwrap()
    }

    fn bare(id: u8, parent: Option<u8>, name: &str, kind: SpanKind, start: i64) -> Span {
        Span {
            project_id: "018f6b4e-7c1a-7c1a-8c1a-000000000001"
                .parse::<ProjectId>()
                .unwrap(),
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736"
                .parse::<TraceId>()
                .unwrap(),
            span_id: span_id(id),
            parent_span_id: parent.map(span_id),
            name: name.to_owned(),
            span_kind: kind,
            start_time: start,
            end_time: start,
            status: SpanStatus::Unset,
            status_message: None,
            service_name: None,
            session_id: None,
            user_id: None,
            environment: None,
            release: None,
            tags: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
            provider: None,
            request_model: None,
            response_model: None,
            operation: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_input: None,
            cost_output: None,
            cost_total: None,
            cost_source: None,
            ttft_ns: None,
            request_params: None,
            input: None,
            input_format: None,
            output: None,
            output_format: None,
            input_preview: None,
            output_preview: None,
            system_instructions: None,
            tool_definitions: None,
            attributes: std::collections::BTreeMap::new(),
            resource_attributes: std::collections::BTreeMap::new(),
            scope_name: None,
            scope_version: None,
            events: Vec::new(),
            links: Vec::new(),
            media_refs: Vec::new(),
            truncated: Vec::new(),
            conventions: Vec::new(),
            ingest_time: 0,
            schema_version: 1,
        }
    }

    fn text_payload(mut span: Span, input: Option<&str>, output: Option<&str>) -> Span {
        if let Some(input) = input {
            span.input = Some(input.to_owned());
            span.input_format = Some(PayloadFormat::Text);
        }
        if let Some(output) = output {
            span.output = Some(output.to_owned());
            span.output_format = Some(PayloadFormat::Text);
        }
        span
    }

    #[test]
    fn orders_a_tree_and_collapses_payloads() {
        let root = text_payload(
            bare(1, None, "root", SpanKind::Agent, 0),
            Some("hello\n\nworld"),
            None,
        );
        let late = bare(3, Some(1), "late", SpanKind::Chain, 30);
        let early = bare(2, Some(1), "early", SpanKind::Llm, 10);
        let tie_high = bare(6, Some(2), "high", SpanKind::Tool, 15);
        let tie_low = bare(4, Some(2), "low", SpanKind::Tool, 15);
        let orphan = text_payload(
            bare(7, Some(9), "orphan", SpanKind::Span, 5),
            Some(" \u{00a0}\n"),
            Some("null"),
        );
        let rendered = render(&[tie_high, late, orphan, root, tie_low, early]);
        assert_eq!(
            rendered,
            "\
agent root: hello world → -
  llm early: - → -
    tool low: - → -
    tool high: - → -
  chain late: - → -
span orphan:   → null"
        );
    }

    #[test]
    fn cuts_a_long_payload_on_a_char_boundary() {
        let long = "a".repeat(2_001);
        let span = text_payload(bare(1, None, "n", SpanKind::Span, 0), Some(&long), None);
        let rendered = render(&[span]);
        let payload = "a".repeat(2_000) + "…";
        assert_eq!(rendered, format!("span n: {payload} → -"));

        let exact = text_payload(
            bare(1, None, "n", SpanKind::Span, 0),
            Some(&"b".repeat(2_000)),
            None,
        );
        let rendered = render(&[exact]);
        assert!(rendered.contains(&"b".repeat(2_000)));
        assert!(!rendered.contains('…'));

        let mut chars = "a".repeat(1_999);
        chars.push('é');
        chars.push('é');
        let span = text_payload(bare(1, None, "n", SpanKind::Span, 0), Some(&chars), None);
        let rendered = render(&[span]);
        let input = rendered
            .strip_prefix("span n: ")
            .unwrap()
            .strip_suffix(" → -")
            .unwrap();
        assert_eq!(input.chars().count(), 2_001);
        assert_eq!(input.chars().nth(1_999), Some('é'));
        assert!(input.ends_with('…'));
        assert_eq!(
            input.chars().filter(|character| *character == 'é').count(),
            1
        );
    }

    #[test]
    fn caps_the_transcript_and_counts_the_spans_left_out() {
        let one = text_payload(
            bare(1, None, &"n".repeat(88), SpanKind::Span, 0),
            None,
            None,
        );
        let line = render(std::slice::from_ref(&one));
        assert_eq!(line.chars().count(), 100);
        assert!(!line.contains('\n'));

        let mut used = 0_usize;
        let mut written = 0_usize;
        loop {
            let separator = usize::from(written > 0);
            if used + separator + 100 > 60_000 {
                break;
            }
            used += separator + 100;
            written += 1;
        }
        let extra = 4_usize;
        let spans = (0..written + extra)
            .map(|index| {
                let mut row = text_payload(
                    bare(
                        1,
                        None,
                        &"n".repeat(88),
                        SpanKind::Span,
                        i64::try_from(index).unwrap(),
                    ),
                    None,
                    None,
                );
                let id = u64::try_from(index).unwrap() + 1;
                row.span_id = SpanId::from_bytes(id.to_be_bytes()).unwrap();
                row
            })
            .collect::<Vec<_>>();
        let rendered = render(&spans);
        let mut lines = rendered.split('\n');
        let body = lines.by_ref().take(written).collect::<Vec<_>>();
        assert_eq!(body.len(), written);
        assert!(body.iter().all(|line| line.chars().count() == 100));
        let summary = format!("[… {extra} more spans]");
        assert_eq!(lines.next(), Some(summary.as_str()));
        assert!(lines.next().is_none());

        let huge = bare(1, None, &"n".repeat(60_000), SpanKind::Span, 0);
        let later = bare(2, None, "later", SpanKind::Span, 1);
        assert_eq!(render(&[huge, later]), "[… 2 more spans]");
    }

    #[test]
    fn renders_a_cycle_once_and_skips_a_duplicate_id() {
        let earlier = bare(1, Some(2), "a", SpanKind::Span, 0);
        let later = bare(2, Some(1), "b", SpanKind::Chain, 1);
        assert_eq!(
            render(&[later, earlier]),
            "\
span a: - → -
  chain b: - → -"
        );

        let first = text_payload(
            bare(5, None, "first", SpanKind::Span, 0),
            Some("kept"),
            None,
        );
        let duplicate = text_payload(
            bare(5, None, "second", SpanKind::Span, 0),
            Some("dropped"),
            None,
        );
        let rendered = render(&[first, duplicate]);
        assert_eq!(rendered, "span first: kept → -");
        assert!(!rendered.contains("second"));
        assert!(!rendered.contains("dropped"));
    }

    #[test]
    fn decodes_payload_formats() {
        let mut span = bare(1, None, "row", SpanKind::Llm, 0);
        span.input =
            Some(r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#.to_owned());
        span.input_format = Some(PayloadFormat::Messages);
        span.output = Some("{not json".to_owned());
        span.output_format = Some(PayloadFormat::Json);
        assert_eq!(render(&[span]), "llm row: user: hi → {not json");

        let mut nulls = bare(2, None, "row", SpanKind::Span, 0);
        nulls.input = Some("null".to_owned());
        nulls.input_format = Some(PayloadFormat::Json);
        assert_eq!(render(&[nulls]), "span row:  → -");

        assert_eq!(render(&[]), "");
    }

    #[test]
    fn snapshots_an_agent_trace() {
        let agent = text_payload(
            bare(1, None, "research", SpanKind::Agent, 0),
            Some("What is the weather in Paris?"),
            Some("It is 18°C and clear."),
        );
        let mut decide = bare(2, Some(1), "decide", SpanKind::Llm, 1);
        decide.input = Some(
            r#"[{"role":"user","parts":[{"type":"text","content":"What is the weather in Paris?"}]}]"#
                .to_owned(),
        );
        decide.input_format = Some(PayloadFormat::Messages);
        decide.output = Some(
            r#"[{"role":"assistant","parts":[{"type":"tool_call","name":"get_weather","arguments":{"city":"Paris"}}]}]"#
                .to_owned(),
        );
        decide.output_format = Some(PayloadFormat::Messages);
        let mut tool = bare(3, Some(2), "get_weather", SpanKind::Tool, 2);
        tool.input = Some(r#"{"city":"Paris"}"#.to_owned());
        tool.input_format = Some(PayloadFormat::Json);
        tool.output = Some("18°C and clear".to_owned());
        tool.output_format = Some(PayloadFormat::Text);
        let mut answer = bare(4, Some(1), "answer", SpanKind::Llm, 3);
        answer.input = Some(
            r#"[{"role":"user","parts":[{"type":"text","content":"What is the weather in Paris?"}]},{"role":"tool","parts":[{"type":"tool_call_response","response":"18°C and clear"}]}]"#
                .to_owned(),
        );
        answer.input_format = Some(PayloadFormat::Messages);
        answer.output = Some(
            r#"[{"role":"assistant","parts":[{"type":"text","content":"It is 18°C and clear."}]}]"#
                .to_owned(),
        );
        answer.output_format = Some(PayloadFormat::Messages);

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("agent_trace", render(&[answer, tool, decide, agent]));
        });
    }
}
