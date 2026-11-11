import type { Schema } from "@/api/types";
import { mulberry32 } from "@/mocks/generated/random";

const DAY_MS = 86_400_000;
const NAMES = [
  "support-router",
  "refund-agent",
  "ticket-summary",
  "billing-lookup",
  "handoff",
  "retrieval",
  "guardrail-check",
  "draft-reply",
  "escalate",
  "close-ticket",
  "qa-review",
  "memory-update",
] as const;
const MODELS = [
  "claude-sonnet-4-5",
  "claude-haiku-4-5",
  "gpt-4.1",
  "gpt-4.1-mini",
  "gemini-2.5-pro",
  "gemini-2.5-flash",
] as const;
const TAGS = ["billing", "priority", "voice", "email", "retry"] as const;
const KINDS = [
  "llm",
  "tool",
  "chain",
  "llm",
  "retriever",
  "span",
  "embedding",
  "guardrail",
  "evaluator",
  "reranker",
] as const satisfies readonly Schema<"SpanKind">[];
const BUCKET_MS: Record<Exclude<Schema<"MetricsBucket">, "auto">, number> = {
  "1m": 60_000,
  "5m": 300_000,
  "10m": 600_000,
  "15m": 900_000,
  "30m": 1_800_000,
  "1h": 3_600_000,
  "6h": 21_600_000,
  "12h": 43_200_000,
  "1d": DAY_MS,
};

type BuiltSpan = {
  depth: number;
  detached: boolean;
  parent?: number;
  startMs: number;
  endMs: number;
  kind: Schema<"SpanKind">;
  status: Schema<"SpanStatus">;
};

export type Dataset = {
  traces: Schema<"TraceSummary">[];
  traceDetail(traceId: string): Schema<"TraceDetail"> | undefined;
  spanDetail(traceId: string, spanId: string): Schema<"SpanDetail"> | undefined;
  sessions(includeEval: boolean, includeExperiments: boolean): Schema<"SessionSummary">[];
  session(
    sessionId: string,
    includeEval: boolean,
    includeExperiments: boolean,
  ): Schema<"SessionDetail"> | undefined;
  users(includeEval: boolean, includeExperiments: boolean): Schema<"EndUserSummary">[];
  user(
    userId: string,
    includeEval: boolean,
    includeExperiments: boolean,
  ): Schema<"EndUserDetail"> | undefined;
  metrics(
    from: number,
    to: number,
    bucket: Schema<"MetricsBucket"> | undefined,
    includeEval: boolean,
    includeExperiments: boolean,
  ): Schema<"MetricsResponse">;
};

export function createDataset(options: { seed: number; now: number; traces: number }): Dataset {
  const traces: Schema<"TraceSummary">[] = [];
  const indexById = new Map<string, number>();
  for (let index = 0; index < options.traces; index += 1) {
    const trace = traceSummary(options.seed, index, options.now);
    traces.push(trace);
    indexById.set(trace.trace_id, index);
  }
  const details = new Map<string, Schema<"TraceDetail">>();

  function traceDetail(traceId: string): Schema<"TraceDetail"> | undefined {
    const index = indexById.get(traceId);
    const trace = index === undefined ? undefined : traces[index];
    if (index === undefined || !trace) {
      return undefined;
    }
    const cached = details.get(traceId);
    if (cached) {
      return cached;
    }
    const detail = buildDetail(options.seed, index, trace);
    details.set(traceId, detail);
    return detail;
  }

  return {
    traces,
    traceDetail,
    spanDetail(traceId, spanId) {
      const detail = traceDetail(traceId);
      const row = detail?.spans.find((span) => span.span.span_id === spanId);
      if (!detail || !row) {
        return undefined;
      }
      return spanFromSummary(row.span);
    },
    sessions(includeEval, includeExperiments) {
      return sessionsFrom(visibleTraces(traces, includeEval, includeExperiments));
    },
    session(sessionId, includeEval, includeExperiments) {
      const rows = visibleTraces(traces, includeEval, includeExperiments)
        .filter((trace) => trace.session_id === sessionId)
        .sort((left, right) => left.start_time.localeCompare(right.start_time));
      const session = sessionsFrom(rows).find((item) => item.session_id === sessionId);
      if (!session) {
        return undefined;
      }
      const turns = rows.slice(0, 1000).map((trace) => ({
        trace,
        input: textPayload(trace.input_preview ?? ""),
        output: textPayload(trace.output_preview ?? ""),
      }));
      return { session, turns, truncated: rows.length > 1000 };
    },
    users(includeEval, includeExperiments) {
      return usersFrom(visibleTraces(traces, includeEval, includeExperiments));
    },
    user(userId, includeEval, includeExperiments) {
      const rows = visibleTraces(traces, includeEval, includeExperiments).filter(
        (trace) => trace.user_id === userId,
      );
      const user = usersFrom(rows).find((item) => item.user_id === userId);
      if (!user) {
        return undefined;
      }
      const sessions = sessionsFrom(rows).sort((left, right) =>
        right.end_time.localeCompare(left.end_time),
      );
      return { user, sessions: sessions.slice(0, 100) };
    },
    metrics(from, to, bucket, includeEval, includeExperiments) {
      return metricsFrom(
        visibleTraces(traces, includeEval, includeExperiments),
        traces.filter((trace) => trace.environment === "llmobs-eval"),
        from,
        to,
        bucket,
      );
    },
  };
}

