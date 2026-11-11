# Competitor web UIs

Research date: 2026-11-11. Sources are public docs, changelogs, release notes, blog posts and community threads only. No source code was read.

## How to read this

Each claim carries a tag:

- **[docs]**: stated in the text of a doc page, changelog or release note.
- **[alt]**: taken from image alt text, a screenshot caption or a video description on a doc page. The research tool could not view the images themselves.
- **[inferred]**: our reading of the docs, not stated outright.
- **[not found]**: the public docs we found don't cover it.

The docs are uneven. Laminar, Langfuse, Phoenix and Opik publish detailed changelogs. LangWatch and Helicone publish little UI detail. Where a product's own docs are silent, we say so rather than fill the gap from memory.

---

## Laminar (laminar.sh)

Main sources:
- https://laminar.sh/docs/platform/viewing-traces
- https://laminar.sh/docs/changelog
- https://laminar.sh/docs/platform/table-views.md
- https://laminar.sh/docs/platform/search.md
- https://laminar.sh/docs/platform/dashboards.md
- https://laminar.sh/docs/tracing/structure/sessions.md
- https://laminar.sh/docs/platform/sql-editor.md
- https://laminar.sh/docs/platform/live-tracing.md

### Traces list
- **Tables:** Traces, Spans and Sessions are separate tables (tabs on the Traces page). [docs]
- **Input column:** It shows an auto-extracted "task", the user's request, rather than the raw root input. [docs, task extraction, 2026-10]
- **Signals:** Signal events and named clusters appear as columns on the traces table. [docs, 2026-09]
- **Columns:** Root output, tags, metadata, session ID and user ID exist, and the table-views example hides them. Sorting by Cost is possible. [docs] The full default column set: [not found]
- **Saved views:** "Table views" (2026-05) save filters, full-text search, sort and the column layout as a named view. Views are per project and shared with members, and each is scoped to one table.
  - An amber dot marks unsaved changes.
  - The dropdown offers Save changes, Save as new view, and Discard. [docs]
- **Search:** Full-text search over span input, output and flattened attributes. It is limited to the current time range, can be restricted to inputs or outputs, and is relevance-ranked. [docs] Autocomplete in the query input was added in 2025-11. [docs]
- **Not documented:** time-range presets, live list refresh, pagination, and bulk actions. [not found]

### Filter bar
- **Filters:** Exist and are saved in views, but the docs don't describe the filter UI. [docs]
- **Escape hatch:** The docs send anything filters can't express to the **SQL editor**:
  - read-only ClickHouse SQL over `spans`, `traces`, signals, evals and datasets;
  - results as a table or JSON;
  - export to a dataset or labeling queue. [docs, 2026-01]
- **"Open in SQL editor":** The span header menu pre-fills a query for that span. [docs]

### Trace view
- **Layout:**
  - A toolbar holds the view-mode dropdown, a search bar, a Timeline toggle, and Metadata, Chat and Trace buttons.
  - Selecting a span in any mode opens a **span detail pane on the right**.
  - Metadata and Chat open as side panels.
  - Whether the trace itself is a drawer or a page: [not found]
- **View modes** (one dropdown):
  - **Transcript (default since 2026-04):**
    - The agent run reads as a conversation, top to bottom. Wrapper spans are hidden.
    - Each LLM turn and tool call is a one-line preview.
    - Each sub-agent is a collapsible card with name, duration, token and cost badges, an Input block and an Output preview. [docs]
  - **Tree:**
    - Collapsible parent/child rows, a duration on every row, and token and cost badges on LLM spans.
    - The list is virtualized, with a synced minimap and a shared session-time cursor. [docs]
  - **Custom:** render templates, which are JSX components plus a SQL span filter. [docs, 2026-07]
