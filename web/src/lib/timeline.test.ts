import { expect, it } from "vitest";
import { formatDuration } from "@/lib/format";
import {
  barGeometry,
  clampView,
  panBy,
  ticks,
  traceWindow,
  zoomAt,
  zoomToSpan,
} from "@/lib/timeline";

const START = "2026-01-01T00:00:00.000000000Z";
const ONE_SECOND = "2026-01-01T00:00:01.000000000Z";
const HALF = "2026-01-01T00:00:00.500000000Z";
const BEFORE = "2025-12-31T23:59:59.000000000Z";
const AFTER = "2026-01-01T00:00:02.000000000Z";

it("widens a zero-width trace to one microsecond and includes span extremes", () => {
  const point = traceWindow({ start_time: START, end_time: START }, []);
  // instantNs is a JS number. Around 2026 its step is 256 ns, so a 1 µs pad
  // lands on the next representable instant rather than exactly 1 000 ns.
  expect(point.endNs - point.startNs).toBeGreaterThanOrEqual(1_000);
  expect(point.endNs - point.startNs).toBeLessThan(2_000);

  const wider = traceWindow({ start_time: START, end_time: ONE_SECOND }, [
    { span: { start_time: BEFORE, end_time: AFTER } },
  ]);
  expect(wider.startNs).toBeLessThan(
    traceWindow({ start_time: START, end_time: ONE_SECOND }, []).startNs,
  );
  expect(wider.endNs).toBeGreaterThan(
    traceWindow({ start_time: START, end_time: ONE_SECOND }, []).endNs,
  );
});

it("clamps bars to the window and keeps a sliver for a zero-duration span", () => {
  const window = traceWindow({ start_time: START, end_time: ONE_SECOND }, []);
  expect(barGeometry({ start_time: START, end_time: ONE_SECOND }, window)).toEqual({
    leftPct: 0,
    widthPct: 100,
    clipStart: false,
    clipEnd: false,
  });

  const zero = barGeometry({ start_time: HALF, end_time: HALF }, window);
  if (zero === null) {
    throw new Error("expected a bar for a span inside the window");
  }
  expect(zero.widthPct).toBeCloseTo(0.15);
  expect(zero.leftPct).toBeGreaterThan(40);
  expect(zero.leftPct).toBeLessThan(60);
  expect(zero.leftPct + zero.widthPct).toBeLessThanOrEqual(100);

  const early = barGeometry({ start_time: BEFORE, end_time: BEFORE }, window);
  expect(early).toBeNull();

  const late = barGeometry({ start_time: AFTER, end_time: AFTER }, window);
  expect(late).toBeNull();
});

it("places four to eight nice ticks labelled from the window start", () => {
  const window = { startNs: 0, endNs: 4_000_000_000 };
  const marks = ticks(window, 640);
  expect(marks.length).toBeGreaterThanOrEqual(4);
  expect(marks.length).toBeLessThanOrEqual(8);
  const first = marks[0];
  const second = marks[1];
  if (!first || !second) {
    throw new Error("expected at least two ticks");
  }
  expect(first).toEqual({ ns: 0, label: "0ms" });
  const step = second.ns - first.ns;
  expect(isNice(step)).toBe(true);
  for (const mark of marks) {
    expect(mark.label).toBe(formatDuration(mark.ns - window.startNs));
  }
  expect(marks.map((mark) => mark.label)).toContain("1.00s");
});

const TRACE = { startNs: 0, endNs: 1_000_000_000 };

it("zooms about an anchor, pans, and stays inside the trace", () => {
  const zoomed = zoomAt(TRACE, TRACE, 50, 2);
  expect(zoomed).toEqual({ startNs: 250_000_000, endNs: 750_000_000 });
  expect(zoomAt(TRACE, zoomed, 0, 0.5)).toEqual(TRACE);
  expect(zoomAt(TRACE, TRACE, 50, 0)).toEqual(TRACE);

  const panned = panBy(TRACE, { startNs: 0, endNs: 400_000_000 }, 50);
  expect(panned).toEqual({ startNs: 200_000_000, endNs: 600_000_000 });
  expect(panBy(TRACE, { startNs: 800_000_000, endNs: 1_000_000_000 }, 100)).toEqual({
    startNs: 800_000_000,
    endNs: 1_000_000_000,
  });

  expect(clampView(TRACE, { startNs: -50, endNs: 100 })).toEqual({
    startNs: 0,
    endNs: 1_000,
  });
  expect(clampView(TRACE, { startNs: 900_000_000, endNs: 1_200_000_000 }).endNs).toBe(TRACE.endNs);
});

it("zooms to a span with five percent padding on each side", () => {
  const span = {
    start_time: "1970-01-01T00:00:00.100000000Z",
    end_time: "1970-01-01T00:00:00.200000000Z",
  };
  expect(zoomToSpan(TRACE, span)).toEqual({
    startNs: 95_000_000,
    endNs: 205_000_000,
  });
});

it("clips a bar that crosses the view and hides one that misses it", () => {
  const view = { startNs: 1_000_000_000, endNs: 2_000_000_000 };
  const crossing = barGeometry(
    {
      start_time: "1970-01-01T00:00:00.500000000Z",
      end_time: "1970-01-01T00:00:01.500000000Z",
    },
    view,
  );
  expect(crossing).toEqual({ leftPct: 0, widthPct: 50, clipStart: true, clipEnd: false });
  const leaving = barGeometry(
    {
      start_time: "1970-01-01T00:00:01.500000000Z",
      end_time: "1970-01-01T00:00:02.500000000Z",
    },
    view,
  );
  expect(leaving).toEqual({ leftPct: 50, widthPct: 50, clipStart: false, clipEnd: true });
});

function isNice(step: number): boolean {
  const exponent = Math.floor(Math.log10(step));
  const base = step / 10 ** exponent;
  return [1, 2, 5].some((nice) => Math.abs(base - nice) < 1e-6);
}