export function visibleTraces(
  traces: readonly Schema<"TraceSummary">[],
  includeEval: boolean,
  includeExperiments: boolean,
): Schema<"TraceSummary">[] {
  return traces.filter((trace) => {
    if (!includeEval && trace.environment === "llmobs-eval") {
      return false;
    }
    if (!includeExperiments && trace.metadata["llmobs.experiment_id"] !== undefined) {
      return false;
    }
    return true;
  });
}

function traceSummary(seed: number, index: number, now: number): Schema<"TraceSummary"> {
  const rng = mulberry32(mix(seed, index));
  const spanCount = index === 0 ? 10_000 : 3 + Math.floor(rng() * 58);
  const errored = rng() < 0.08;
  const errorCount = errored ? Math.min(spanCount, 1 + Math.floor(rng() * 3)) : 0;
  const environment = environmentOf(rng);
  const experiment = environment !== "llmobs-eval" && rng() < 0.04;
  const ageMs = ageOf(rng, index);
  const durationMs = index === 0 ? 60_000 : 200 + Math.floor(rng() * 20_000);
  const startMs = now - ageMs;
  const name = NAMES[Math.floor(rng() * NAMES.length)] ?? NAMES[0];
  const model = MODELS[Math.floor(rng() * MODELS.length)] ?? MODELS[0];
  const inputTokens = 100 + Math.floor(rng() * 1_900);
  const outputTokens = 20 + Math.floor(rng() * 400);
  const session =
    rng() < 0.5 ? `sess-${String(Math.floor(rng() * 120)).padStart(4, "0")}` : undefined;
  const user = rng() < 0.6 ? `user-${String(Math.floor(rng() * 40)).padStart(3, "0")}` : undefined;
  const tags = pickTags(rng);
  const metadata: Record<string, string> = {};
  if (experiment) {
    metadata["llmobs.experiment_id"] = `exp-${String(index % 12).padStart(2, "0")}`;
  }
  const traceId = `a${index.toString(16).padStart(31, "0")}`;
  return {
    trace_id: traceId,
    root_span_id: spanId(index, 0),
    has_root: true,
    name,
    start_time: rfc3339(startMs),
    end_time: rfc3339(startMs + durationMs),
    duration_ns: durationMs * 1_000_000,
    status: errorCount > 0 ? "error" : "ok",
    environment,
    release: index % 2 === 0 ? "v1.5.0" : "v1.4.0",
    tags,
    metadata,
    input_preview: `Ask ${name} about ticket ${index}`,
    output_preview: errorCount > 0 ? "The tool failed." : `Done with ticket ${index}.`,
    span_count: spanCount,
    error_count: errorCount,
    llm_count: llmCount(spanCount),
    input_tokens: inputTokens,
    output_tokens: outputTokens,
    total_tokens: inputTokens + outputTokens,
    cost_total: Math.round(inputTokens * 3 + outputTokens * 15) / 1_000_000,
    models: [model],
    scores: index % 2 === 0 ? scoresFor(index) : [],
    ...(session ? { session_id: session } : {}),
    ...(user ? { user_id: user } : {}),
  };
}

