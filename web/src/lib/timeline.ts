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

/** Bar position clamped to the window. A zero-duration span keeps a sliver of width. */
export function barGeometry(
  span: { start_time: string; end_time: string },
  window: TimeWindow,
): { leftPct: number; widthPct: number } {
  const spanNs = window.endNs - window.startNs;
  const rawLeft = ((instantNs(span.start_time) - window.startNs) / spanNs) * 100;
  const rawRight = ((instantNs(span.end_time) - window.startNs) / spanNs) * 100;
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
  return { leftPct: left, widthPct: width };
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
