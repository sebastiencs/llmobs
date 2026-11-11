# UI research: synthesis

Research done 2026-11-11 from public docs, changelogs and community threads only. No source code was read. Per-product findings and source URLs are in [competitors.md](competitors.md), where every claim is tagged [docs], [alt] (from an image caption), [inferred] or [not found].

Target: a dense developer-tool UI, closer to Laminar, Datadog, Linear and Grafana than to an airy one. It uses React, shadcn/ui, TanStack Table and Virtual, CodeMirror and ECharts. The filter language is a SQL `WHERE` with visual chips.

## Trends in the last 18 months

1. **Agent narrative first.** Laminar now opens traces in a Transcript view, with the Tree one click away. Langfuse added a Log View (the whole trace as one scrollable page) and a session view that reads as one conversation. Opik and Phoenix render threads as chats.
2. **Text query languages with chips and autocomplete.** Langfuse v4, Phoenix, LangWatch and Datadog all moved to typed queries. Langfuse keeps a facet sidebar as a second editor over the same state.
3. **Per-row metrics everywhere.** Phoenix tree rows end with latency, tokens and cost. Laminar's tree shows duration on every row and token and cost badges on LLM spans. Score and eval badges sit under rows.
4. **Observations or spans as the main table.** Langfuse v4 made one row per observation the main table, with the root filter on by default. Datadog has a Traces/Spans toggle. Phoenix has separate Spans and Traces tabs.
5. **Saved views as a first-class object.** Laminar Table Views, Langfuse My Views and LangWatch Lenses all store filters, sort, columns and the time range per table, shared in the project.
6. **SQL as the escape hatch.** Laminar has a SQL editor, an "Open in SQL editor" span action and SQL charts. Helicone has HQL. Phoenix exposes SQL over MCP.

## Adopt / avoid

### Trace view layout
- **Adopt:** a split pane. The tree (with optional timeline) goes on the left and span detail on the right, as in Laminar, Phoenix and Opik.
  - The splitter is resizable and persisted.
  - The detail pane can also be docked at the bottom, which is Datadog's height-adjustable section.
- **Adopt:** the same trace component in two hosts.
  - A **peek drawer** over the list: J/K for the next and previous row, Esc to close, as in Langfuse.
  - A **full page** with a permalink: Datadog's "Open Full Page".
  - Langfuse made its timeline adapt to trace page, peek and phone. Build ours to adapt to its container from day one.
- **Adopt:** a slim header with name, status, duration, tokens, cost, start time, a copy-ID button, and download (span JSON, OTLP JSON, trace) as Phoenix does.
- **Avoid:** a narrow modal or drawer as the only trace view. Avoid a drawer that loses the list position.
- **Avoid:** pages that refetch or re-mount when switching spans. Datadog-style focus changes only the display.

### Tree vs timeline
- **Adopt:** **one combined tree+bars grid** as the default for engineers.
  - Each virtualized row has an indented name, type icon and status, then a right-aligned latency, tokens and cost, then a duration bar on a shared time axis.
  - This is Datadog's waterfall with Laminar's and Phoenix's per-row metrics. It answers "what called what" and "where did time go" without a toggle.
- **Adopt:** timeline interactions from Langfuse (2026-08) and Laminar.
  - Fit-to-trace by default.
  - Cmd-scroll zooms and drag selects a time range; spans outside the range dim.
  - Double-click zooms to a span.
  - A hover needle shows exact times.
- **Adopt:** a Transcript/conversation mode as the second view for agent traces.
  - LLM turns and tool calls appear as one-line previews, with sub-agents as collapsible cards and wrapper spans hidden.
  - Remember the last-used mode.
- **Adopt:** in-trace search that filters or highlights the tree with prev/next (Phoenix, Datadog). Add a "show errors only" toggle (Datadog).
- **Large traces:**
  - Virtualize rows.
  - Load span payloads lazily, only for the selected span; Phoenix loads hover previews only after the pointer settles.
  - Collapse repeated siblings into **numbered pills** (Datadog Trace Preview).
- **Avoid:** separate, non-equivalent tree and timeline views. Langfuse had to bring theirs to "parity".
- **Avoid:** hard caps that hide spans. Phoenix users hit a 50-row tree limit.
- **Avoid:** fetching all payloads with the tree.

### Filter bar
- **Adopt:** one CodeMirror input holding the SQL `WHERE`, with completed predicates rendered as **chips**.
  - Clicking a chip edits it.
  - A raw-text mode is one keystroke away.
  - This matches Opik's search-with-chips row and Langfuse's chips.
- **Adopt:** schema-aware autocomplete.
  - Field names grouped by category, with operators per type.
  - **Observed values** for low-cardinality fields (Langfuse).
  - Snippet insertion for complex constructs (Phoenix comprehensions).
  - Recent conditions per project (Phoenix).
- **Adopt:** strict validation with inline errors.
  - Underline the bad token.
  - Show "unknown column `lantency`, did you mean `latency`?" (Phoenix).
  - Never silently apply a broken filter, and never silently match nothing. Phoenix shipped exactly that bug and fixed it.
- **Adopt:** a facet sidebar (optional, collapsible) as a second editor over the same `WHERE`.
  - Facet counts must reflect the other active filters (Langfuse), so a click never yields zero rows.
