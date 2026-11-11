import { expect, it } from "vitest";
import {
  API_KEY_NOTE,
  otelSnippet,
  pythonSnippet,
  typescriptSnippet,
  vercelSnippet,
} from "@/components/onboarding/snippets";
import { tracesNeedOnboarding } from "@/components/onboarding/state";

const ORIGIN = "https://llmobs.example";

it("builds the OTel endpoint from the origin", () => {
  const snippet = otelSnippet(ORIGIN);
  expect(snippet).toContain(`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=${ORIGIN}/v1/traces`);
  expect(snippet).toContain('OTEL_EXPORTER_OTLP_TRACES_HEADERS="Authorization=Bearer sk-llmobs-…"');
  expect(snippet).toContain("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf");
});

it("uses the spec init names and env vars", () => {
  for (const snippet of [pythonSnippet(ORIGIN), typescriptSnippet(ORIGIN), vercelSnippet(ORIGIN)]) {
    expect(snippet).toContain(ORIGIN);
    expect(snippet).toContain("sk-llmobs-…");
  }
  expect(pythonSnippet(ORIGIN)).toContain('api_key="sk-llmobs-…"');
  expect(pythonSnippet(ORIGIN)).toContain(`base_url="${ORIGIN}"`);
  expect(pythonSnippet(ORIGIN)).toContain("LLMOBS_API_KEY");
  expect(pythonSnippet(ORIGIN)).toContain("LLMOBS_BASE_URL");
  expect(typescriptSnippet(ORIGIN)).toContain('apiKey: "sk-llmobs-…"');
  expect(typescriptSnippet(ORIGIN)).toContain(`baseUrl: "${ORIGIN}"`);
  expect(vercelSnippet(ORIGIN)).toContain("experimental_telemetry: llmobs.aiTelemetry()");
  expect(API_KEY_NOTE).toBe("Create a secret key in Settings → API keys");
});

it("keeps onboarding for a preset and drops it when the list is constrained", () => {
  const open = { kind: "preset", preset: "24h" } as const;
  expect(tracesNeedOnboarding({}, open)).toBe(true);
  expect(tracesNeedOnboarding({}, { kind: "preset", preset: "7d" })).toBe(true);
  expect(tracesNeedOnboarding({ filter: "status = 'error'" }, open)).toBe(false);
  expect(tracesNeedOnboarding({ search: "refund" }, open)).toBe(false);
  expect(tracesNeedOnboarding({ eval: 1 }, open)).toBe(false);
  expect(tracesNeedOnboarding({ exp: 1 }, open)).toBe(false);
  expect(
    tracesNeedOnboarding(
      {},
      { kind: "absolute", from: "2026-01-01T00:00:00.000Z", to: "2026-01-02T00:00:00.000Z" },
    ),
  ).toBe(false);
});