function buildDetail(
  seed: number,
  index: number,
  trace: Schema<"TraceSummary">,
): Schema<"TraceDetail"> {
  const rng = mulberry32(mix(seed, index + 17));
  const spanCount = trace.span_count;
  const maxDepth = index === 0 ? 8 : 6;
  const shape = spanShape(spanCount, maxDepth, index === 0 ? 2_000 : 0, rng);
  const startMs = Date.parse(trace.start_time);
  const endMs = Date.parse(trace.end_time);
  const built = layoutSpans(shape, startMs, endMs, trace.error_count, rng);
  const spans = built.map((span, spanIndex) => treeSpan(span, spanIndex, index, trace));
  return { trace, spans, truncated: false };
}

function spanShape(
  spanCount: number,
  maxDepth: number,
  detachedEvery: number,
  rng: () => number,
): { depth: number; detached: boolean }[] {
  const rows = [{ depth: 0, detached: false }];
  for (let index = 1; index < spanCount; index += 1) {
    if (detachedEvery > 0 && index % detachedEvery === 0) {
      rows.push({ depth: 0, detached: true });
      continue;
    }
    const previous = rows[index - 1]?.depth ?? 0;
    const ceiling = Math.min(maxDepth, previous + 1);
    const depth = rng() < 0.65 ? ceiling : 1 + Math.floor(rng() * Math.max(1, ceiling - 1));
    rows.push({ depth: Math.min(ceiling, Math.max(1, depth)), detached: false });
  }
  return rows;
}

function layoutSpans(
  shape: { depth: number; detached: boolean }[],
  startMs: number,
  endMs: number,
  errorCount: number,
  rng: () => number,
): BuiltSpan[] {
  const built: BuiltSpan[] = [];
  const stack: number[] = [];
  const lastChild = new Map<number, number>();
  for (let index = 0; index < shape.length; index += 1) {
    const row = shape[index];
    if (!row) {
      continue;
    }
    const kind = kindAt(index);
    const status = errorSpan(index, shape.length, errorCount) ? "error" : "ok";
    if (row.detached || row.depth === 0) {
      stack.length = 0;
      stack[0] = index;
      built.push({
        depth: 0,
        detached: row.detached,
        startMs,
        endMs,
        kind,
        status,
      });
      continue;
    }
    const parentIndex = stack[row.depth - 1] ?? 0;
    const parent = built[parentIndex];
    const previousIndex = lastChild.get(parentIndex);
    const previous = previousIndex === undefined ? undefined : built[previousIndex];
    const window = childWindow(parent ?? { startMs, endMs }, previous, kind, rng);
    stack.length = row.depth;
    stack[row.depth] = index;
    lastChild.set(parentIndex, index);
    built.push({
      depth: row.depth,
      detached: false,
      parent: parentIndex,
      startMs: window.startMs,
      endMs: window.endMs,
      kind,
      status,
    });
  }
  return built;
}

function childWindow(
  parent: { startMs: number; endMs: number },
  previous: BuiltSpan | undefined,
  kind: Schema<"SpanKind">,
  rng: () => number,
): { startMs: number; endMs: number } {
  const span = Math.max(1, parent.endMs - parent.startMs);
  let startMs = parent.startMs + Math.floor(rng() * span);
  let length = Math.max(1, Math.floor(rng() * Math.max(1, parent.endMs - startMs)));
  if (previous?.kind === "llm" && kind === "llm") {
    const previousSpan = Math.max(1, previous.endMs - previous.startMs);
    startMs = previous.startMs + Math.floor(previousSpan * 0.4);
    length = Math.max(1, Math.floor(previousSpan * 0.8));
  }
  if (startMs < parent.startMs || startMs >= parent.endMs) {
    startMs = parent.startMs;
  }
  let endMs = Math.min(parent.endMs, startMs + length);
  if (endMs <= startMs) {
    endMs = Math.min(parent.endMs, startMs + 1);
  }
  if (endMs <= startMs) {
    return { startMs: parent.startMs, endMs: parent.endMs };
  }
  return { startMs, endMs };
}

