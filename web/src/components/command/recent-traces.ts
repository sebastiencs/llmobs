import { STORAGE_PREFIX } from "@/product";

export type RecentTrace = { id: string; name: string };

const STORAGE_KEY = `${STORAGE_PREFIX}recent-traces`;
const LIMIT = 10;

function projectKey(org: string, project: string): string {
  return `${org}/${project}`;
}

function validTrace(item: unknown): RecentTrace | undefined {
  if (!item || typeof item !== "object" || !("id" in item) || !("name" in item)) {
    return undefined;
  }
  const { id, name } = item;
  if (typeof id !== "string" || typeof name !== "string" || id.length === 0 || name.length === 0) {
    return undefined;
  }
  return { id, name };
}

function readStore(): Record<string, RecentTrace[]> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) {
      return {};
    }
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {};
    }
    const store: Record<string, RecentTrace[]> = {};
    for (const [key, value] of Object.entries(parsed)) {
      if (!Array.isArray(value)) {
        continue;
      }
      const traces = value.flatMap((item) => {
        const trace = validTrace(item);
        return trace ? [trace] : [];
      });
      if (traces.length > 0) {
        store[key] = traces.slice(0, LIMIT);
      }
    }
    return store;
  } catch {
    return {};
  }
}

export function readRecent(org: string, project: string): RecentTrace[] {
  return readStore()[projectKey(org, project)] ?? [];
}

/** Remembers an opened trace. Stores the id and name only. */
export function rememberTrace(org: string, project: string, trace: RecentTrace): void {
  const store = readStore();
  const key = projectKey(org, project);
  const existing = store[key] ?? [];
  const previous = existing.find((item) => item.id === trace.id);
  const name = trace.name === trace.id && previous ? previous.name : trace.name;
  store[key] = [{ id: trace.id, name }, ...existing.filter((item) => item.id !== trace.id)].slice(
    0,
    LIMIT,
  );
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
  } catch {
    // Private mode can throw. The palette still works for this page.
  }
}

/** Trace id currently open in the split (`?trace=`) or on the full page. */
export function openTraceId(pathname: string, search: unknown): string | undefined {
  const fromPath = /\/traces\/([0-9a-f]{32})$/.exec(pathname)?.[1];
  if (fromPath) {
    return fromPath;
  }
  if (!search || typeof search !== "object" || !("trace" in search)) {
    return undefined;
  }
  const trace = search.trace;
  return typeof trace === "string" && /^[0-9a-f]{32}$/.test(trace) ? trace : undefined;
}
