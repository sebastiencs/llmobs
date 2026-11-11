# C · Trace view (U15–U20)

See [README.md](README.md) for the decisions, boundaries, dependency versions and the rules that apply to every sub-phase.

U7 built the trace view's frame:
- the split;
- the combined tree and timeline grid;
- a span detail with a plain Input/Output tab.

These sub-phases complete it as spec 11 § Trace view describes, with D126's Transcript mode. Everything here lives in `web/src/components/trace/` and `web/src/components/payload/`. The traces page split, the spans page split and the full page (U19) render the same `TraceView`.

---

## U15 — Timeline interactions and tree search

**Edit only:** `web/src/lib/timeline.ts` (+ test), `web/src/lib/tree.ts` (+ test), `web/src/components/trace/{tree-grid,tree-toolbar,trace-view}.tsx` (+ tests).

**Commit:** `web: timeline zoom and tree search`

### Zoom window

`lib/timeline.ts` gains a view window `{startNs, endNs}` inside the trace window, and these pure functions:
- `zoomAt(window, view, anchorPct, factor)`: zoom about a point;
- `zoomToSpan(window, span)`: the span plus 5 % padding each side;
- `panBy(window, view, deltaPct)`;
- `clampView(window, view)`: the minimum width is 1 µs, and the view never leaves the trace window.

`barGeometry` takes the view window. Bars outside it are hidden; bars crossing its edges are clipped with a small notch on the clipped side.

### Interactions (timeline column only)

- **Ctrl/⌘ + wheel** zooms about the pointer.
- **Shift + wheel** pans.
- **Drag across the timeline header** selects a range and zooms to it.
- **Double-click a bar** zooms to that span.
- **"Fit"** (toolbar, or the key `0`) resets to the trace window.
- **Hover needle:** a 1 px vertical line follows the pointer in the timeline column, with a label of the offset from trace start (`formatDuration`).

The zoom window is component state; it is not in the URL.

### Tree toolbar (`tree-toolbar.tsx`, above the grid, 32 px)

- **Search box:** case-insensitive match on span name, model and `span_id` prefix.
  - Matches are highlighted.
  - Non-matching rows stay visible, but dimmed.
  - The ancestors of matches are auto-expanded.
  - "n / N" with prev/next buttons and Enter / Shift+Enter, which move the selection to the next or previous match.
- **"Errors only"** toggle: the visible rows become error spans plus their ancestors. `lib/tree.ts` gains `filterRows(spans, ends, predicate): number[]`, which keeps ancestors. It's O(n) and stack-based.
- **Expand all / collapse to depth 1.**
- **"Fit"** and the current zoom label.

**Tests:**
- pure tests for `zoomAt`, `zoomToSpan`, `panBy`, `clampView` and `filterRows` (including 10 000 spans: the counts are right);
- page tests:
  - search "tool" highlights and auto-expands the fixture trace's tool span;
  - Enter moves the selection;
  - "Errors only" on fixture trace 3 (`a100…0003`, the one with an error span) leaves the error span and its ancestors;
  - double-click a bar changes the tick labels (zoomed);
  - Fit restores them.

---

## U16 — Span header and attributes

**Edit only:** `web/src/components/trace/{span-detail,span-header,attributes-tab,events-list}.tsx` (+ tests), shadcn `scroll-area` and `collapsible` if needed.

**Commit:** `web: span header, attributes, events and links`

### `SpanHeader` (replaces U7's header line)

Two compact lines:
1. `KindIcon`, the name (selectable text), `StatusDot` + `status_message`, and `IdText` of the span with copy.
2. Small "key value" pairs, each shown only when present:
   - model (`model`, with `request_model` / `response_model` in the tooltip when they differ);
   - provider and operation;
   - duration and TTFT;
   - tokens: in / out / cache read / cache write / reasoning, as `in 2.5k · out 100 · cache 2.2k`;
   - cost, with a tooltip of input / output / source (`cost_source`: computed / reported / none);
   - service;
   - environment and release;
   - session and user, as links to their pages once U21/U22 exist (plain text before that).

**Badges:**
- "truncated at ingest" when `truncated` is non-empty (the tooltip lists the attribute names);
- "detached" for a detached span;
- the `conventions` list as small muted badges.

