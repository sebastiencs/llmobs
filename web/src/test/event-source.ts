/**
 * Test EventSource. jsdom has none, and MSW's `sse()` refuses to build a
 * handler without the constructor. The client owns reconnects, so this
 * polyfill closes on failure instead of retrying.
 */
export class FetchEventSource extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;

  readonly CONNECTING = 0;
  readonly OPEN = 1;
  readonly CLOSED = 2;
  readonly url: string;
  readonly withCredentials: boolean;
  readyState = FetchEventSource.CONNECTING;

  #abort = new AbortController();
  #closed = false;
  #reader: ReadableStreamDefaultReader<Uint8Array> | undefined;

  constructor(url: string | URL, init?: EventSourceInit) {
    super();
    this.url = url.toString();
    this.withCredentials = init?.withCredentials ?? false;
    void this.#connect().catch(() => {});
  }

  close(): void {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    this.readyState = FetchEventSource.CLOSED;
    if (this.#reader) {
      // Cancelling a live body aborts the fetch, and undici then asserts
      // when the mock socket closes. Stop reading and leave the body to end.
      return;
    }
    this.#abort.abort();
  }

  async #connect(): Promise<void> {
    try {
      const response = await fetch(this.url, {
        headers: { Accept: "text/event-stream" },
        credentials: this.withCredentials ? "include" : "same-origin",
        signal: this.#abort.signal,
        cache: "no-store",
      });
      if (this.#closed) {
        return;
      }
      if (!response.ok || !response.body) {
        this.#fail();
        return;
      }
      this.readyState = FetchEventSource.OPEN;
      this.dispatchEvent(new Event("open"));
      await this.#read(response.body);
      if (!this.#closed) {
        this.#fail();
      }
    } catch {
      if (!this.#closed) {
        this.#fail();
      }
    }
  }

  #fail(): void {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    this.readyState = FetchEventSource.CLOSED;
    this.dispatchEvent(new Event("error"));
  }

  async #read(body: ReadableStream<Uint8Array>): Promise<void> {
    const reader = body.getReader();
    this.#reader = reader;
    const decoder = new TextDecoder();
    let buffer = "";
    while (!this.#closed) {
      const { done, value } = await reader.read();
      if (this.#closed || done) {
        return;
      }
      buffer += decoder.decode(value, { stream: true });
      buffer = drain(buffer, (block) => this.#dispatch(block));
    }
  }

  #dispatch(block: string): void {
    let event = "message";
    const data: string[] = [];
    for (const line of block.split("\n")) {
      if (line === "" || line.startsWith(":")) {
        continue;
      }
      const colon = line.indexOf(":");
      const field = colon === -1 ? line : line.slice(0, colon);
      let value = colon === -1 ? "" : line.slice(colon + 1);
      if (value.startsWith(" ")) {
        value = value.slice(1);
      }
      if (field === "event") {
        event = value;
      } else if (field === "data") {
        data.push(value);
      }
    }
    if (data.length === 0) {
      return;
    }
    this.dispatchEvent(new MessageEvent(event, { data: data.join("\n") }));
  }
}

function drain(buffer: string, emit: (block: string) => void): string {
  const normalized = buffer.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const parts = normalized.split("\n\n");
  const rest = parts.pop() ?? "";
  for (const part of parts) {
    emit(part);
  }
  return rest;
}

export function installTestEventSource(): void {
  if (typeof globalThis.EventSource === "function") {
    return;
  }
  Object.defineProperty(globalThis, "EventSource", {
    value: FetchEventSource,
    configurable: true,
    writable: true,
  });
}
