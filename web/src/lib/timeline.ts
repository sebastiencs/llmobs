import { formatDuration, instantNs } from "@/lib/format";

/** One microsecond. A window narrower than this is widened so bars have a scale. */
const MIN_WINDOW_NS = 1_000;
const MIN_BAR_PCT = 0.15;
const MIN_TICKS = 4;
const MAX_TICKS = 8;

export type TimeWindow = {
  startNs: number;
  endNs: number;
};

export function traceWindow(
  trace: { start_time: string; end_time: string },
  spans: readonly { span: { start_time: string; end_time: string } }[],
): TimeWindow {
  let startNs = instantNs(trace.start_time);
  let endNs = instantNs(trace.end_time);
  for (const row of spans) {
    startNs = Math.min(startNs, instantNs(row.span.start_time));
    endNs = Math.max(endNs, instantNs(row.span.end_time));
  }
  if (!(endNs - startNs >= MIN_WINDOW_NS)) {
    endNs = startNs + MIN_WINDOW_NS;
  }
  return { startNs, endNs };
}

export type BarGeometry = {
  leftPct: number;
  widthPct: number;
  /** The span starts before the view. */
  clipStart: boolean;
  /** The span ends after the view. */
  clipEnd: boolean;
};

/**
 * Bar position inside the view window.
 * A span that misses the view is hidden. A span that crosses an edge is clipped.
 * A zero-duration span inside the view keeps a sliver of width.
 */
export function barGeometry(
  span: { start_time: string; end_time: string },
  view: TimeWindow,
): BarGeometry | null {
  const spanNs = view.endNs - view.startNs;
  if (!(spanNs > 0)) {
    return null;
  }
  const start = instantNs(span.start_time);
  const end = instantNs(span.end_time);
  if (end <= view.startNs || start >= view.endNs) {
    return null;
  }
  const clipStart = start < view.startNs;
  const clipEnd = end > view.endNs;
  const rawLeft = ((start - view.startNs) / spanNs) * 100;
  const rawRight = ((end - view.startNs) / spanNs) * 100;
  let left = clamp(rawLeft, 0, 100);
  let right = clamp(rawRight, 0, 100);
  if (right < left) {
    right = left;
  }
  let width = right - left;
  if (width < MIN_BAR_PCT) {
    width = MIN_BAR_PCT;
    if (left + width > 100) {
      left = 100 - MIN_BAR_PCT;
    }
  }
  return { leftPct: left, widthPct: width, clipStart, clipEnd };
}

/** Keep `view` inside the trace window. The narrowest view is one microsecond. */
export function clampView(trace: TimeWindow, view: TimeWindow): TimeWindow {
  const span = trace.endNs - trace.startNs;
  const minWidth = Math.min(span, MIN_WINDOW_NS);
  let width = view.endNs - view.startNs;
  if (!(width >= minWidth)) {
    width = minWidth;
  }
  if (width > span) {
    width = span;
  }
  let start = Number.isFinite(view.startNs) ? view.startNs : trace.startNs;
  if (start < trace.startNs) {
    start = trace.startNs;
  }
  if (start + width > trace.endNs) {
    start = trace.endNs - width;
  }
  return { startNs: start, endNs: start + width };
}

/**
 * Zoom about a point. `factor` above 1 narrows the view; below 1 widens it.
 * `anchorPct` is the pointer's place in the current view, 0–100, and it stays put.
 */
export function zoomAt(
  trace: TimeWindow,
  view: TimeWindow,
  anchorPct: number,
  factor: number,
): TimeWindow {
  const current = clampView(trace, view);
  if (!(factor > 0) || !Number.isFinite(factor)) {
    return current;
  }
  const width = current.endNs - current.startNs;
  const pct = clamp(anchorPct, 0, 100) / 100;
  const anchor = current.startNs + pct * width;
  const nextWidth = width / factor;
  return clampView(trace, {
    startNs: anchor - pct * nextWidth,
    endNs: anchor - pct * nextWidth + nextWidth,
  });
}

/** Move the view by a percentage of its width. Positive goes later. */
export function panBy(trace: TimeWindow, view: TimeWindow, deltaPct: number): TimeWindow {
  const current = clampView(trace, view);
  const width = current.endNs - current.startNs;
  const delta = (deltaPct / 100) * width;
  return clampView(trace, {
    startNs: current.startNs + delta,
    endNs: current.endNs + delta,
  });
}

/** The span, plus 5 % of its duration on each side, clamped to the trace. */
export function zoomToSpan(
  trace: TimeWindow,
  span: { start_time: string; end_time: string },
): TimeWindow {
  const start = instantNs(span.start_time);
  let end = instantNs(span.end_time);
  if (end < start) {
    end = start;
  }
  const width = Math.max(end - start, MIN_WINDOW_NS);
  const pad = width * 0.05;
  return clampView(trace, { startNs: start - pad, endNs: start + width + pad });
}

/**
 * Four to eight ticks on a 1-2-5 step, labelled as a duration from the window start.
 * `widthPx` picks the density: a wider timeline aims for more ticks, still inside 4–8.
 */
export function ticks(window: TimeWindow, widthPx: number): { ns: number; label: string }[] {
  const span = window.endNs - window.startNs;
  const target = clamp(Math.round(widthPx / 80), MIN_TICKS, MAX_TICKS);
  const step = tickStep(span, target);
  const count = Math.min(MAX_TICKS, Math.max(1, Math.floor(span / step) + 1));
  const marks: { ns: number; label: string }[] = [];
  for (let index = 0; index < count; index++) {
    const offset = Math.min(span, index * step);
    marks.push({ ns: window.startNs + offset, label: formatDuration(offset) });
  }
  return marks;
}

function tickStep(span: number, target: number): number {
  if (!(span > 0)) {
    return 1;
  }
  const exponent = Math.floor(Math.log10(span));
  let best = span;
  let bestScore = Number.POSITIVE_INFINITY;
  for (let power = exponent - 2; power <= exponent; power++) {
    const magnitude = 10 ** power;
    if (!Number.isFinite(magnitude) || magnitude <= 0) {
      continue;
    }
    for (const base of [1, 2, 5]) {
      const step = base * magnitude;
      if (step <= 0 || step > span) {
        continue;
      }
      const count = Math.floor(span / step) + 1;
      if (count < MIN_TICKS || count > MAX_TICKS) {
        continue;
      }
      const score = Math.abs(count - target);
      if (score < bestScore) {
        bestScore = score;
        best = step;
      }
    }
  }
  return best;
}

function clamp(value: number, min: number, max: number): number {
  if (Number.isNaN(value)) {
    return min;
  }
  return Math.min(max, Math.max(min, value));
}
