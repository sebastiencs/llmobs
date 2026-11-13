import { expect, it } from "vitest";
import { classifyMedia } from "@/lib/media";

it.each([
  ["llmobs:dropped", "acme/support-bot", { kind: "dropped" }],
  [
    "media://497790947d466676",
    "acme/support-bot",
    { kind: "stored", url: "/api/v1/projects/acme%2Fsupport-bot/media/497790947d466676" },
  ],
  ["media://497790947d466676", undefined, { kind: "unsupported" }],
  [
    "https://example.com/a.png",
    "acme/support-bot",
    { kind: "external", url: "https://example.com/a.png" },
  ],
  [
    "HTTPS://example.com/a.png",
    "acme/support-bot",
    { kind: "external", url: "HTTPS://example.com/a.png" },
  ],
  [
    "Http://example.com/a.png",
    "acme/support-bot",
    { kind: "external", url: "Http://example.com/a.png" },
  ],
  ["//example.com/a.png", "acme/support-bot", { kind: "external", url: "//example.com/a.png" }],
  [
    "data:image/png;base64,AAAA",
    "acme/support-bot",
    { kind: "inline", url: "data:image/png;base64,AAAA" },
  ],
  [
    "DATA:AUDIO/wav;base64,AAAA",
    "acme/support-bot",
    { kind: "inline", url: "DATA:AUDIO/wav;base64,AAAA" },
  ],
  ["data:text/html,<b>x</b>", "acme/support-bot", { kind: "unsupported" }],
  ["javascript:alert(1)", "acme/support-bot", { kind: "unsupported" }],
  ["ftp://example.com/a.png", "acme/support-bot", { kind: "unsupported" }],
  ["gs://bucket/a.png", "acme/support-bot", { kind: "unsupported" }],
  ["a.png", "acme/support-bot", { kind: "unsupported" }],
  ["/a.png", "acme/support-bot", { kind: "unsupported" }],
])("classifies %s", (uri, ref, expected) => {
  expect(classifyMedia(ref, uri)).toEqual(expected);
});
