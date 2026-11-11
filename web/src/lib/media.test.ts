import { expect, it } from "vitest";
import { mediaUrl } from "@/lib/media";

it("encodes the project ref in a media url", () => {
  expect(mediaUrl("acme/support-bot", "media://497790947d466676")).toBe(
    "/api/v1/projects/acme%2Fsupport-bot/media/497790947d466676",
  );
});

it("returns undefined for media dropped at ingest", () => {
  expect(mediaUrl("acme/support-bot", "llmobs:dropped")).toBeUndefined();
});

it("returns https and data uris unchanged", () => {
  expect(mediaUrl("acme/support-bot", "https://example.com/a.png")).toBe(
    "https://example.com/a.png",
  );
  expect(mediaUrl("acme/support-bot", "data:text/plain,hi")).toBe("data:text/plain,hi");
});
