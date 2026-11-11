import { type HttpHandler, HttpResponse, http } from "msw";
import type { Schema } from "@/api/types";
import { type Dataset, visibleTraces } from "@/mocks/generated/dataset";
import {
  type SpanStreamSource,
  type StreamEmit,
  spanStreamHandler,
  type TraceStreamSource,
  traceStreamHandler,
} from "@/mocks/sse";

const PROJECT = "/api/v1/projects/:project";

type Order = Schema<"SortOrder">;

export function largeHandlers(dataset: Dataset): HttpHandler[] {
  return [
    traceStreamHandler(liveTraceSource(dataset)),
    spanStreamHandler(liveSpanSource(dataset)),
    http.get(`${PROJECT}/traces`, ({ request }) => listTraces(dataset, request)),
    http.get(`${PROJECT}/traces/:trace_id/spans/:span_id`, ({ params }) =>
      oneSpan(dataset, params.trace_id, params.span_id),
    ),
    http.get(`${PROJECT}/traces/:trace_id`, ({ params }) => oneTrace(dataset, params.trace_id)),
    http.get(`${PROJECT}/spans`, ({ request }) => listSpans(dataset, request)),
    http.get(`${PROJECT}/sessions/:session_id`, ({ request, params }) =>
      oneSession(dataset, request, params.session_id),
    ),
    http.get(`${PROJECT}/sessions`, ({ request }) => listSessions(dataset, request)),
    http.get(`${PROJECT}/users/:user_id`, ({ request, params }) =>
      oneUser(dataset, request, params.user_id),
    ),
    http.get(`${PROJECT}/users`, ({ request }) => listUsers(dataset, request)),
    http.get(`${PROJECT}/metrics`, ({ request }) => metrics(dataset, request)),
  ];
}

function listTraces(dataset: Dataset, request: Request): Response {
  const parsed = readQuery(new URL(request.url));
  if (parsed instanceof Response) {
    return parsed;
  }
  const rows = sortTraces(filterTraces(dataset.traces, parsed), parsed.sort, parsed.order);
  return page(rows, parsed, ignoredFilter(parsed));
}

function listSpans(dataset: Dataset, request: Request): Response {
  const parsed = readQuery(new URL(request.url));
  if (parsed instanceof Response) {
    return parsed;
  }
  const traces = sortTraces(filterTraces(dataset.traces, parsed), "start_time", parsed.order);
  if (parsed.sort !== "start_time") {
    const spans = traces.flatMap((trace) => spansOf(dataset, trace.trace_id, "asc"));
    return page(
      sortSpans(filterSpans(spans, parsed), parsed.sort, parsed.order),
      parsed,
      ignoredFilter(parsed),
    );
  }
  return pageSpans(dataset, traces, parsed);
}

function oneTrace(dataset: Dataset, traceId: string | readonly string[] | undefined): Response {
  const id = text(traceId);
  const detail = extraDetail(dataset, id) ?? dataset.traceDetail(id);
  if (!detail) {
    return problem(404, "not_found", "Not Found");
  }
  return HttpResponse.json(detail);
}

function oneSpan(
  dataset: Dataset,
  traceId: string | readonly string[] | undefined,
  spanId: string | readonly string[] | undefined,
): Response {
  const trace = text(traceId);
  const span = text(spanId);
  const fromExtra = extraDetail(dataset, trace)?.spans.find((row) => row.span.span_id === span);
  const detail = fromExtra ? spanDetailFrom(fromExtra.span) : dataset.spanDetail(trace, span);
  if (!detail) {
    return problem(404, "not_found", "Not Found");
  }
  return HttpResponse.json(detail);
}

function listSessions(dataset: Dataset, request: Request): Response {
  const parsed = readQuery(new URL(request.url));
  if (parsed instanceof Response) {
    return parsed;
  }
  const rows = dataset
    .sessions(parsed.includeEval, parsed.includeExperiments)
    .filter((session) => inRange(session.start_time, parsed.from, parsed.to))
    .sort((left, right) => compareSessions(left, right, parsed.sort, parsed.order));
  return page(rows, parsed, ignoredFilter(parsed));
}

