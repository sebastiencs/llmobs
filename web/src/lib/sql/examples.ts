export type SqlExample = { name: string; query: string };

export const SQL_EXAMPLES: readonly SqlExample[] = [
  {
    name: "Cost per model per day",
    query: [
      "SELECT date_trunc('day', start_time) AS day, model, sum(cost_total) AS cost",
      "FROM spans",
      "WHERE span_kind = 'llm' AND model IS NOT NULL",
      "GROUP BY day, model",
      "ORDER BY day, cost DESC",
    ].join("\n"),
  },
  {
    name: "LLM latency percentiles per model",
    query: [
      "SELECT model,",
      "  approx_percentile_cont(duration_ms, 0.5) AS p50_ms,",
      "  approx_percentile_cont(duration_ms, 0.95) AS p95_ms,",
      "  approx_percentile_cont(duration_ms, 0.99) AS p99_ms,",
      "  count(*) AS calls",
      "FROM spans",
      "WHERE span_kind = 'llm' AND model IS NOT NULL",
      "GROUP BY model",
      "ORDER BY calls DESC",
    ].join("\n"),
  },
  {
    name: "Traces with the most tool calls",
    query: [
      "SELECT trace_id, count(*) AS tool_calls",
      "FROM spans",
      "WHERE span_kind = 'tool'",
      "GROUP BY trace_id",
      "ORDER BY tool_calls DESC",
      "LIMIT 50",
    ].join("\n"),
  },
  {
    name: "Average judge score per release",
    query: [
      "SELECT t.release, s.name, avg(s.value_num) AS avg_score, count(*) AS scores",
      "FROM scores s",
      "JOIN traces t ON s.trace_id = t.trace_id",
      "WHERE s.source = 'llm_judge' AND s.value_num IS NOT NULL",
      "GROUP BY t.release, s.name",
      "ORDER BY t.release, s.name",
    ].join("\n"),
  },
  {
    name: "Users by spend",
    query: [
      "SELECT user_id, count(*) AS traces, sum(cost_total) AS cost",
      "FROM traces",
      "WHERE user_id IS NOT NULL",
      "GROUP BY user_id",
      "ORDER BY cost DESC",
      "LIMIT 50",
    ].join("\n"),
  },
];