- **Timeline:** A separate toggle shows a condensed timeline at the top of the trace view, above whichever mode is active. [docs]
  - Colored bars share one time axis.
  - Zoom with +/- or cmd/ctrl+scroll.
  - A hover needle shows exact times.
  - Drag selects a range and dims the spans outside it.
  - A session playhead appears for browser sessions. [docs]
- **Inputs and outputs:**
  - Viewers have a CUSTOM mode that renders through a JSX template. [docs, 2026-05]
  - The Metadata panel is read-only JSON with syntax highlighting, search and copy. [docs]
- **Large traces:** The tree is virtualized (2025-04/05). Transcript collapses sub-agents into cards and hides wrapper spans. [docs]
- **Keyboard:** Only cmd/ctrl+scroll zoom is documented. [docs]
- **Chat with trace:** An LLM side panel summarises the trace, with span citations shown as clickable chips that jump to the span. [docs]
- **Live:** Spans appear in an open trace as they are exported, before the root span ends. [docs]

### Sessions and users
- **Sessions table:**
  - One row per session.
  - Columns: duration, total cost, total tokens, trace count, user ID. [docs]
- **Session page (full page):**
  - A breadcrumb, then a header with duration, tokens and cost.
  - A Timeline toggle shows when each trace ran.
  - A search box matches text, span name, ID and tags.
  - Each trace is a numbered card (`1/5`) showing the extracted Input and the final LLM output, with a menu to open the full trace. [docs]
  - Sessions update live. [docs, 2025]
- **Users:** No users page. User ID is only a column. [docs]

### Dashboard
- **Layout:** Several dashboards per project. Each is a drag/resize grid of charts that share one time range (preset or custom). Bucketing is by minute, hour or day. [docs]
- **Preset charts:**
  - Traces: p90 cost, p90 duration, new traces, total duration, expensive traces, longest traces.
  - Spans: total cost, total tokens, tokens by model, top span names, expensive spans, longest spans.
  - Signal charts. [docs]
- **Chart types:** line, bar (stacked when grouped), horizontal bar, and table with infinite scroll. Line and bar charts can show a total or an average as a display value. [docs]
- **Chart editor:** A form ("Standard") or raw SQL ("Advanced") bound to `start_time`, `end_time` and `interval_unit` parameters, with a live preview. [docs]
- **Zoom:** Drag across a time-series chart to zoom, or to open the matching traces. [docs]

### Notable and complaints
- **Transcript-first:** Making Transcript the default over the tree is a strong statement that the agent's narrative matters more than span structure, with the tree "one click away". [docs]
- **SQL everywhere:** The editor, chart Advanced mode, render-template filters and "Open in SQL editor" all use SQL. This is close to our own filter design.
- **Complaints:** No public UI complaints found. The docs warn that SDK batching delays spans in the UI. [docs]

---

## Langfuse

Main sources:
- https://langfuse.com/docs/observability/features/filter-search-bar.md
- https://langfuse.com/changelog/2025-03-19-new-trace-view
- https://langfuse.com/changelog/2025-03-21-table-peek-view
- https://langfuse.com/changelog/2025-11-05-langfuse-for-agents
- https://langfuse.com/changelog/2026-08-28-responsive-timeline.md
- https://langfuse.com/changelog/2026-10-13-redesigned-session-view.md
- https://langfuse.com/changelog/2026-06-30-media-previews-json-views.md
- https://langfuse.com/faq/all/explore-observations-in-v4.md
- https://langfuse.com/blog/2026-03-10-simplify-langfuse-for-scale.md
- https://langfuse.com/docs/metrics/features/custom-dashboards
- https://langfuse.com/faq/all/dashboard-changes-in-v4.md
- https://langfuse.com/docs/observability/features/users

### Traces list
- **v4 is observations-first:** The main table is **observations**, one row per LLM call, tool or agent step. It replaces the one-row-per-trace table. [docs]
  - By default it applies "Is Root Observation = true", so it still looks like a trace list until you remove that filter. Langfuse remembers removing it, and drops the filter itself when no roots match. [docs]
  - The stated reason is that one row per trace "was increasingly hiding the signal": users look for the failing tool call, not just the failing trace. [docs, blog 2026-03]