function treeSpan(
  span: BuiltSpan,
  spanIndex: number,
  traceIndex: number,
  trace: Schema<"TraceSummary">,
): Schema<"TreeSpan"> {
  const id = spanId(traceIndex, spanIndex);
  const summary: Schema<"SpanSummary"> = {
    span_id: id,
    trace_id: trace.trace_id,
    span_kind: span.kind,
    name: spanIndex === 0 ? trace.name : `${span.kind} ${spanIndex}`,
    start_time: rfc3339(span.startMs),
    end_time: rfc3339(span.endMs),
    duration_ns: (span.endMs - span.startMs) * 1_000_000,
    status: span.status,
    metadata: { ...trace.metadata },
    tags: trace.tags,
    ...(span.status === "error" ? { status_message: "tool failed" } : {}),
    ...(span.parent === undefined ? {} : { parent_span_id: spanId(traceIndex, span.parent) }),
    ...(trace.environment ? { environment: trace.environment } : {}),
    service_name: "support-bot",
    ...(trace.session_id ? { session_id: trace.session_id } : {}),
    ...(trace.user_id ? { user_id: trace.user_id } : {}),
    ...(trace.release ? { release: trace.release } : {}),
    ...(span.kind === "llm" && trace.models[0]
      ? { model: trace.models[0], provider: "anthropic" }
      : {}),
  };
  if (spanIndex === 0) {
    summary.input_preview = trace.input_preview;
    summary.output_preview = trace.output_preview;
    summary.input_tokens = trace.input_tokens;
    summary.output_tokens = trace.output_tokens;
    summary.total_tokens = trace.total_tokens;
    summary.cost_total = trace.cost_total;
  }
  return { depth: span.depth, detached: span.detached, span: summary };
}

function spanFromSummary(span: Schema<"SpanSummary">): Schema<"SpanDetail"> {
  const truncated = spanIndex(span.span_id) % 17 === 0 ? ["input"] : [];
  const detail: Schema<"SpanDetail"> = {
    ...span,
    attributes: { "gen_ai.operation.name": span.span_kind },
    conventions: ["genai"],
    events:
      span.status === "error"
        ? [
            {
              name: "exception",
              time: span.end_time,
              attributes: {
                "exception.stacktrace": "Error: tool failed\n    at lookup (agent.ts:12:5)",
              },
            },
          ]
        : [],
    ingest_time: span.end_time,
    links: [],
    media_refs: [],
    resource_attributes: { "host.name": "mock" },
    schema_version: 1,
    truncated,
    scope_name: "llmobs",
  };
  if (span.span_kind === "llm") {
    detail.input = {
      format: "messages",
      value: [
        { role: "system", parts: [{ type: "text", content: "You are a support agent." }] },
        {
          role: "user",
          parts: [{ type: "text", content: span.input_preview ?? "Hello" }],
        },
        {
          role: "assistant",
          parts: [{ type: "tool_call", id: "call-1", name: "lookup", arguments: { ticket: "1" } }],
        },
        {
          role: "tool",
          parts: [{ type: "tool_call_response", id: "call-1", response: { status: "open" } }],
        },
      ],
    };
    detail.output = {
      format: "messages",
      value: [
        {
          role: "assistant",
          parts: [{ type: "text", content: span.output_preview ?? "Done." }],
        },
      ],
    };
  } else if (span.span_kind === "tool") {
    detail.input = { format: "json", value: { ticket: "1" } };
    detail.output = { format: "json", value: { status: "open" } };
  } else {
    detail.input = { format: "text", value: span.input_preview ?? span.name };
    detail.output = { format: "text", value: span.output_preview ?? "ok" };
  }
  return detail;
}

function sessionsFrom(traces: readonly Schema<"TraceSummary">[]): Schema<"SessionSummary">[] {
  const groups = new Map<string, Schema<"TraceSummary">[]>();
  for (const trace of traces) {
    if (!trace.session_id) {
      continue;
    }
    const rows = groups.get(trace.session_id) ?? [];
    rows.push(trace);
    groups.set(trace.session_id, rows);
  }
  return [...groups.entries()].map(([sessionId, rows]) => {
    const ordered = [...rows].sort((left, right) =>
      left.start_time.localeCompare(right.start_time),
    );
    const start = Date.parse(ordered[0]?.start_time ?? "");
    const end = ordered.reduce(
      (latest, trace) => Math.max(latest, Date.parse(trace.end_time)),
      start,
    );
    const scores = new Map<string, Schema<"ScoreBrief">>();
    const tags = new Set<string>();
    let errors = 0;
    let cost = 0;
    let tokens = 0;
    let userId: string | undefined;
    for (const trace of ordered) {
      errors += trace.error_count;
      cost += trace.cost_total ?? 0;
      tokens += trace.total_tokens ?? 0;
      userId = trace.user_id ?? userId;
      for (const tag of trace.tags) {
        tags.add(tag);
      }
      for (const score of trace.scores) {
        scores.set(score.name, score);
      }
    }
    return {
      session_id: sessionId,
      start_time: rfc3339(start),
      end_time: rfc3339(end),
      duration_ns: Math.max(0, end - start) * 1_000_000,
      error_count: errors,
      trace_count: ordered.length,
      cost_total: cost,
      total_tokens: tokens,
      tags: [...tags].sort(),
      scores: [...scores.values()],
      ...(userId ? { user_id: userId } : {}),
    };
  });
}

