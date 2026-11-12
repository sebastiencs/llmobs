import { sse } from "msw";
import type { Schema } from "@/api/types";

const PROJECT = "/api/v1/projects/:project";
const PING_INTERVAL_MS = 15_000;

export type StreamEmit = {
  trace(trace: Schema<"TraceSummary">): void;
  span(span: Schema<"SpanSummary">): void;
  resync(): void;
  ping(): void;
  close(): void;
};

export type TraceStreamSource = (emit: StreamEmit) => (() => void) | undefined;
export type SpanStreamSource = (emit: StreamEmit, traceId: string) => (() => void) | undefined;

type StreamEvents = {
  trace: Schema<"TraceSummary">;
  span: Schema<"SpanSummary">;
  resync: Record<string, never>;
  ping: Record<string, never>;
};

export function traceStreamHandler(source: TraceStreamSource) {
  return sse<StreamEvents>(`${PROJECT}/traces/stream`, ({ client, request }) => {
    // The Node interceptor delivers headers only after the first body byte.
    client.send({ data: "" });
    const stop = source(bind(client));
    request.signal.addEventListener("abort", () => {
      stop?.();
    });
  });
}

export function spanStreamHandler(source: SpanStreamSource) {
  return sse<StreamEvents>(`${PROJECT}/traces/:trace_id/stream`, ({ client, request, params }) => {
    client.send({ data: "" });
    const traceId = typeof params.trace_id === "string" ? params.trace_id : "";
    const stop = source(bind(client), traceId);
    request.signal.addEventListener("abort", () => {
      stop?.();
    });
  });
}

/** Fixture projects are static: a ping keeps the stream open and nothing else changes. */
export function fixtureStreamHandlers() {
  return [traceStreamHandler(pingOnly), spanStreamHandler(pingOnly)];
}

function pingOnly(emit: StreamEmit): () => void {
  const timer = setInterval(() => {
    emit.ping();
  }, PING_INTERVAL_MS);
  return () => {
    clearInterval(timer);
  };
}

function bind(client: {
  send(payload: { event: "trace"; data: Schema<"TraceSummary"> }): void;
  send(payload: { event: "span"; data: Schema<"SpanSummary"> }): void;
  send(payload: { event: "resync"; data: Record<string, never> }): void;
  send(payload: { event: "ping"; data: Record<string, never> }): void;
  close(): void;
}): StreamEmit {
  return {
    trace(trace) {
      client.send({ event: "trace", data: trace });
    },
    span(span) {
      client.send({ event: "span", data: span });
    },
    resync() {
      client.send({ event: "resync", data: {} });
    },
    ping() {
      client.send({ event: "ping", data: {} });
    },
    close() {
      client.close();
    },
  };
}