- **Sorting:** by `total_cost`, `latency`, `model` and so on. [docs]
- **Columns:** Resizable since 2024-07, with column visibility since 2023-12. [docs] The default column list: [not found]
- **Peek view:** Clicking a row opens a preview panel without leaving the table. J and K move to the next and previous row, and Esc closes the panel. [docs, 2025-03]
- **Saved views:** "My Views" store filters, visible columns, sort and time range. [docs]
- **Live:** Data appears in near real time with recent SDKs. The UI's live toggle: [not found]
- **Not documented:** pagination style and bulk actions. [not found]

### Filter bar (v4)
- **Syntax:** A text query language of `field:value` terms joined by implicit AND, with:
  - comparisons (`latency:>2`);
  - wildcards (`name:*checkout*`);
  - negation (`-environment:production`);
  - grouped OR (`level:(ERROR OR WARNING)`);
  - quoted score names (`scores."Answer Relevance":>=0.9`);
  - `has:` presence checks;
  - free text across ids, names, input and output, or scoped with `input:` and `output:`;
  - short aliases (`env`, `cost`, `ttft`). [docs]
- **Chips:** Each term renders as a chip. [docs]
- **Autocomplete:** After a field name, the bar suggests operators and the values it has observed. [docs]
- **Errors:** A bare field name with no value is flagged invalid and not applied. Reserved tokens (`!`, lowercase `and`, `or`, `not`) are flagged. [docs]
- **Facets sidebar:** The text bar and the sidebar are **two editors over the same filter state**. Facet counts reflect the active filters, so picking a facet value never returns zero rows. [docs]
- **Sharing:** The full query is in the URL. [docs]
- **Ask AI:** Natural-language filtering, off by default. [docs]

### Trace view
- **Layout:**
  - Full trace page, plus the "side peek" from tables. [docs]
  - A tree/timeline toggle; the March 2025 redesign keeps both views "fully on par" for metrics and scores.
  - Display settings show or hide scores, comments and metrics, and turn on color coding.
  - Search finds observations by type, ID or name. [docs]
- **Log View (2025-11):** All observations concatenated into one scrollable page, so the browser's Cmd/Ctrl+F searches the whole trace. [docs]
- **Agent graph:** Inferred from timings and nesting. As of 2026-07 it has an aggregated layout and an as-it-ran DAG. [docs]
- **Responsive timeline (2026-08):** [docs]
  - Fits any trace to the available space on both axes.
  - Zoomed out, rows become single lines colored by observation type, so fan-outs are visible.
  - Scroll pans; pinch or Cmd-scroll zooms.
  - Drag selects a time range, and a Fit button resets.
  - Double-click zooms to an observation and restores names, the tree gutter and durations.
  - Hovering the left edge previews names.
  - The same component works in the trace page, the peek panel and on phones.
- **Chat rendering:**
  - "Pretty" mode for ChatML, extended to tools and functions in 2024-03. [docs]
  - Since 2025-11, every tool available to the LLM is listed at the top of each generation, and clicking one shows its definition. Called tools show their arguments and call IDs, numbered to match the list. [docs]
- **JSON:**
  - A faster JSON viewer for large payloads shipped as a beta. [docs, release notes v3.141.0]
  - Media values (media refs, data URIs, URLs that look like media) render as compact tags that open in place. [docs, 2026-06]
- **Keyboard:** J, K and Esc in the peek view. [docs]

### Sessions and users
- **Session view (redesigned 2026-10):**
  - User and assistant messages from all traces form **one continuous conversation**. [docs]
  - A sidebar lists turns, traces and tool usage. Clicking a turn jumps to it. [alt, video]
  - Search within the session.
  - Clicking a trace name opens that trace in a side panel. [docs/alt]
  - The sessions table has Tool Names (Available), Tool Names (Called) and Tool Calls filters. [docs]
  - Sessions can be published, bookmarked and annotated with scores. [docs]