function usersFrom(traces: readonly Schema<"TraceSummary">[]): Schema<"EndUserSummary">[] {
  const groups = new Map<string, Schema<"TraceSummary">[]>();
  for (const trace of traces) {
    if (!trace.user_id) {
      continue;
    }
    const rows = groups.get(trace.user_id) ?? [];
    rows.push(trace);
    groups.set(trace.user_id, rows);
  }
  return [...groups.entries()].map(([userId, rows]) => {
    const start = rows.reduce(
      (earliest, trace) => Math.min(earliest, Date.parse(trace.start_time)),
      Infinity,
    );
    const end = rows.reduce((latest, trace) => Math.max(latest, Date.parse(trace.end_time)), 0);
    const sessions = new Set(rows.flatMap((trace) => (trace.session_id ? [trace.session_id] : [])));
    return {
      user_id: userId,
      first_seen: rfc3339(start),
      last_seen: rfc3339(end),
      trace_count: rows.length,
      session_count: sessions.size,
      error_count: rows.reduce((sum, trace) => sum + trace.error_count, 0),
      cost_total: rows.reduce((sum, trace) => sum + (trace.cost_total ?? 0), 0),
      total_tokens: rows.reduce((sum, trace) => sum + (trace.total_tokens ?? 0), 0),
    };
  });
}

function metricsFrom(
  traces: readonly Schema<"TraceSummary">[],
  evalTraces: readonly Schema<"TraceSummary">[],
  from: number,
  to: number,
  bucket: Schema<"MetricsBucket"> | undefined,
): Schema<"MetricsResponse"> {
  const widthMs = bucketWidth(bucket, from, to);
  const times: number[] = [];
  for (let time = from; time < to; time += widthMs) {
    times.push(time);
  }
  const inRange = traces.filter((trace) => {
    const start = Date.parse(trace.start_time);
    return start >= from && start < to;
  });
  const buckets = times.map(() => [] as Schema<"TraceSummary">[]);
  for (const trace of inRange) {
    const slot = Math.floor((Date.parse(trace.start_time) - from) / widthMs);
    buckets[slot]?.push(trace);
  }
  const previousFrom = from - (to - from);
  const previous = traces.filter((trace) => {
    const start = Date.parse(trace.start_time);
    return start >= previousFrom && start < from;
  });
  const series: Schema<"MetricSeries">[] = [
    seriesOf("traces", times, buckets, (rows) => rows.length),
    seriesOf(
      "errors",
      times,
      buckets,
      (rows) => rows.filter((trace) => trace.status === "error").length,
    ),
    seriesOf("error_rate", times, buckets, (rows) =>
      rows.length === 0
        ? undefined
        : rows.filter((trace) => trace.status === "error").length / rows.length,
    ),
    seriesOf("latency_p50", times, buckets, (rows) =>
      percentile(
        rows.map((trace) => trace.duration_ns),
        50,
      ),
    ),
    seriesOf("latency_p95", times, buckets, (rows) =>
      percentile(
        rows.map((trace) => trace.duration_ns),
        95,
      ),
    ),
    seriesOf("latency_p99", times, buckets, (rows) =>
      percentile(
        rows.map((trace) => trace.duration_ns),
        99,
      ),
    ),
    seriesOf("cost", times, buckets, (rows) => sum(rows, (trace) => trace.cost_total ?? 0)),
    seriesOf("input_tokens", times, buckets, (rows) =>
      sum(rows, (trace) => trace.input_tokens ?? 0),
    ),
    seriesOf("output_tokens", times, buckets, (rows) =>
      sum(rows, (trace) => trace.output_tokens ?? 0),
    ),
    ...modelSeries(times, buckets),
    ...scoreSeries(times, buckets),
    seriesOf(
      "eval_cost",
      times,
      times.map((time) =>
        evalTraces.filter((trace) => {
          const start = Date.parse(trace.start_time);
          return start >= time && start < time + widthMs && start < to;
        }),
      ),
      (rows) => sum(rows, (trace) => trace.cost_total ?? 0),
    ),
  ];
  return {
    from: rfc3339(from),
    to: rfc3339(to),
    bucket_ns: widthMs * 1_000_000,
    kpis: kpis(inRange, previous),
    series,
    top_models: topModels(inRange),
  };
}