function oneSession(
  dataset: Dataset,
  request: Request,
  sessionId: string | readonly string[] | undefined,
): Response {
  const parsed = readQuery(new URL(request.url));
  if (parsed instanceof Response) {
    return parsed;
  }
  const detail = dataset.session(text(sessionId), parsed.includeEval, parsed.includeExperiments);
  if (!detail) {
    return problem(404, "not_found", "Not Found");
  }
  return HttpResponse.json(detail);
}

function listUsers(dataset: Dataset, request: Request): Response {
  const parsed = readQuery(new URL(request.url));
  if (parsed instanceof Response) {
    return parsed;
  }
  const rows = dataset
    .users(parsed.includeEval, parsed.includeExperiments)
    .filter((user) => overlaps(user.first_seen, user.last_seen, parsed.from, parsed.to))
    .sort((left, right) => compareUsers(left, right, parsed.sort, parsed.order));
  return page(rows, parsed, ignoredFilter(parsed));
}

function oneUser(
  dataset: Dataset,
  request: Request,
  userId: string | readonly string[] | undefined,
): Response {
  const parsed = readQuery(new URL(request.url));
  if (parsed instanceof Response) {
    return parsed;
  }
  const detail = dataset.user(text(userId), parsed.includeEval, parsed.includeExperiments);
  if (!detail) {
    return problem(404, "not_found", "Not Found");
  }
  return HttpResponse.json(detail);
}

function metrics(dataset: Dataset, request: Request): Response {
  const url = new URL(request.url);
  const parsed = readQuery(url);
  if (parsed instanceof Response) {
    return parsed;
  }
  if (parsed.filter === "bad") {
    return invalidFilter();
  }
  const from = parsed.from;
  const to = parsed.to;
  if (from === undefined || to === undefined) {
    return problem(400, "bad_request", "Bad Request");
  }
  const body = dataset.metrics(
    from,
    to,
    bucketOf(url.searchParams.get("bucket")),
    parsed.includeEval,
    parsed.includeExperiments,
  );
  return HttpResponse.json(body, { headers: ignoredFilter(parsed) });
}

type Query = {
  from?: number;
  to?: number;
  sort: string;
  order: Order;
  limit: number;
  offset: number;
  includeEval: boolean;
  includeExperiments: boolean;
  search?: string;
  filter?: string;
};

function readQuery(url: URL): Query | Response {
  const params = url.searchParams;
  if (params.get("filter") === "bad") {
    return invalidFilter();
  }
  const limit = readLimit(params.get("limit"));
  if (limit instanceof Response) {
    return limit;
  }
  const offset = readCursor(params.get("cursor"));
  if (offset instanceof Response) {
    return offset;
  }
  const from = readTime(params.get("from"));
  const to = readTime(params.get("to"));
  if (from instanceof Response || to instanceof Response) {
    return problem(400, "bad_request", "Bad Request");
  }
  const order = params.get("order") ?? "desc";
  if (order !== "asc" && order !== "desc") {
    return problem(400, "bad_request", "Bad Request");
  }
  return {
    ...(from === undefined ? {} : { from }),
    ...(to === undefined ? {} : { to }),
    sort: params.get("sort") ?? "start_time",
    order,
    limit,
    offset,
    includeEval: flag(params.get("include_eval")),
    includeExperiments: flag(params.get("include_experiments")),
    ...(params.get("search") ? { search: params.get("search") ?? "" } : {}),
    ...(params.get("filter") ? { filter: params.get("filter") ?? "" } : {}),
  };
}

function filterTraces(
  traces: readonly Schema<"TraceSummary">[],
  query: Query,
): Schema<"TraceSummary">[] {
  return visibleTraces(traces, query.includeEval, query.includeExperiments).filter((trace) => {
    if (!inRange(trace.start_time, query.from, query.to)) {
      return false;
    }
    if (!query.search) {
      return true;
    }
    const needle = query.search.toLowerCase();
    return [trace.name, trace.input_preview, trace.output_preview].some((value) =>
      value?.toLowerCase().includes(needle),
    );
  });
}