- **Users table:** first event, last event, trace count, tokens, cost. [alt, video] The user page shows observation totals, tokens, cost, the user's traces and a Scores tab. [docs]

### Dashboard
- **Home:**
  - The project Home is itself a dashboard.
  - Its default is a curated overview of traces, model cost, latency percentiles, scores and model usage.
  - Curated Latency, Cost and Usage dashboards are also provided.
  - Editing a managed dashboard duplicates it first. [docs]
- **Widgets:**
  - A widget is built from a data source (traces, observations or scores), metrics with aggregations (p95…), dimensions, filters and a chart type: line, bar, time series or pie.
  - Widgets live in a library and can be copied or exported as JSON or CSV. [docs]
  - Layout is a drag/resize grid.
- **v4 constraints:** High-cardinality dimensions such as userId need top-N, and time series can't group by them. Trace latency percentiles may be dropped because they are slow at scale. [docs]

### Notable and complaints
- **Complaints:**
  - Large users could only view "a few days of data" in dashboards, and listing recent traces "took seconds". This was the motivation for v4. [docs, blog]
  - Multi-megabyte inputs flood the trace viewer, and the advice is to truncate at the SDK. [docs/community, see search notes]
  - The Responsive Timeline changelog asks where "large traces stop keeping up", so large traces are a known pain point. [docs]

---

## Arize Phoenix

Main sources:
- https://arize.com/docs/phoenix/release-notes (and the entries linked below)
- https://arize.com/docs/phoenix/release-notes/08-2026/08-17-2026-trace-filters-and-analytics-sql
- https://arize.com/docs/phoenix/release-notes/07-2026/07-14-2026-command-palette-sessions-and-tables
- https://arize.com/docs/phoenix/release-notes/07-2026/07-07-2026-metric-charts-trace-search-and-rest-api
- https://arize.com/docs/phoenix/release-notes/07-2026/07-28-2026-experiment-charts-span-downloads-and-root-span-filters.md
- https://arize.com/docs/phoenix/release-notes/09-2026/09-29-2026-editable-dataset-examples-and-trace-metrics.md
- https://arize.com/docs/phoenix/release-notes/06-2026/06-16-2026-time-range-metrics-and-sharing.md
- https://community.arize.com/x/phoenix-support (performance threads)

### Traces list
- **Tabs:** A project has Spans, Traces and Sessions tabs, each a table. [docs]
- **Chart strip:**
  - A **resizable strip of metric charts** sits above each table.
  - Charts are chosen per table from a catalog, and legend toggles hide series.
  - Drag zooms all charts and the table to that window. Clicking a chart bin sets the time range. [docs, 2026-05 to 2026-08]
  - A latency/token/error metrics panel beside the spans table is on by default. [docs, 2026-05]
- **Columns:**
  - The column chooser supports drag-to-reorder and persists per project. [docs, 2026-07]
  - Each annotation gets its own column. [docs, 2026-08]
- **Time range:**
  - A searchable selector accepts typed durations ("85d").
  - A two-month calendar with editable start and end times; changes apply on confirm.
  - A strip to pan and zoom the window.
  - Shared links keep relative presets relative. [docs, 2026-06]
- **Live:** A play/pause toggle streams live data. [docs]
- **Pagination:** A "load more" footer. [docs, 2026-09]
- **Older note:** "+ n more spans" appeared in the trace table. [docs, 2025-03]
- **Not documented:** bulk actions. [not found]

### Filter bar
- **Language:** A **Python-like expression language**, one per table (span, trace and session filters are distinct). Examples:
  - `num_spans > 10 and error_count > 0`
  - `max(span.latency_ms for span in spans) > 5_000`
  - comprehensions over `spans`, `trace_annotations` and `span_annotations`, with `any`, `all`, `len`, `max`, `min` and `sum`
  - topology helpers: `children`, `parent_span`, `siblings`. [docs, 2026-08]