function seriesOf(
  metric: Schema<"MetricName">,
  times: readonly number[],
  buckets: readonly Schema<"TraceSummary">[][],
  value: (rows: readonly Schema<"TraceSummary">[]) => number | undefined,
  group?: string,
): Schema<"MetricSeries"> {
  return {
    metric,
    ...(group ? { group } : {}),
    points: times.map((time, index) => {
      const next = value(buckets[index] ?? []);
      return next === undefined ? { time: rfc3339(time) } : { time: rfc3339(time), value: next };
    }),
  };
}

function modelSeries(
  times: readonly number[],
  buckets: readonly Schema<"TraceSummary">[][],
): Schema<"MetricSeries">[] {
  const models = new Set<string>();
  for (const rows of buckets) {
    for (const trace of rows) {
      const model = trace.models[0];
      if (model) {
        models.add(model);
      }
    }
  }
  return [...models].sort().map((model) =>
    seriesOf(
      "cost_by_model",
      times,
      buckets,
      (rows) =>
        sum(
          rows.filter((trace) => trace.models[0] === model),
          (trace) => trace.cost_total ?? 0,
        ),
      model,
    ),
  );
}

function scoreSeries(
  times: readonly number[],
  buckets: readonly Schema<"TraceSummary">[][],
): Schema<"MetricSeries">[] {
  const names = new Set<string>();
  for (const rows of buckets) {
    for (const trace of rows) {
      for (const score of trace.scores) {
        names.add(score.name);
      }
    }
  }
  return [...names].sort().map((name) =>
    seriesOf(
      "score_avg",
      times,
      buckets,
      (rows) => {
        const values = rows.flatMap((trace) =>
          trace.scores.filter((score) => score.name === name).map(scoreNumber),
        );
        const numbers = values.filter((item): item is number => item !== undefined);
        if (numbers.length === 0) {
          return undefined;
        }
        return numbers.reduce((total, item) => total + item, 0) / numbers.length;
      },
      name,
    ),
  );
}

function kpis(
  current: readonly Schema<"TraceSummary">[],
  previous: readonly Schema<"TraceSummary">[],
): Schema<"Kpi">[] {
  const value = (rows: readonly Schema<"TraceSummary">[], metric: Schema<"KpiMetric">) => {
    if (rows.length === 0) {
      return undefined;
    }
    if (metric === "traces") {
      return rows.length;
    }
    if (metric === "error_rate") {
      return rows.filter((trace) => trace.status === "error").length / rows.length;
    }
    if (metric === "latency_p95") {
      return percentile(
        rows.map((trace) => trace.duration_ns),
        95,
      );
    }
    if (metric === "cost_total") {
      return sum(rows, (trace) => trace.cost_total ?? 0);
    }
    return sum(rows, (trace) => trace.total_tokens ?? 0);
  };
  const metrics: Schema<"KpiMetric">[] = [
    "traces",
    "error_rate",
    "latency_p95",
    "cost_total",
    "total_tokens",
  ];
  return metrics.map((metric) => {
    const currentValue = value(current, metric);
    const previousValue = value(previous, metric);
    return {
      metric,
      ...(currentValue === undefined ? {} : { value: currentValue }),
      ...(previousValue === undefined ? {} : { previous: previousValue }),
    };
  });
}

function topModels(traces: readonly Schema<"TraceSummary">[]): Schema<"ModelUsage">[] {
  const groups = new Map<string, { cost: number; tokens: number; llm: number }>();
  for (const trace of traces) {
    const model = trace.models[0];
    if (!model) {
      continue;
    }
    const row = groups.get(model) ?? { cost: 0, tokens: 0, llm: 0 };
    row.cost += trace.cost_total ?? 0;
    row.tokens += trace.total_tokens ?? 0;
    row.llm += trace.llm_count;
    groups.set(model, row);
  }
  return [...groups.entries()]
    .sort((left, right) => right[1].cost - left[1].cost)
    .slice(0, 10)
    .map(([model, row]) => ({
      model,
      llm_count: row.llm,
      cost_total: row.cost,
      total_tokens: row.tokens,
    }));
}

