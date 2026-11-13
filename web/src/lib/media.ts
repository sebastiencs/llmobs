const MEDIA_PREFIX = "media://";

export type MediaSource =
  /** `llmobs:dropped`: the bytes were dropped at ingest. */
  | { kind: "dropped" }
  /** `media://<sha256>` of this project: served by the API. */
  | { kind: "stored"; url: string }
  /** `data:image/…` or `data:audio/…`: inline bytes, no network. */
  | { kind: "inline"; url: string }
  /** `http:`/`https:` in any case, or protocol-relative `//host/…`: loaded only after a click. */
  | { kind: "external"; url: string }
  /** Anything else (other schemes, relative paths, other `data:` types, or `media://` with no project): never loaded or linked. */
  | { kind: "unsupported" };

export function classifyMedia(projectRef: string | undefined, uri: string): MediaSource {
  if (uri === "llmobs:dropped") return { kind: "dropped" };
  if (uri.startsWith(MEDIA_PREFIX)) {
    if (projectRef === undefined) return { kind: "unsupported" };
    const sha = uri.slice(MEDIA_PREFIX.length);
    return {
      kind: "stored",
      url: `/api/v1/projects/${encodeURIComponent(projectRef)}/media/${encodeURIComponent(sha)}`,
    };
  }
  if (uri.startsWith("//")) return { kind: "external", url: uri };
  const scheme = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(uri)?.[1]?.toLowerCase();
  if (scheme === "http" || scheme === "https") return { kind: "external", url: uri };
  if (scheme === "data" && /^data:(image|audio)\//i.test(uri)) return { kind: "inline", url: uri };
  return { kind: "unsupported" };
}