- **Typeahead:**
  - Field names are grouped by category.
  - Picking a comprehension inserts an editable snippet with the loop variable already named.
  - Recent conditions are remembered per project. [docs]
- **Errors:**
  - Unknown names are rejected with "did you mean".
  - Malformed or over-nested expressions get a clear syntax error. [docs, 2026-08, 19.11.1+]
  - The release notes say that **previously a misspelled name silently matched nothing**: a known problem, now fixed. [docs]
- **AI query:** A toggle turns plain English into an expression. [docs]
- **Annotation popover:** Hovering an annotation token shows every annotation with its score and author. Inline **filter chips add that condition** to the table filter. [docs]
- **Root-span scoping:** Part of the expression (`parent_span is None`), not a separate flag. [docs]

### Trace view
- **Layout:** The trace tree sits on the left with span details on the right (long-standing layout). [inferred from docs: "trace detail view", "span info tab", tree rows]
  - A separate timeline view, added in 11.26 (2025-08), shows span order and duration. [docs] How it sits relative to the tree: [not found]
- **Tree rows:**
  - Each row ends with **latency, tokens and cost**. [docs, 2026-09]
  - Annotation badges sit under rows, unfavourable results first. [docs, 2026-10]
  - Hovering a row shows a breakdown preview that follows the pointer and loads only once it settles, "which keeps large traces responsive". [docs]
- **Search:** A box above the tree filters it as you type. [docs, 2026-07]
- **Span details:**
  - An "Info" tab of collapsible cards, with one control to collapse or expand all.
  - Attributes as a **flattened, searchable key/value table**.
  - Annotations and notes as sortable tables.
  - A download menu: span JSON, span OTLP JSON, or the whole trace. [docs, 2026-07]
  - Pressing `n` opens a pinned note bar. [docs]
- **Messages:** OTel GenAI semconv traces render with full message I/O, tool calls and token counts. [docs, 2026-05]
- **Command palette:** ⌘K, keyboard-first, across the app. [docs, 2026-07]

### Sessions and users
- **Sessions table:**
  - A collapsible stats panel shows the session count, average traces per session, duration (average/P50/P99) and per-label annotation summaries. [docs]
  - Session filters use the same expression language. [docs]
- **Session details:**
  - Turns are separated by dividers, each with a copy control.
  - Long messages collapse by default.
  - Each turn has thumbs up/down and annotate buttons.
  - Hovering a turn previews evals, tokens and cost. [docs]
- **Users:** No users page found. [not found]

### Dashboard
- **Metrics page:**
  - Latency, error rate, tokens and trace count.
  - Prompt tokens split into input, cache and audio; completion tokens into output, reasoning and audio.
  - An average-score chart per annotation name at span, trace and session level.
  - Drag zooms all panels together. [docs]
  - Chart style: [not found]

### Notable and complaints
- **Slow loading** of projects pages and traces with large payloads (base64 images); support cites GitHub issues #4006, #6201, #3917 and #4399. [community]
- **Tree cap:** A user reported the trace tree capped at 50 items, so the rest could not be scrolled to. [community]
- **Fixes:** lazy span loading, a page size cut from 100 to 50, and removing double fetching. [community, docs 2025-03]

---

## LangWatch

Main sources:
- https://langwatch.ai/llm-observability (product page)
- https://langwatch.ai/changelog
- https://langwatch.ai/changelog/traces-v2-model-cost-preview
- https://langwatch.ai/docs/features/querying-your-data.md
- https://langwatch.ai/docs/features/dashboards.md

The docs have no UI reference for the trace explorer or trace drawer. Most of what follows comes from the product page and changelog.

### Traces list
- **Trace Explorer:**
  - "Ask AI" natural-language querying.
  - Facet counts for Origin (Application, Evaluation, Simulation, Gateway), Status (OK, Error, Warning) and Error message.
  - Filters by model, labels and metadata. [docs, product page]
