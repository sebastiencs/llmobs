import { useEffect, useRef, useState } from "react";
import type { Schema } from "@/api/types";

const BACKOFF_START_MS = 1_000;
const BACKOFF_CAP_MS = 30_000;
const PING_GAP_MS = 45_000;

export type StreamStatus = "connecting" | "open" | "retrying" | "closed";

export type StreamEvent =
  | { type: "span"; span: Schema<"SpanSummary"> }
  | { type: "trace"; trace: Schema<"TraceSummary"> }
  | { type: "resync" };

/**
 * List stream URL. Same query as the trace list, without paging or time.
 * `project` is `projectRef(org, project)`.
 */
export function traceStreamUrl(
  project: string,
  query: {
    filter?: string;
    search?: string;
    includeEval?: boolean;
    includeExperiments?: boolean;
  },
): string {
  const params = new URLSearchParams();
  if (query.filter) {
    params.set("filter", query.filter);
  }
  if (query.search) {
    params.set("search", query.search);
  }
  if (query.includeEval) {
    params.set("include_eval", "true");
  }
  if (query.includeExperiments) {
    params.set("include_experiments", "true");
  }
  const path = `/api/v1/projects/${encodeURIComponent(project)}/traces/stream`;
  const search = params.toString();
  return search ? `${path}?${search}` : path;
}

export function openStream(
  url: string,
  onEvent: (event: StreamEvent) => void,
  onStatus: (status: StreamStatus) => void,
): () => void {
  let attempt = 0;
  let everOpened = false;
  let userClosed = false;
  let socket: EventSource | undefined;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let pingTimer: ReturnType<typeof setTimeout> | undefined;
  const dropped = { count: 0 };

  const clearPing = () => {
    if (pingTimer !== undefined) {
      clearTimeout(pingTimer);
      pingTimer = undefined;
    }
  };

  const armPing = () => {
    clearPing();
    pingTimer = setTimeout(() => {
      fail();
    }, PING_GAP_MS);
  };

  const fail = () => {
    if (userClosed || socket === undefined) {
      return;
    }
    const current = socket;
    socket = undefined;
    current.close();
    clearPing();
    onStatus("retrying");
    const delay = Math.min(BACKOFF_START_MS * 2 ** attempt, BACKOFF_CAP_MS);
    attempt += 1;
    retryTimer = setTimeout(connect, delay);
  };

  const connect = () => {
    if (userClosed) {
      return;
    }
    retryTimer = undefined;
    onStatus("connecting");
    const source = new EventSource(url, { withCredentials: false });
    socket = source;

    source.addEventListener("open", () => {
      if (socket !== source) {
        return;
      }
      onStatus("open");
      if (everOpened) {
        onEvent({ type: "resync" });
      }
      everOpened = true;
      attempt = 0;
      armPing();
    });

    source.addEventListener("error", () => {
      if (socket !== source) {
        return;
      }
      fail();
    });

    source.addEventListener("ping", () => {
      if (socket === source) {
        armPing();
      }
    });

    source.addEventListener("span", (event) => {
      deliver(source, "span", event);
    });
    source.addEventListener("trace", (event) => {
      deliver(source, "trace", event);
    });
    source.addEventListener("resync", (event) => {
      deliver(source, "resync", event);
    });
  };

  const deliver = (source: EventSource, type: StreamEvent["type"], event: Event) => {
    if (socket !== source) {
      return;
    }
    const value = parseObject(event);
    if (!value) {
      dropped.count += 1;
      return;
    }
    if (type === "span") {
      onEvent({ type, span: value as Schema<"SpanSummary"> });
    } else if (type === "trace") {
      onEvent({ type, trace: value as Schema<"TraceSummary"> });
    } else {
      onEvent({ type: "resync" });
    }
  };

  connect();

  return () => {
    userClosed = true;
    if (retryTimer !== undefined) {
      clearTimeout(retryTimer);
      retryTimer = undefined;
    }
    clearPing();
    const current = socket;
    socket = undefined;
    current?.close();
    onStatus("closed");
  };
}

export function useStream(
  url: string | undefined,
  onEvent: (event: StreamEvent) => void,
): { status: StreamStatus } {
  const [status, setStatus] = useState<StreamStatus>(url ? "connecting" : "closed");
  const onEventRef = useRef(onEvent);
  onEventRef.current = onEvent;

  useEffect(() => {
    if (!url) {
      setStatus("closed");
      return;
    }
    let active = true;
    setStatus("connecting");
    const stop = openStream(
      url,
      (event) => {
        onEventRef.current(event);
      },
      (next) => {
        if (active) {
          setStatus(next);
        }
      },
    );
    return () => {
      active = false;
      stop();
    };
  }, [url]);

  return { status };
}

function parseObject(event: Event): Record<string, unknown> | undefined {
  if (!(event instanceof MessageEvent) || typeof event.data !== "string") {
    return undefined;
  }
  try {
    const value: unknown = JSON.parse(event.data);
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      return undefined;
    }
    return value as Record<string, unknown>;
  } catch {
    return undefined;
  }
}
