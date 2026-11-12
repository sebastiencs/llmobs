import { expect, it } from "vitest";
import type { MetricSeries } from "@/lib/metrics";
import { foldGroups, kpiDelta, seriesByMetric, toChartRows } from "@/lib/metrics";

const T0 = "2026-01-01T10:00:00.000000000Z";
const T1 = "2026-01-01T10:10:00.000000000Z";
const T2 = "2026-01-01T10:20:00.000000000Z";
const BUCKET_NS = 600_000_000_000;

function series(
  group: string | undefined,
  points: MetricSeries["points"],
  metric: MetricSeries["metric"] = "traces",
): MetricSeries {
  return { metric, ...(group ? { group } : {}), points };
}

it("keeps a missing point as null and fills a skipped bucket", () => {
  const rows = toChartRows(
    [
      series(
        undefined,
        [
          { time: T0, value: 0.2 },
          { time: T2, value: 0 },
        ],
        "error_rate",
      ),
    ],
    BUCKET_NS,
  );
  const start = Date.parse("2026-01-01T10:00:00.000Z");
  expect(rows).toEqual([
    { time: start, all: 0.2 },
    { time: start + 600_000, all: null },
    { time: start + 1_200_000, all: 0 },
  ]);
});

it("uses one column per group", () => {
  const rows = toChartRows(
    [
      series("prod", [{ time: T0, value: 2 }, { time: T1 }]),
      series("dev", [{ time: T0 }, { time: T1, value: 4 }]),
    ],
    BUCKET_NS,
  );
  const start = Date.parse(T0);
  expect(rows[0]).toEqual({ time: start, prod: 2, dev: null });
  expect(rows[1]).toEqual({ time: start + 600_000, prod: null, dev: 4 });
});

it("folds groups past the limit into other and keeps a gap when every folded series lacks a value", () => {
  const kept = Array.from({ length: 8 }, (_, index) =>
    series(`g${index}`, [{ time: T0, value: 100 - index }, { time: T1 }]),
  );
  const folded = foldGroups([
    ...kept,
    series("low-a", [{ time: T0, value: 1 }, { time: T1 }]),
    series("low-b", [
      { time: T0, value: 2 },
      { time: T1, value: 3 },
    ]),
  ]);
  expect(folded).toHaveLength(9);
  expect(folded.map((item) => item.group).slice(0, 8)).toEqual([
    "g0",
    "g1",
    "g2",
    "g3",
    "g4",
    "g5",
    "g6",
    "g7",
  ]);
  const other = folded[8];
  expect(other?.group).toBe("other");
  expect(other?.points).toEqual([
    { time: T0, value: 3 },
    { time: T1, value: 3 },
  ]);

  const bothMissing = foldGroups([
    ...kept,
    series("low-a", [{ time: T0, value: 1 }, { time: T1 }]),
    series("low-b", [{ time: T0, value: 2 }, { time: T1 }]),
  ]);
  expect(bothMissing[8]?.points).toEqual([{ time: T0, value: 3 }, { time: T1 }]);
});

it("groups series by metric", () => {
  const grouped = seriesByMetric({
    series: [
      series("a", [], "cost_by_model"),
      series(undefined, [], "traces"),
      series("b", [], "cost_by_model"),
    ],
  });
  expect(grouped.get("cost_by_model")?.map((item) => item.group)).toEqual(["a", "b"]);
  expect(grouped.get("traces")).toHaveLength(1);
  expect(grouped.has("errors")).toBe(false);
});

it("reports kpi direction and leaves the percentage undefined when the previous value is 0", () => {
  expect(kpiDelta({ value: 10, previous: 5 })).toEqual({
    value: 10,
    previous: 5,
    changePct: 100,
    direction: "up",
  });
  expect(kpiDelta({ value: 4, previous: 8 })).toEqual({
    value: 4,
    previous: 8,
    changePct: -50,
    direction: "down",
  });
  expect(kpiDelta({ value: 3, previous: 3 })).toMatchObject({ direction: "flat", changePct: 0 });
  expect(kpiDelta({ value: 5, previous: 0 })).toEqual({
    value: 5,
    previous: 0,
    direction: "up",
  });
  expect(kpiDelta({ value: 0, previous: 0 })).toEqual({
    value: 0,
    previous: 0,
    direction: "flat",
  });
  expect(kpiDelta({ value: 5 })).toEqual({ value: 5, direction: "flat" });
  expect(kpiDelta({ previous: 5 })).toEqual({ previous: 5, direction: "flat" });
});