- **Lenses:** Saved views that keep layout, filters and columns. [docs]
- **Bulk action:** Build a dataset from selected traces. [docs]
- **Live updates:** Exist; a 2026-06 entry made them "calmer". [docs]
- **Not documented:** default columns, density, time range and pagination. [not found]

### Filter bar
- **Search language:** The explorer's search bar speaks a text language: [docs]
  - `status:error AND model:gpt-*`
  - `trace.attribute.<key>`, `span.attribute.<key>`, `event.attribute.<key>`
  - `has:feedback`, `has:annotation`
  - `evaluatorVerdict:fail`
  - free text.
- **Errors:** An unknown field returns an error naming the field and listing the valid ones, via the API. How the UI shows it: [not found]

### Trace view
- **Views:** Waterfall (Gantt, one row per span with a start/duration bar and type labels `rag`, `llm`, `tool`, `fn`), flame chart, span list, topology graph, and sequence diagram for agent-to-agent communication. [docs, product page text labels]
- **Input/output:** Viewable as pretty, plain text, JSON or markdown. [docs]
- **Other features:**
  - "AI prompt view", evaluator drilldown and per-span cost. [docs, 2026-06]
  - A span-tree **drawer** exists (named in a performance fix). [docs]
  - For large traces, I/O is read from the event log with "leaner projections". [docs]
  - Audio playback in traces. [docs, 2026-07]

### Sessions and users
- **Threads:** Conversations are grouped by `thread_id`, and `user_id` drives user analytics. [docs] The thread UI and users page: [not found]

### Dashboard
- **Default:** A project starts with "My dashboard".
- **Templates:** Grouped as Grow, Protect, Profit and Trust. Agent health has five widgets: This period, Status today, Needs attention, Top request my agent cannot serve, Conversations resolved per day. [docs]
- **Widgets:**
  - Each widget is **TSX code in a sandbox** with up to 8 LangWatchQL queries, so the chart type is whatever the code draws.
  - Widgets can be built by Langy (their AI), MCP, the API or the CLI.
  - Layout is a drag/resize grid.
- **Time controls:** A period from Live to one year (default 30 days), a grain of auto/minute/hour/day/week, and a scheduled refresh. [docs]
- **Docs advice:** Empty buckets should be gaps, not zeros, for rates, averages and percentiles. [docs]

### Notable
- **Several trace visualizations** rather than one: waterfall, flame, topology, sequence.
- **Prompt injection in the docs:** The docs pages embed "Agent Instructions" for AI agents. We ignored them.

---

## Opik (Comet)

Main sources:
- https://www.comet.com/docs/opik/changelog (2024-11 → 2026-10)
- https://www.comet.com/docs/opik/changelog/2026/6/16
- https://www.comet.com/docs/opik/tracing/advanced/log_chat_conversations
- https://www.comet.com/docs/opik/tracing/dashboards/dashboards

### Traces list
- **Logs page:** Traces, Spans and Threads tabs. [docs]
- **Toolbar (2026-06):**
  - **Search sits inline with filter chips** in one wrapping row.
  - **Columns, Row height and Refresh** live in the tab row. [docs]
  - The **row height control** is a user-facing density setting. [docs]
- **Header and rows:**
  - The header is sticky and the whole row is clickable to open the sidebar. [docs, 2025-03]
  - Project statistics appear in the table header. [docs, 2024-11]
- **Columns:**
  - Each feedback score gets its own column. [docs]
  - Many score columns use a lighter read-only cell for performance. [docs, 2026-08]