### Tabs

The order at the end of C is **Chat** (U18, when `input.format` or `output.format` is `messages`), **Input / Output** (U17), **Attributes**, **Scores** (U19). The selected tab persists per user in localStorage (`${STORAGE_PREFIX}span-tab`); when the stored tab doesn't apply, it falls back to the first available.

### Attributes tab

- **A search field** that filters every section by key or value substring.
- **Four sections**, each collapsible with a count: **Metadata**, **Attributes**, **Resource attributes** and **Request params** (`request_params`, parsed JSON).
  - Each is a two-column key/value list, 24 px rows, with mono keys, sorted by key.
  - Values that parse as JSON objects or arrays render with the JSON viewer (U17) once it exists; before U17, as `<pre>`.
  - Each row has a copy button.
  - Metadata and attribute rows get a `+` click-to-filter action, which appends `metadata['k'] = 'v'` or `attributes['k'] = 'v'` to the **traces or spans list filter** of the page the trace is open in (no-op on the full page). It goes through a callback prop `onAddFilter?: (conjunct: string) => void` that the list pages pass.
- **Events:** time (offset from span start), name, attributes.
  - `exception` events show `exception.type` and `exception.message` prominently.
  - `exception.stacktrace` goes in a collapsible mono block, collapsed when longer than 12 lines.
- **Links:** `trace_id` / `span_id` as `IdText`; a link to the trace when it is in this project (`/traces/$traceId?span=`); attributes.

**Tests:**
- the header shows the fixture LLM span's model, tokens and cost, and the cost tooltip shows the source;
- the truncated badge appears for a span with `truncated` (use `server.use` to return a modified fixture);
- attribute search filters rows;
- an exception event shows its stacktrace collapsed;
- click-to-filter from an attribute row appends to the list's filter (on the traces page with a span open).

---

## U17 — The JSON viewer and the Input/Output tab

**Edit only:** `web/src/components/payload/{json-view,payload-view,format-switch}.tsx` (+ tests), `web/src/components/trace/{span-detail,io-tab}.tsx`, `web/src/components/trace/attributes-tab.tsx` (switch to the JSON viewer), `web/src/lib/json-rows.ts` (+ test).

**Commit:** `web: JSON tree viewer and payload views`

Spec 11 asks for a custom collapsible tree, with no Shiki.

### `lib/json-rows.ts` (pure)

It flattens a JSON value into rows `{path, depth, key, kind: "object"|"array"|"string"|"number"|"boolean"|"null", preview, childCount, end}`, in pre-order. Like `tree.ts`, `end` is the index past the last descendant, so collapsing reuses the same row-skipping logic. It's iterative, with no recursion: deep JSON from the network must not overflow the stack. Long strings are kept whole in the row and truncated in rendering.

### `JsonView({value, defaultExpandDepth = 2})`

- **Virtualized** when there are more than 300 rows; plain otherwise. 22 px rows.
- **Rows:** key (mono, muted), value coloured by kind, and `{…} 12 keys` / `[…] 3 items` previews on collapsed nodes.
- **Strings** over 200 characters show the first 200 plus "… (N chars)". Clicking expands that one string inline, as a wrapped block.
- **Toolbar:** expand all, collapse all, copy (the whole value as pretty JSON), a search that highlights matching keys and values and auto-expands their ancestors.
- **`media://` strings** (raw JSON media references, spec 02 step 5) render as a media chip; U18's `MediaChip` is used once it exists.

### `PayloadView({payload, label})`

The `Payload` `{format, value}` (D70). A **format switch** (Pretty / JSON / Raw) is remembered per user in localStorage (`${STORAGE_PREFIX}payload-format`):
- `json`: Pretty = `JsonView`; JSON = pretty-printed text in a `<pre>`; Raw = the compact JSON.
- `text`: Pretty = wrapped text with preserved newlines; JSON and Raw = the string.
- `messages`: Pretty = the compact message list (role + text of each message), until the Chat tab exists (U18); JSON = `JsonView` of the messages array; Raw = compact JSON.

### Input / Output tab