function filterSpans(
  spans: readonly Schema<"SpanSummary">[],
  query: Query,
): Schema<"SpanSummary">[] {
  return spans.filter((span) => {
    if (!query.includeEval && span.environment === "llmobs-eval") {
      return false;
    }
    if (!query.includeExperiments && span.metadata["llmobs.experiment_id"] !== undefined) {
      return false;
    }
    if (!inRange(span.start_time, query.from, query.to)) {
      return false;
    }
    if (!query.search) {
      return true;
    }
    const needle = query.search.toLowerCase();
    return [span.name, span.input_preview, span.output_preview].some((value) =>
      value?.toLowerCase().includes(needle),
    );
  });
}

function pageSpans(
  dataset: Dataset,
  traces: readonly Schema<"TraceSummary">[],
  query: Query,
): Response {
  const data: Schema<"SpanSummary">[] = [];
  let index = 0;
  let hasMore = false;
  for (const trace of traces) {
    for (const span of filterSpans(spansOf(dataset, trace.trace_id, query.order), query)) {
      if (index >= query.offset + query.limit) {
        hasMore = true;
        break;
      }
      if (index >= query.offset) {
        data.push(span);
      }
      index += 1;
    }
    if (hasMore) {
      break;
    }
  }
  return HttpResponse.json(
    { data, next_cursor: hasMore ? encodeCursor(query.offset + query.limit) : null },
    { headers: ignoredFilter(query) },
  );
}

function spansOf(dataset: Dataset, traceId: string, order: Order): Schema<"SpanSummary">[] {
  const spans = (dataset.traceDetail(traceId)?.spans ?? [])
    .map((row) => row.span)
    .sort((left, right) => left.start_time.localeCompare(right.start_time));
  return order === "desc" ? spans.reverse() : spans;
}

function page<T>(rows: readonly T[], query: Query, headers: HeadersInit): Response {
  const data = rows.slice(query.offset, query.offset + query.limit);
  const next =
    query.offset + query.limit < rows.length ? encodeCursor(query.offset + query.limit) : null;
  return HttpResponse.json({ data, next_cursor: next }, { headers });
}

function sortTraces(
  traces: readonly Schema<"TraceSummary">[],
  sort: string,
  order: Order,
): Schema<"TraceSummary">[] {
  const direction = order === "asc" ? 1 : -1;
  return [...traces].sort((left, right) => {
    const diff = traceKey(left, sort) - traceKey(right, sort);
    return diff === 0 ? left.trace_id.localeCompare(right.trace_id) * direction : diff * direction;
  });
}

function sortSpans(
  spans: readonly Schema<"SpanSummary">[],
  sort: string,
  order: Order,
): Schema<"SpanSummary">[] {
  const direction = order === "asc" ? 1 : -1;
  return [...spans].sort((left, right) => (spanKey(left, sort) - spanKey(right, sort)) * direction);
}

function traceKey(trace: Schema<"TraceSummary">, sort: string): number {
  if (sort === "duration") {
    return trace.duration_ns;
  }
  if (sort === "cost_total") {
    return trace.cost_total ?? 0;
  }
  if (sort === "total_tokens") {
    return trace.total_tokens ?? 0;
  }
  return Date.parse(trace.start_time);
}

function spanKey(span: Schema<"SpanSummary">, sort: string): number {
  if (sort === "duration") {
    return span.duration_ns;
  }
  if (sort === "cost_total") {
    return span.cost_total ?? 0;
  }
  if (sort === "total_tokens") {
    return span.total_tokens ?? 0;
  }
  return Date.parse(span.start_time);
}

