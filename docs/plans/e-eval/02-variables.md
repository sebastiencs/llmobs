# E · Variables and templates (E5–E8)

See [README.md](README.md). These sub-phases turn trace data or a dataset item into named variables (D162), render the trace transcript, and render a judge prompt (D163). Everything here is a plain function; nothing is `async`.

## E5 — Mapping expressions

**Commit:** `eval: parse variable mapping expressions`

**Edit only:** `crates/eval/`, the root `Cargo.toml` and `Cargo.lock` (`serde_json_path = "0.7.2"` in the workspace and the crate).

**`mapping/mod.rs`.**

```rust
pub struct Expr { source: Source, path: Option<serde_json_path::JsonPath>, optional: bool, text: String }
pub(crate) enum Source {
    Trace(TraceField),                  // trace.input | trace.output | trace.metadata.<k> | trace.transcript
    Span(SpanSelector, SpanField),      // span.… | span[name="…"].… | span[kind="…"].…
    Expected,                           // expected
    Item(ItemField),                    // item.input | item.metadata.<k>
}
pub(crate) enum TraceField { Input, Output, Metadata(String), Transcript }
pub(crate) enum SpanSelector { Target, Name(String), Kind(SpanKind) }
pub(crate) enum SpanField { Input, Output, Metadata(String), Attributes(String) }
pub(crate) enum ItemField { Input, Metadata(String) }
pub struct VariableMapping(BTreeMap<String, Expr>);
```

