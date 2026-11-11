const KEY = "sk-llmobs-…";

export const API_KEY_NOTE = "Create a secret key in Settings → API keys";

export const WAITING_LINE = "Waiting for the first trace…";

/** Raw OTLP/HTTP exporter settings. The endpoint is `<origin>/v1/traces`. */
export function otelSnippet(origin: string): string {
  return [
    `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=${origin}/v1/traces`,
    `OTEL_EXPORTER_OTLP_TRACES_HEADERS="Authorization=Bearer ${KEY}"`,
    "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf",
  ].join("\n");
}

/** `llmobs.init` with the Python option names. Env vars apply when an argument is omitted. */
export function pythonSnippet(origin: string): string {
  return [
    "import llmobs",
    "",
    "# Or set LLMOBS_API_KEY and LLMOBS_BASE_URL.",
    "llmobs.init(",
    `    api_key="${KEY}",`,
    `    base_url="${origin}",`,
    ")",
  ].join("\n");
}

/** `llmobs.init` with the TypeScript option names. The env vars match Python. */
export function typescriptSnippet(origin: string): string {
  return [
    'import * as llmobs from "llmobs";',
    "",
    "// Or set LLMOBS_API_KEY and LLMOBS_BASE_URL.",
    "llmobs.init({",
    `  apiKey: "${KEY}",`,
    `  baseUrl: "${origin}",`,
    "});",
  ].join("\n");
}

/** Pass `llmobs.aiTelemetry()` as the AI SDK's `experimental_telemetry`. */
export function vercelSnippet(origin: string): string {
  return [
    'import { generateText } from "ai";',
    'import * as llmobs from "llmobs";',
    "",
    "llmobs.init({",
    `  apiKey: "${KEY}",`,
    `  baseUrl: "${origin}",`,
    "});",
    "",
    "await generateText({",
    "  model,",
    '  prompt: "Hello",',
    "  experimental_telemetry: llmobs.aiTelemetry(),",
    "});",
  ].join("\n");
}