function compareSessions(
  left: Schema<"SessionSummary">,
  right: Schema<"SessionSummary">,
  sort: string,
  order: Order,
): number {
  const direction = order === "asc" ? 1 : -1;
  const key = (session: Schema<"SessionSummary">) => {
    if (sort === "end_time") {
      return Date.parse(session.end_time);
    }
    if (sort === "trace_count") {
      return session.trace_count;
    }
    if (sort === "cost_total") {
      return session.cost_total ?? 0;
    }
    return Date.parse(session.start_time);
  };
  return (key(left) - key(right)) * direction;
}

function compareUsers(
  left: Schema<"EndUserSummary">,
  right: Schema<"EndUserSummary">,
  sort: string,
  order: Order,
): number {
  const direction = order === "asc" ? 1 : -1;
  const key = (user: Schema<"EndUserSummary">) => {
    if (sort === "trace_count") {
      return user.trace_count;
    }
    if (sort === "cost_total") {
      return user.cost_total ?? 0;
    }
    return Date.parse(user.last_seen);
  };
  return (key(left) - key(right)) * direction;
}

function inRange(start: string, from: number | undefined, to: number | undefined): boolean {
  const time = Date.parse(start);
  if (from !== undefined && time < from) {
    return false;
  }
  if (to !== undefined && time >= to) {
    return false;
  }
  return true;
}

function overlaps(
  first: string,
  last: string,
  from: number | undefined,
  to: number | undefined,
): boolean {
  const start = Date.parse(first);
  const end = Date.parse(last);
  if (from !== undefined && end < from) {
    return false;
  }
  if (to !== undefined && start >= to) {
    return false;
  }
  return true;
}

function readLimit(value: string | null): number | Response {
  if (value === null) {
    return 50;
  }
  if (!/^\d+$/.test(value)) {
    return problem(400, "bad_request", "Bad Request");
  }
  const limit = Number(value);
  if (limit < 1 || limit > 1000) {
    return problem(400, "bad_request", "Bad Request");
  }
  return limit;
}

function readCursor(value: string | null): number | Response {
  if (value === null) {
    return 0;
  }
  try {
    const text = atob(value);
    if (!/^\d+$/.test(text)) {
      return problem(400, "bad_request", "Bad Request");
    }
    return Number(text);
  } catch {
    return problem(400, "bad_request", "Bad Request");
  }
}

function readTime(value: string | null): number | undefined | Response {
  if (value === null) {
    return undefined;
  }
  if (/^\d+$/.test(value)) {
    return Number(value);
  }
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) {
    return problem(400, "bad_request", "Bad Request");
  }
  return parsed;
}

function encodeCursor(offset: number): string {
  return btoa(String(offset));
}

function flag(value: string | null): boolean {
  return value === "true" || value === "1";
}

function bucketOf(value: string | null): Schema<"MetricsBucket"> | undefined {
  const allowed = ["auto", "1m", "5m", "10m", "15m", "30m", "1h", "6h", "12h", "1d"];
  if (value !== null && allowed.includes(value)) {
    return value as Schema<"MetricsBucket">;
  }
  return undefined;
}

function ignoredFilter(query: Query): HeadersInit {
  if (!query.filter) {
    return {};
  }
  return { "X-Mock-Filter-Ignored": "1" };
}

function invalidFilter(): Response {
  return problem(400, "invalid_filter", "Bad Request", [
    { field: "filter", message: "unknown column bad", position: { line: 1, column: 1 } },
  ]);
}

function problem(
  status: number,
  code: Schema<"ProblemCode">,
  title: string,
  errors?: Schema<"FieldError">[],
): Response {
  return HttpResponse.json(
    {
      type: "about:blank",
      title,
      status,
      code,
      ...(errors ? { errors } : {}),
    },
    { status, headers: { "Content-Type": "application/problem+json" } },
  );
}

function text(value: string | readonly string[] | undefined): string {
  return typeof value === "string" ? value : "";
}

/** A trace is recent when it ended within the last 10 minutes. */
const RECENT_MS = 10 * 60 * 1000;
const PING_INTERVAL_MS = 15_000;