- `Expr::parse(text: &str) -> Result<Expr, MappingError>`. A hand-written parser over the grammar of D162 (no parser crate):
  - the source prefix first;
  - `metadata.<key>` and `attributes.<key>`: the key runs to the first `$`, or to a final `?`, or to the end. It must be non-empty and may contain dots (`llmobs.eval.rule_id` is a valid key);
  - `span[name="…"]`: a double-quoted string with `\"` and `\\` escapes; `span[kind="…"]`: a `SpanKind` stored value (`llm`, `retriever`, …; use the enum's `FromStr`);
  - an optional JSONPath suffix starting at `$`, parsed with `JsonPath::parse` (RFC 9535);
  - an optional final `?`.
  
  Rejected, each with its own `MappingError::Syntax { expr_offset, reason }` (the offset into the expression and a fixed reason string): an unknown source; `trace.transcript` with a JSONPath suffix (it is text); an empty key; an unterminated string; an unknown kind; trailing characters; more than 1 KiB.
- `Display` prints `text`; `Expr` stores the original text for error messages (expressions are configuration, not payload, so they may appear in errors).
- `VariableMapping::from_json(&Value) -> Result<Self, MappingError>`: a JSON object `{var: "expr"}`; variable names match `^[A-Za-z_][A-Za-z0-9_]{0,63}$`; at most 32 entries; errors name the variable.
- `VariableMapping::needs_item(&self) -> bool` (any `Expected` or `Item` source) and `needs_target_span(&self) -> bool` (any `SpanSelector::Target`). The server uses them to refuse an online rule that maps `expected`, or a trace rule that maps `span.*`.

**Tests:** one table-driven test over every valid form of D162 (including a key with dots, an escaped quote in a name, every `SpanKind`, a JSONPath with an index and a filter, the `?` with and without a path) checking the parsed structure; one table of invalid expressions checking the reason; the mapping limits; `needs_item` and `needs_target_span`.

**Checks:** `just check`.

## E6 — Transcript and value rendering

**Commit:** `eval: render values and trace transcripts`

**Edit only:** `crates/eval/`.

**`mapping/transcript.rs`.** `pub(crate) fn render(spans: &[Span]) -> String` per D163:
- Build the tree from `parent_span_id`; spans whose parent is missing are roots. Roots and children are ordered by `start_time`, then `span_id`. Track visited ids so malformed data (a cycle, a duplicate id) never loops or repeats a span.
- One line per span: two spaces of indent per depth, then `{kind} {name}: {input} → {output}`, where `kind` is the stored value (`llm`, `tool`, …).
- Payloads are decoded as in E7 (by `PayloadFormat`; put the decoding helper in `mapping/mod.rs` now so E7 reuses it). A payload renders with `text::render_value` (below). Whitespace runs (newlines included) collapse to one space. Each payload is cut to 2 000 characters with `…` appended when cut. An absent payload renders as `-`.
- The total is capped at 60 000 characters: stop before the line that would cross it, and append `[… n more spans]` with the count of spans not written. Characters are Unicode scalar values (`chars()`), and cutting never splits one.

**`text.rs`** gains `pub(crate) fn render_value(&Value) -> String` (D163), which E8's formatter reuses: string → as is; messages → one line per message, `role: ` followed by its parts: text as is, image `[image]`, audio `[audio]`, file `[file]`, tool call `tool_call name(arguments as compact JSON)`, tool result `tool_result: <content rendered>`; check `llmobs_core::message::Part` for the exact variants and map each one; null → `""`; any other JSON → compact JSON.

**Tests:** a three-level tree with siblings out of order; long payload cut with `…`; total cap with the `[… n more spans]` line and its count; multi-byte characters at the cut; a cycle (`a → b → a`) renders each span once; `render_value` for every `Part` variant; an `insta` snapshot of one realistic agent trace (agent → llm → tool → llm).

**Checks:** `just check`.

## E7 — Resolution over a trace

**Commit:** `eval: resolve mapped variables from trace data`

**Edit only:** `crates/eval/`.

**`mapping/resolve.rs`.**

```rust
pub struct TraceData<'a> { spans: &'a [Span], target_span: Option<SpanId> }
pub struct ItemData { pub input: Value, pub expected: Option<Value>, pub metadata: serde_json::Map<String, Value> }
pub struct Variables(BTreeMap<String, Value>);
pub fn resolve(mapping: &VariableMapping, trace: Option<&TraceData>, item: Option<&ItemData>) -> Result<Variables, MappingError>
```

- `TraceData::new(spans, target_span)`. The spans are one trace's spans with heavy columns loaded, in any order. `Variables::from_map(BTreeMap<String, Value>)` is public for the evaluator test endpoint, which passes raw variables.
- **Root** (D162): the spans with no `parent_span_id`, or whose parent is not in the slice; the earliest `start_time` wins, then the lowest `span_id` bytes to break ties. `trace.input`/`trace.output` read the root; if there is no span without a parent at all, they read the first `llm` span by start time.
- **Target span:** `span.*` reads the span whose id is `target_span`; `MappingError::NoTargetSpan` if `target_span` is `None` or not found.
- **Selectors:** first by `start_time` (ties: `span_id`) whose `name` equals the string exactly / whose `span_kind` matches.
- **Payload decoding** (the helper from E6): `input`/`output` by their `PayloadFormat`: `messages` and `json` → `serde_json::from_str` (if that fails, the raw string — storage guarantees valid JSON, so this is defensive and documented as such), `text` → a JSON string, absent → absent.
- **Metadata and attributes:** look the key up in `metadata` / `attributes`; the stored value is JSON-encoded, so decode it, falling back to the raw string. `trace.metadata.<k>` reads the root's metadata.
- **Item:** `item.input` → `ItemData.input`; `item.metadata.<k>`; `expected` → `ItemData.expected` (absent when `None`). `MappingError::NoItem` when an item source is used without an item; `NoTrace` when a trace source is used without trace data.
- **`trace.transcript`:** a JSON string from E6's `transcript::render` over all the spans.
- **JSONPath:** applied to the resolved value. Zero matches → absent, one → that value, several → a JSON array of them (in match order).
- **Absent:** a required variable → `MappingError::MissingVariable { name, expr }`; an optional one → `Value::Null`.

**Tests** (build `Span` literals with a small helper in `#[cfg(test)]`): root choice (one root; two roots by time; no root at all → first `llm` span; orphan parent); each source; `span[name=…]` picks the earliest; `span[kind=…]`; target missing; payload formats; metadata keys with dots; JSON-encoded attribute decoding; JSONPath with zero, one and several matches; optional absent → null; required absent → error naming the variable and expression; item sources with and without an item; `MappingError`'s `Display` never contains a planted payload value.

**Checks:** `just check`.

## E8 — Templating

**Commit:** `eval: render judge prompts with minijinja`

**Edit only:** `crates/eval/`, the root `Cargo.toml` and `Cargo.lock` (`minijinja` per README § Dependencies).

**`template.rs`.**

```rust
pub struct PromptTemplate { env: minijinja::Environment<'static> }   // holds one template named "prompt"
impl PromptTemplate {
    pub fn compile(source: &str) -> Result<Self, TemplateError>;
    pub fn variables(&self) -> BTreeSet<String>;          // undeclared top-level names
    pub fn render(&self, vars: &Variables) -> Result<String, TemplateError>;
}
```

- `Environment` setup: `UndefinedBehavior::Strict`, no auto-escape (`set_auto_escape_callback` returning `AutoEscape::None`), `set_fuel(Some(100_000))`, and a custom formatter (`set_formatter`) that writes every value with `text::render_value` (convert the minijinja value back to `serde_json::Value` through `serde`; undefined and none write nothing). The template source is owned (`add_template_owned`).
- `compile`: rejects sources over 64 KiB. A syntax error becomes `TemplateError::Syntax { line, kind }` from the minijinja error's line and kind. Never `err.to_string()`: with some features it quotes template source, and later it may quote values.
- `variables`: `Template::undeclared_variables(false)`.
- `render`: first, every name in `variables()` must be in `vars`, else `TemplateError::MissingVariable { name }` (the first missing name, sorted). Then render with each variable passed as `minijinja::Value::from_serialize`. A strict-undefined failure inside (for example `{{ output.answer }}` when `answer` is absent) → `TemplateError::Undefined { line }`. Fuel exhaustion → `TemplateError::TooComplex`. A result over 2 MiB → `TemplateError::PromptTooLarge { bytes }`.
- E2's `Evaluator::validate` now compiles the judge prompt.

**Tests:** plain substitution; a messages variable renders as role lines; a JSON object renders as compact JSON and `{{ output.answer }}` indexes into it; null renders empty; a missing variable names it; a missing attribute gives `Undefined` with the line; `{% for %}` over a list works; an infinite-ish loop (`{% for i in range(100000) %}{% for j in range(100000) %}…`) hits `TooComplex`; HTML characters are not escaped; a syntax error gives the line; no `TemplateError` `Display` contains a planted variable value; `variables()` for a template with a loop variable and a filter.

**Checks:** `just check`.
