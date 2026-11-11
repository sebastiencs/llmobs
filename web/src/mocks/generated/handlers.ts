import { type HttpHandler, HttpResponse, http } from "msw";
import type { Schema } from "@/api/types";
import { type Dataset, visibleTraces } from "@/mocks/generated/dataset";

const PROJECT = "/api/v1/projects/:project";

type Order = Schema<"SortOrder">;

export function largeHandlers(dataset: Dataset): HttpHandler[] {
  return [
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
  const detail = dataset.traceDetail(text(traceId));
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
  const detail = dataset.spanDetail(text(traceId), text(spanId));
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