function scoreNumber(score: Schema<"ScoreBrief">): number | undefined {
  if (score.value.data_type === "numeric") {
    return score.value.value;
  }
  if (score.value.data_type === "boolean") {
    return score.value.value ? 1 : 0;
  }
  if (score.value.data_type === "categorical") {
    return score.value.value.value;
  }
  return undefined;
}

function percentile(values: readonly number[], p: number): number | undefined {
  if (values.length === 0) {
    return undefined;
  }
  const sorted = [...values].sort((left, right) => left - right);
  const rank = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[rank];
}

function sum(
  rows: readonly Schema<"TraceSummary">[],
  value: (trace: Schema<"TraceSummary">) => number,
): number {
  return rows.reduce((total, trace) => total + value(trace), 0);
}

function bucketWidth(
  bucket: Schema<"MetricsBucket"> | undefined,
  from: number,
  to: number,
): number {
  if (bucket && bucket !== "auto") {
    return BUCKET_MS[bucket];
  }
  const span = Math.max(1, to - from);
  const widths = Object.values(BUCKET_MS);
  return widths.find((width) => span / width <= 120) ?? DAY_MS;
}

function kindAt(index: number): Schema<"SpanKind"> {
  if (index === 0) {
    return "agent";
  }
  return KINDS[index % KINDS.length] ?? "span";
}

function llmCount(spanCount: number): number {
  let count = 0;
  for (let index = 0; index < spanCount; index += 1) {
    if (kindAt(index) === "llm") {
      count += 1;
    }
  }
  return count;
}

function errorSpan(index: number, spanCount: number, errorCount: number): boolean {
  if (errorCount <= 0) {
    return false;
  }
  if (spanCount === 1) {
    return true;
  }
  return index > 0 && index <= errorCount;
}

function environmentOf(rng: () => number): string {
  const roll = rng();
  if (roll < 0.03) {
    return "llmobs-eval";
  }
  if (roll < 0.28) {
    return "staging";
  }
  return "production";
}

function ageOf(rng: () => number, index: number): number {
  if (index === 0) {
    return 30_000;
  }
  if (rng() < 0.55) {
    return 60_000 + Math.floor(rng() * DAY_MS);
  }
  return DAY_MS + Math.floor(rng() * 29 * DAY_MS);
}

function pickTags(rng: () => number): string[] {
  const count = Math.floor(rng() * 3);
  const tags = new Set<string>();
  for (let index = 0; index < count; index += 1) {
    const tag = TAGS[Math.floor(rng() * TAGS.length)];
    if (tag) {
      tags.add(tag);
    }
  }
  return [...tags].sort();
}

function scoresFor(index: number): Schema<"ScoreBrief">[] {
  return [
    {
      name: "correctness",
      score_id: scoreUuid(index, 0),
      source: "llm_judge",
      value: { data_type: "numeric", value: ((index % 10) + 1) / 10 },
    },
    {
      name: "relevance",
      score_id: scoreUuid(index, 1),
      source: "human",
      value: { data_type: "boolean", value: index % 4 !== 0 },
    },
  ];
}

function scoreUuid(index: number, which: number): string {
  const tail = (index * 2 + which).toString(16).padStart(12, "0");
  return `00000000-0000-7000-8000-${tail}`;
}

function spanId(traceIndex: number, spanIndex: number): string {
  return `${traceIndex.toString(16).padStart(8, "0")}${(spanIndex + 1).toString(16).padStart(8, "0")}`;
}

function spanIndex(spanIdValue: string): number {
  return Number.parseInt(spanIdValue.slice(8), 16) - 1;
}

function textPayload(value: string): Schema<"Payload"> {
  return { format: "text", value };
}

function rfc3339(ms: number): string {
  return new Date(ms)
    .toISOString()
    .replace(/\.(\d{3})Z$/, (_match, fraction: string) => `.${fraction}000000Z`);
}

function mix(seed: number, index: number): number {
  return Math.imul(seed ^ index, 0x9e3779b1) >>> 0;
}
