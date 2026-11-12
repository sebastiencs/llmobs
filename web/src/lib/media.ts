const MEDIA_PREFIX = "media://";

/** URL the UI may request for a media URI. External URLs are returned unchanged and must not be loaded until the user asks. */
export function mediaUrl(projectRef: string, uri: string): string | undefined {
  if (uri === "llmobs:dropped") return undefined;
  if (uri.startsWith(MEDIA_PREFIX)) {
    const sha = uri.slice(MEDIA_PREFIX.length);
    return `/api/v1/projects/${encodeURIComponent(projectRef)}/media/${encodeURIComponent(sha)}`;
  }
  return uri;
}