- Input on top, Output below, each with a header (label, format badge, size in characters, copy).
- When the span has `system_instructions` or `tool_definitions`, they come first, each collapsed under its own header, with JSON as `JsonView`.
- **Rendering cap:** above 1 MB of serialized JSON, the Pretty view shows a "Large payload (N MB)" notice with a "Render anyway" button, and JSON and Raw are offered first.

**Tests:**
- `json-rows` on nested values;
- a deep (depth 5 000) generated array doesn't throw;
- collapse and expand;
- search auto-expands;
- format switch persistence;
- a long string truncates and then expands;
- the 1 MB cap shows the notice;
- the fixture LLM span's input shows in the Input / Output tab.

---

## U18 — The Chat tab and media

**Edit only:** `web/src/components/payload/{chat-view,message-view,part-view,tool-call-card,media-chip,media-view}.tsx` (+ tests), `web/src/components/trace/{span-detail,chat-tab}.tsx`, `web/src/components/payload/json-view.tsx` (to use `MediaChip`), `web/src/lib/media.ts` (+ test).

**Commit:** `web: chat rendering, tool calls and media`

Canonical messages (spec 01 § Canonical message format, schema `Message` / `Part`).

### `ChatView({input, output, systemInstructions, toolDefinitions})`

- **System instructions** first, collapsed to one line with a line count. Click to expand.
- **"Tools (N)"**: a collapsible list of `tool_definitions` (name and description, with the JSON schema in `JsonView`). The names of tools called in this span are highlighted.
- **Input messages, then a thin divider labelled "Output", then output messages.**
- **`MessageView`**, a compact block:
  - a header line with the role (system / user / assistant / tool / other, as text with a coloured left border per role), `name` when present, and `finish_reason` when present as a muted badge;
  - then the parts.
  - A message longer than 40 lines is collapsed with "Show N more lines".
- **`PartView` per part type:**
  - `text`: wrapped text with preserved newlines. There is no markdown rendering in this plan; it would need a dependency.
  - `reasoning`: collapsed by default, labelled "Reasoning" with a character count, and a muted italic body.
  - `tool_call`: `ToolCallCard` with the name, the `id` (`IdText`) and `arguments` in `JsonView`, or as text when it isn't JSON.
  - `tool_call_response`: `ToolCallCard` with the response, labelled with the matching call's name when a `tool_call` with the same `id` exists in this span's messages. Clicking the label scrolls to the call, and the call shows "→ response" linking forward.
  - `uri` and `blob`: `MediaView` (below).
  - `unknown`: `JsonView` of the part with an "Unknown part" label.

### Media

`lib/media.ts`:
- `mediaUrl(projectRef, uri)`: `media://<sha256>` → `/api/v1/projects/<encoded ref>/media/<sha256>`;
- `llmobs:dropped` → `undefined`;
- other URIs (`https:`, `data:`) are returned as is. **External URLs are never loaded automatically** (no network calls, spec 11).

`MediaView`:
- **image** (`modality` image, or `mime_type` `image/*`): a thumbnail of max 240 × 160 with `loading="lazy"`; a click opens a dialog at full size.
- **audio:** `<audio controls preload="none">`.
- **PDF** and other types: a `MediaChip` (icon, MIME type, short SHA) linking to the media URL in a new tab.
- **external `https:` URIs:** a chip with the URL text and a "Load" button.
- **`llmobs:dropped`:** a muted chip "Media dropped at ingest (too large, N MB)".
- **`blob` parts** (inline base64 below the extraction threshold): render from a `data:` URL built from `content` and `mime_type`. The base64 is never shown as text.

The Chat tab is the default tab when the span has `messages` input or output (U16 order).

**Tests:**
- fixture span `b100000000000043` of trace `a100…0004` references `media://4977…`: its Chat tab renders an `<img>` whose `src` is the media URL;
- a tool call and its response are linked (build a span with `server.use`);
- reasoning is collapsed;
- system instructions are collapsed with a line count;
- `llmobs:dropped` shows the dropped chip;
- an external URL is not loaded until "Load" is clicked;
- `mediaUrl` encodes the project ref.

---

## U19 — Scores, the trace header and the full page

**Edit only:** `web/src/components/trace/{scores-tab,trace-header,trace-view}.tsx` (+ tests), `web/src/routes/$org/$project/traces/$traceId.tsx` (new), `web/src/lib/search/traces.ts`, `web/src/api/queries.ts`.