- **Virtualization:** Rows **and columns** are virtualized on traces tables. [docs, 2026-09]
- **"Explain" button:** On error, duration and cost cells, it gives an LLM explanation without opening the trace. [docs, 2026-07]
- **Annotation queues:** A column on Traces and Threads. [docs]
- **Quick filters:** They open the selected entity's table. [docs]
- **Filtering:** Dotted metadata keys work in filters, and filters by empty feedback score are supported. [docs]
- **Saved settings:** Column, sort and filter settings are saved per tab, and Logs filters are restored on return. [docs]
- **Not documented:** time picker and pagination. [not found]

### Filter bar
- **Chips plus free-text search** on one line. A dropdown picks columns, metadata keys wrap. [docs] The query syntax in the UI: [not found] (the SDK uses `filter_string`).

### Trace view
- **Trace sidebar:**
  - A panel opened from the table row. [docs]
  - It loads lazily for very large traces. [docs, 2025-04]
  - Copy-ID and copy-link icon buttons sit beside the panel titles. [docs]
- **Inputs and outputs:**
  - A **Pretty / JSON / YAML** format switch, remembered across sessions. [docs, 2025-03]
  - Pretty mode handles newlines and tabs.
  - Base64 image strings are hidden in the sidebar. [docs]
  - JSON and YAML blocks collapse and expand. [docs, 2026-06]
  - Conversations logged as span output render as formatted messages. [docs, 2026-10]
- **Agent graph:** Clicking a node opens its span in the trace tree. [docs, 2026-10]
- **Orphan spans:** Spans whose parent isn't loaded appear at the top. [docs]
- **Not documented:** detail tab names, a timeline, and keyboard shortcuts. [not found]

### Sessions and users
- **Threads table:** Shows feedback scores per thread. [docs]
- **Thread view:**
  - Trace input renders as the user message and output as the assistant message, in markdown.
  - Each response has Trace and Tool calls links and thumbs up/down.
  - An Annotate button adds thread-level scores, tags and comments. [docs]
  - Internal traces are hidden behind a toggle and badged. [docs, 2026-10]
- **Users:** No users page found. [not found]

### Dashboard
- **Project overview (read-only):**
  - **At a glance:** trace count, error count, p50 and p99 latency, total estimated cost.
  - **Volume, latency & cost:** charts over time.
  - **Quality & conversations:** feedback scores, thread volume, duration and scores. [docs]
- **Custom dashboards:**
  - Collapsible, reorderable sections over a **6-column grid**, with autosave.
  - Widgets: time series, single metric, markdown, and LLM-built charts.
  - The date picker runs from 24 h to 60 days (default 30 days). [docs]
- **Chart style:** Bars are violet. [docs]

### Notable
- **Density controls:** An explicit row-height control and a Pretty/JSON/YAML toggle are both small and worth copying.

---

## Helicone

Main sources:
- https://www.helicone.ai/changelog/20241003-request-redesign
- https://www.helicone.ai/changelog/20250329-filter-ui-update
- https://www.helicone.ai/changelog/20250418-april-changelog
- https://www.helicone.ai/changelog/20250506-smarter-sessions-insights
- https://docs.helicone.ai/features/sessions
- https://docs.helicone.ai/features/hql
- https://community.helicone.ai/applying-filters-saving-and-adding-functionality-PR6SlcbFpQl0

Helicone is proxy-first, so its main list is **Requests**, not traces. Its official docs say little about the UI, and several detailed pages exist only on third-party mirrors, which we did not use.

### Requests list
- **Density:** The 2024-10 redesign made rows **more compact**, to "fit more on screen".
- **Drawer:** A request drawer **stays open while you move between requests**. [docs]
  - The drawer was redesigned again in 2025-04 for requests, responses and key metrics. [docs]
- **Time selector:** Quick-select presets (2024-10), and a calendar plus time picker (2025-04). [docs]
- **Custom properties:** They appear as columns and filters. [docs]

### Filter bar
- **Unified filter (2025-03):**
  - One filter component used everywhere.
  - Filters **persist across the dashboard, requests, users and sessions pages**.
  - Filters are in the URL. [docs]
