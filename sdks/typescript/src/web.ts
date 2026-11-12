/**
 * Runtime globals.
 *
 * The main tsconfig has no DOM or Node lib (`lib: ["ES2023"]`, `types: []`),
 * so no global is in scope for the build. This module declares structural
 * types for exactly what the SDK uses and the one accessor every other file
 * goes through. Nothing here knows a concrete runtime: the shapes fit Node,
 * Bun, Deno, edge runtimes and browsers alike.
 */

export interface RuntimeGlobals {
  console?: { warn(message: string): void };
  TextEncoder?: new () => { encode(text: string): Uint8Array };
  TextDecoder?: new (
    label?: string,
    options?: { fatal?: boolean },
  ) => {
    decode(bytes: Uint8Array): string;
  };
  setTimeout?: (fn: () => void, ms: number) => unknown;
  clearTimeout?: (handle: unknown) => void;
  setInterval?: (fn: () => void, ms: number) => unknown; // the handle may have unref()
  clearInterval?: (handle: unknown) => void;
  crypto?: { getRandomValues<T extends Uint8Array>(array: T): T };
  fetch?: FetchLike;
  AbortSignal?: { timeout?(ms: number): unknown };
  CompressionStream?: new (format: "gzip") => unknown;
  DecompressionStream?: new (format: "gzip") => unknown;
  Response?: new (body: unknown) => { arrayBuffer(): Promise<ArrayBuffer>; body: unknown };
  Blob?: new (parts: Uint8Array[]) => { stream(): { pipeThrough(transform: unknown): unknown } };
  process?: {
    once?(event: string, listener: () => void): unknown;
    env?: Record<string, string | undefined>;
  };
}

export function runtimeGlobals(): RuntimeGlobals {
  return globalThis as unknown as RuntimeGlobals;
}

export type FetchLike = (
  url: string,
  init: {
    method: string;
    headers: Record<string, string>;
    body: Uint8Array | string;
    signal?: unknown;
    keepalive?: boolean;
  },
) => Promise<ResponseLike>;

export interface ResponseLike {
  status: number;
  ok: boolean;
  headers: { get(name: string): string | null };
}

/** `fetch` bound to globalThis, when the runtime has one. */
export function getFetch(): FetchLike | undefined {
  return runtimeGlobals().fetch?.bind(globalThis);
}

/** Gzip `bytes`; `undefined` when CompressionStream/Response/Blob are missing. */
export async function gzip(bytes: Uint8Array): Promise<Uint8Array | undefined> {
  const { Blob, CompressionStream, Response } = runtimeGlobals();
  if (Blob === undefined || CompressionStream === undefined || Response === undefined) {
    return undefined;
  }
  const stream = new Blob([bytes]).stream().pipeThrough(new CompressionStream("gzip"));
  const buffer = await new Response(stream).arrayBuffer();
  return new Uint8Array(buffer);
}

/** `AbortSignal.timeout(ms)` or undefined. */
export function timeoutSignal(ms: number): unknown {
  return runtimeGlobals().AbortSignal?.timeout?.(ms);
}

/** UTF-8 encode through the runtime's TextEncoder. */
export function utf8Encode(text: string): Uint8Array {
  const { TextEncoder } = runtimeGlobals();
  if (TextEncoder === undefined) {
    throw new Error("TextEncoder is not available");
  }
  return new TextEncoder().encode(text);
}

/** UTF-8 decode through the runtime's TextDecoder. */
export function utf8Decode(bytes: Uint8Array): string {
  const { TextDecoder } = runtimeGlobals();
  if (TextDecoder === undefined) {
    throw new Error("TextDecoder is not available");
  }
  return new TextDecoder().decode(bytes);
}