**Commit:** `web: trace header, scores tab and trace page`

### Scores tab (read-only, D123)

- `list_scores` with `trace_id` (and `span_id` when a span is selected; the trace's own scores when the root is selected). Show both groups: "On this span" and "On this trace".
- **A table:** name, value (numeric with 3 decimals, boolean ✓/✗, categorical or text value), source, reason (truncated, full on hover), author or evaluator (whichever is present), updated time. Sorted by name.
- **Empty state:** "No scores". The annotation form is not in this plan (D123).

### `TraceHeader` (replaces U7's header)

- **Left:** `StatusDot`, the name, `IdText` (trace ID), `TimeText` of start.
- **Totals:** duration, spans (count), LLM calls, tokens (with the breakdown tooltip), cost.
- **Context:** session and user (links when their pages exist, else text), environment, release, tags.
- **Actions:**
  - **copy link** (the full-page URL of the trace with `span`);
  - **open full page** (only in the split);
  - **close** (only in the split).

  Each action not in this plan (add to dataset, run evaluator, delete) is simply absent.
- The latest scores per name (`TraceSummary.scores`) as compact badges `helpfulness 0.9`.

### Full page

`/$org/$project/traces/$traceId` renders `TraceView` full width with `span` and `mode` search params. Its loader prefetches `get_trace`; a 404 `Problem` shows the not-found state. Back navigation returns to the list as it was: the list's URL holds its state.

**Tests:**
- the scores tab shows the fixture scores for trace 1;
- the header shows the totals and tags;
- copy link writes the full-page URL;
- the full page renders and selects `?span=`;
- an unknown trace gives not-found.

---

## U20 — Transcript mode

**Edit only:** `web/src/components/trace/{transcript-view,trace-view,tree-toolbar}.tsx` (+ tests), `web/src/lib/transcript.ts` (+ test), `web/src/lib/search/traces.ts`.

**Commit:** `web: transcript mode for agent traces`

D126. A segmented control in the trace toolbar, **Tree | Transcript**, writes `mode`. In Transcript mode, the left pane shows the transcript instead of the tree grid. The span detail pane stays, and clicking a turn selects its span.

### `lib/transcript.ts` (pure)

`transcriptSpans(spans: TreeSpan[]): TreeSpan[]` keeps the spans with `span_kind` `llm` or `tool`, in pre-order. That's start order within siblings, and a reasonable conversation order. Agent and chain spans are kept as **section headers** only when they have at least one kept descendant: `{kind: "section", span, depth}` items. Other spans are dropped.

### `TranscriptView`

- **Items:**
  - a **section header** (24 px): the agent or chain name with `KindIcon`, indented by section depth, collapsible;
  - an **LLM turn**: the model, duration, tokens and cost on one line, then the **new** messages of that call.
    - When an LLM span's input repeats the previous LLM span's conversation, show only the messages not seen in the previous call's input + output: a prefix match on role + text.
    - When the inputs don't share a prefix, show the full input.
    - Then the output messages.
    - Rendered with `MessageView`.
  - a **tool turn**: `ToolCallCard` with the input as arguments and the output as response.
- **Loading** (R2): span details are fetched lazily as items scroll into view (the list is virtualized with dynamic row heights: `measureElement`).
  - The concurrency is at most 4 requests.
  - At most the first 50 transcript spans are loaded until the user clicks "Load more" (+50).
  - Each item shows a skeleton until its detail arrives.
  - Details go through the same query key as the span detail (`get_span`), so switching to Tree and selecting a span is instant.
- A trace with no LLM or tool spans shows "No LLM or tool calls in this trace. Switch to Tree."

**Tests:**
- `transcriptSpans` on a hand-built tree (sections kept only with kept descendants);
- the prefix de-duplication;
- page:
  - `mode=transcript` on fixture trace 1 (agent → llm, tool, llm) renders an agent section with two LLM turns and the tool turn between them;
  - clicking a turn selects that span in the detail pane;
  - the toggle writes `mode`;
  - with `largeHandlers` and a trace with more than 50 LLM spans (generate one with a seed that has it, or use `server.use` with a built trace): at most 50 `get_span` requests before "Load more", and at most 4 in flight at once (track it in the handler).