- **Interaction:** "Add Filter" → pick a property → pick a value. [community]
- **Complaint:** A filter from Properties didn't apply on Requests, or applied only after a delay. [community]
- **HQL (selected workspaces):**
  - A SQL editor with a left table/column browser.
  - Cmd+Enter runs and Cmd+S saves.
  - Saved queries are shared in the org. [docs]

### Trace / session view
- **Sessions:** Built from a path header (`/task/research/web_search`), which gives a hierarchy. [docs]
  - A duration distribution chart colors requests by path. [docs]
  - Session-level metrics: average latency and total cost (2025-05). [docs]
  - Tree, timeline and chat views: [not found] in official docs.

### Users and dashboard
- **Users:**
  - User metrics, plus a cohort retention table (Week 1/2/4/8/12).
  - Automatic segments: Power, Casual, New, At-Risk. [docs]
- **Dashboard:** Headline KPIs (requests, cost, latency, errors, users) and time series. These come from third-party summaries only and are unverified.

---

## Datadog LLM Observability (reference for dense UI)

Main sources:
- https://docs.datadoghq.com/llm_observability/monitoring/
- https://docs.datadoghq.com/llm_observability/investigate/
- https://docs.datadoghq.com/llm_observability/investigate/querying.md
- https://docs.datadoghq.com/tracing/trace_explorer/trace_view/ (APM trace view, which LLM Obs reuses)
- https://www.datadoghq.com/blog/datadog-llm-observability/

### Traces list and filter bar
- **Query bar:** The search uses Datadog's query syntax:
  - `ml_app:shopist-chat-v2 'purchase' -'discount' @trace.total_tokens:>=20`
  - tags as `key:value` and attributes as `@key:value`;
  - free text in quotes, matching the first 20,500 characters of I/O;
  - `AND`, `OR` and `-`;
  - `@evaluation…`, `@feedback…`, `@meta.metadata.<key>`;
  - `@child` for nested spans, `@trace.*` for trace totals. [docs]
- **Scope:** A Traces / Spans toggle: root spans only, or all spans. [docs]
- **Explorer layout:** A facet sidebar, a timeseries above the list, saved views, and a live time picker. These are general Datadog explorer conventions; the LLM pages' text doesn't describe them. [inferred]

### Trace view (APM docs)
- **Layout:**
  - A side panel with an "Open Full Page" permalink.
  - A header with root service and resource, trace ID, duration and start.
  - Below it, a **height-adjustable bottom section** holds span details. [docs]
- **Four visualizations:** Flame Graph (default), Span List, Waterfall and Map. [docs]
  - **Flame graph:** a minimap, scroll to zoom and drag to pan, and a legend by service with % exec time. An "Errors" checkbox highlights error spans. [docs]
  - **Waterfall:** each row is a span with a duration bar plus absolute ms and status. Chevrons expand and collapse, with Expand all / Collapse all buttons and color-by. [docs]
  - **Span List:** groups spans by resource, with sortable count, average duration and % exec time. [docs]
- **Focus on span:** Makes a span the root of the view. [docs]
- **In-trace search:** Highlights matches with prev/next arrows and supports `key:value`, `OR` and `duration:>200ms`. [docs]
- **Large traces:** Above 100 MB, "Trace Preview" keeps only key spans (entry, error, long, linked). Only Waterfall and Map are offered, with **numbered pills that expand hidden groups**. [docs]
- **LLM specifics:**
  - The flame graph with a selected `OpenAI.createResponse` span shows input and output messages. [alt]
  - An **Evaluations** tab shows a hallucination check with the flagged output, a context quote and an explanation. [alt]
  - An Errors tab exists. [docs]

### Dashboard
- **Operational Insights:** An out-of-the-box dashboard with trace- and span-level metrics, error rates, latency breakdowns, token consumption (prompt, cached, completion, total), model usage and triggered monitors. [docs]
- **Patterns/Clusters:** Topic clusters with KPIs: clustered interactions, topics, % clustered. [alt]