const extras = new WeakMap<Dataset, Map<string, Schema<"TraceDetail">>>();

type TraceLoop = {
  listeners: Set<StreamEmit>;
  timer: ReturnType<typeof setTimeout> | undefined;
  seq: number;
};

const traceLoops = new WeakMap<Dataset, TraceLoop>();

type SpanJob = {
  listeners: Set<StreamEmit>;
  timers: ReturnType<typeof setTimeout>[];
};

const spanJobs = new WeakMap<Dataset, Map<string, SpanJob>>();

let nextLiveSpan = 0;

export function liveTraceSource(dataset: Dataset): TraceStreamSource {
  return (emit) => {
    let loop = traceLoops.get(dataset);
    if (!loop) {
      loop = { listeners: new Set(), timer: undefined, seq: 1 };
      traceLoops.set(dataset, loop);
      scheduleCreate(dataset, loop);
    }
    loop.listeners.add(emit);
    return () => {
      const current = traceLoops.get(dataset);
      if (!current) {
        return;
      }
      current.listeners.delete(emit);
      if (current.listeners.size === 0) {
        if (current.timer !== undefined) {
          clearTimeout(current.timer);
        }
        traceLoops.delete(dataset);
      }
    };
  };
}

export function liveSpanSource(dataset: Dataset): SpanStreamSource {
  return (emit, traceId) => {
    const ping = setInterval(() => {
      emit.ping();
    }, PING_INTERVAL_MS);
    const stopSpans = watchSpans(dataset, traceId, emit);
    return () => {
      clearInterval(ping);
      stopSpans();
    };
  };
}

function scheduleCreate(dataset: Dataset, loop: TraceLoop): void {
  loop.timer = setTimeout(() => {
    if (traceLoops.get(dataset) !== loop) {
      return;
    }
    const created = addLiveTrace(dataset, loop.seq, Date.now());
    loop.seq += 1;
    emitTrace(loop, created);
    loop.timer = setTimeout(() => {
      if (traceLoops.get(dataset) !== loop) {
        return;
      }
      const updated = updateLiveTrace(dataset, created.trace_id, Date.now());
      if (updated) {
        emitTrace(loop, updated);
      }
      scheduleCreate(dataset, loop);
    }, 1_000);
  }, 2_000);
}

function emitTrace(loop: TraceLoop, trace: Schema<"TraceSummary">): void {
  for (const listener of loop.listeners) {
    listener.trace(trace);
  }
}

function watchSpans(dataset: Dataset, traceId: string, emit: StreamEmit): () => void {
  const trace = dataset.traces.find((row) => row.trace_id === traceId);
  if (!trace?.root_span_id || !isRecent(trace)) {
    return () => {};
  }
  let jobs = spanJobs.get(dataset);
  if (!jobs) {
    jobs = new Map();
    spanJobs.set(dataset, jobs);
  }
  let job = jobs.get(traceId);
  if (!job) {
    job = { listeners: new Set(), timers: [] };
    jobs.set(traceId, job);
    for (let index = 1; index <= 3; index += 1) {
      const timer = setTimeout(() => {
        const current = spanJobs.get(dataset)?.get(traceId);
        if (!current) {
          return;
        }
        const span = addChildSpan(dataset, trace, Date.now());
        for (const listener of current.listeners) {
          listener.span(span);
        }
      }, index * 1_000);
      job.timers.push(timer);
    }
  }
  job.listeners.add(emit);
  return () => {
    const current = spanJobs.get(dataset)?.get(traceId);
    if (!current) {
      return;
    }
    current.listeners.delete(emit);
    if (current.listeners.size === 0) {
      for (const timer of current.timers) {
        clearTimeout(timer);
      }
      spanJobs.get(dataset)?.delete(traceId);
    }
  };
}