- **Adopt:** **click-to-filter from anywhere**: cell values, attribute rows, score badges and chart bins add a chip (Phoenix annotation chips, Datadog tags).
- **Adopt:** saved views per table holding filter, sort, columns and the time range.
  - An "unsaved changes" dot, with Save, Save as and Discard (Laminar).
  - All of it also lives in the URL (Langfuse, Helicone).
- **Adopt:** carry the filter across pages (Helicone persists filters across requests, sessions and dashboard). Do this at least for the time range and environment.
- **Avoid:** a custom DSL that differs from the SQL people already know. Phoenix now has three filter languages (span, trace, session).
- **Avoid:** an AI-only query box. Natural language can come later, as an optional generator of SQL.

### List density
- **Adopt:** 28–32 px rows, monospace numbers, right-aligned metrics, one-line truncated input/output previews, and a full preview on hover or in the peek.
  - Offer a **row height** setting (Opik) and resizable, reorderable columns persisted per project (Phoenix, Langfuse).
- **Adopt:** a **metric strip above the list**: a small, resizable band of time-series charts (volume, errors, p95 latency, cost).
  - Dragging across it sets the list's time range (Phoenix, Laminar, Datadog).
- **Adopt:** a time picker with typed durations ("15m", "7d"), presets, a calendar, pan/zoom arrows, and a Live play/pause (Phoenix).
- **Adopt:**
  - Virtualize rows and columns (Opik).
  - Keyset "load more" or infinite scroll, not page numbers.
  - A sticky header.
- **Adopt:** default to the trace list, with a Traces/Spans scope switch (Datadog).
  - Langfuse's lesson: engineers often hunt for one failing span, so a span table one click away is required.
- **Adopt:** bulk actions on selected rows: add to dataset (LangWatch), annotate, export, delete.
- **Avoid:** rendering large JSON in cells.
  - LangSmith documents that a metadata column with big payloads makes the table slow.
  - Truncate server-side and cap preview length.
- **Avoid:** LLM "Explain" buttons in cells (Opik); they add noise for a dense UI.

### Span detail tabs
- **Adopt:** few tabs.
  - **Overview:** messages or I/O.
  - **Attributes:** a flattened, searchable key/value table with a copy button and click-to-filter (Phoenix, Datadog Span Info).
  - **Events/Errors:** exception and stack trace.
  - **Scores:** annotations as a sortable table.
  - **Raw:** JSON of the span.
- **Adopt:** an I/O format switch (Pretty / JSON / YAML / Markdown) remembered per user (Opik, LangWatch).
  - Collapsible JSON nodes and an expand-all/collapse-all control (Phoenix, Opik).
- **Adopt:** media values as compact tags that open in place; hide base64 bodies (Langfuse, Opik).
- **Avoid:** Datadog-scale tab sprawl (Infrastructure, Processes, Network…). Avoid putting metadata only in a separate panel.

### Chat rendering
- **Adopt:** role-labelled message blocks with a compact header (role, tokens if known).
  - The system prompt is collapsed by default with a line count.
  - Long messages are collapsed with "show more" (Phoenix sessions).
- **Adopt:** tool calls rendered inline as cards: name, call ID and arguments as collapsible JSON.
  - Each tool result is linked back to its call by ID.
  - **Available tools listed at the top** of the generation, with called ones highlighted (Langfuse 2025-11).
- **Adopt:** images and audio inline as thumbnails; markdown rendering togglable; reasoning blocks collapsed.
- **Adopt:** a whole-session conversation view built from trace inputs and outputs (Langfuse, Opik, Laminar).
  - A turn index sidebar, per-turn trace links, and per-turn tokens and cost.
- **Avoid:** making the pretty view the only view. A raw JSON toggle must always be one click away.

### Dashboard composition
- **Adopt:** a fixed default dashboard (Opik's structure works).
  - A KPI row: traces, error rate, p50/p99 latency, cost, tokens.
  - Then time series: volume by status, latency percentiles, cost by model, tokens by type (input/cached/output/reasoning, as Phoenix and Datadog split them).
  - Then top-N tables: expensive traces, slowest spans, top models and span names (Laminar presets).
- **Adopt:** one shared time range for the whole dashboard; drag on a chart to zoom everything, or to open the matching traces.
- **Adopt:** a grid with drag and resize (Langfuse, Laminar, Opik all use one). A chart is a SQL query with axis mapping (Laminar Advanced mode), which fits our SQL-first design.
- **Chart style:**
  - Thin lines and stacked bars, no pies.
  - Gaps, not zeros, for empty buckets on rates and percentiles (LangWatch).
  - Units in tooltips; `$` and duration formatting.
- **Avoid:** curated dashboards that must be duplicated before editing (Langfuse, Opik, LangWatch). That's a sign of a heavy dashboard model.
- **Avoid:** high-cardinality group-bys without top-N limits. Langfuse had to forbid them.

## Answered (2026-11-11)

- Transcript ships as a toggle on the combined tree (D126).
- No facet sidebar yet (D129, R3 in the C plan).
- Traces and spans are separate pages (D133). There is no spans-first default.