function addLiveTrace(dataset: Dataset, seq: number, now: number): Schema<"TraceSummary"> {
  const template = dataset.traces[0];
  const start = rfc3339(now);
  const rootId = liveSpanId();
  const trace: Schema<"TraceSummary"> = {
    trace_id: `c${seq.toString(16).padStart(31, "0")}`,
    root_span_id: rootId,
    has_root: true,
    name: "live-trace",
    start_time: start,
    end_time: start,
    duration_ns: 0,
    status: "ok",
    span_count: 1,
    error_count: 0,
    llm_count: 0,
    metadata: {},
    models: [],
    tags: [],
    scores: [],
    input_preview: "Live trace",
    output_preview: "In progress",
    ...(template?.environment ? { environment: template.environment } : {}),
    ...(template?.release ? { release: template.release } : {}),
  };
  dataset.traces.unshift(trace);
  remember(dataset, {
    trace,
    spans: [{ depth: 0, detached: false, span: rootSpan(trace, now) }],
    truncated: false,
  });
  return trace;
}

function updateLiveTrace(
  dataset: Dataset,
  traceId: string,
  now: number,
): Schema<"TraceSummary"> | undefined {
  const trace = dataset.traces.find((row) => row.trace_id === traceId);
  if (!trace) {
    return undefined;
  }
  const startMs = Date.parse(trace.start_time);
  trace.end_time = rfc3339(now);
  trace.duration_ns = Number.isFinite(startMs) ? Math.max(0, now - startMs) * 1_000_000 : 0;
  trace.span_count += 1;
  return trace;
}

function addChildSpan(
  dataset: Dataset,
  trace: Schema<"TraceSummary">,
  now: number,
): Schema<"SpanSummary"> {
  const time = rfc3339(now);
  const span: Schema<"SpanSummary"> = {
    span_id: liveSpanId(),
    trace_id: trace.trace_id,
    ...(trace.root_span_id ? { parent_span_id: trace.root_span_id } : {}),
    span_kind: "span",
    name: "live span",
    start_time: time,
    end_time: time,
    duration_ns: 0,
    status: "ok",
    metadata: {},
    tags: [],
  };
  const detail = extraDetail(dataset, trace.trace_id) ?? dataset.traceDetail(trace.trace_id);
  if (detail) {
    detail.spans.push({ depth: 1, detached: false, span });
    detail.trace.span_count += 1;
  }
  return span;
}

function rootSpan(trace: Schema<"TraceSummary">, now: number): Schema<"SpanSummary"> {
  const time = rfc3339(now);
  return {
    span_id: trace.root_span_id ?? liveSpanId(),
    trace_id: trace.trace_id,
    span_kind: "agent",
    name: trace.name,
    start_time: time,
    end_time: time,
    duration_ns: 0,
    status: "ok",
    metadata: {},
    tags: [],
  };
}

function isRecent(trace: Schema<"TraceSummary">): boolean {
  const end = Date.parse(trace.end_time);
  return Number.isFinite(end) && Date.now() - end <= RECENT_MS;
}

function extraDetail(dataset: Dataset, traceId: string): Schema<"TraceDetail"> | undefined {
  return extras.get(dataset)?.get(traceId);
}

function remember(dataset: Dataset, detail: Schema<"TraceDetail">): void {
  let map = extras.get(dataset);
  if (!map) {
    map = new Map();
    extras.set(dataset, map);
  }
  map.set(detail.trace.trace_id, detail);
}

function spanDetailFrom(span: Schema<"SpanSummary">): Schema<"SpanDetail"> {
  return {
    ...span,
    attributes: {},
    conventions: [],
    events: [],
    ingest_time: span.end_time,
    links: [],
    media_refs: [],
    resource_attributes: {},
    schema_version: 1,
    truncated: [],
    scope_name: "llmobs",
  };
}

function liveSpanId(): string {
  nextLiveSpan += 1;
  return `e${nextLiveSpan.toString(16).padStart(15, "0")}`;
}

function rfc3339(ms: number): string {
  return new Date(ms)
    .toISOString()
    .replace(/\.(\d{3})Z$/, (_match, fraction: string) => `.${fraction}000000Z`);
}
