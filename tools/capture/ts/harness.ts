import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { OTLPTraceExporter } from "@opentelemetry/exporter-trace-otlp-proto";
import { defaultResource, resourceFromAttributes } from "@opentelemetry/resources";
import { NodeTracerProvider, SimpleSpanProcessor } from "@opentelemetry/sdk-trace-node";

const projectRoot = fileURLToPath(new URL(".", import.meta.url));

export function setup(): NodeTracerProvider {
  const serviceName = process.env.OTEL_SERVICE_NAME;
  // defaultResource() does not read OTEL_SERVICE_NAME. The runner sets that
  // variable, so the service name is applied on top of the SDK resource.
  const resource =
    serviceName === undefined
      ? defaultResource()
      : defaultResource().merge(resourceFromAttributes({ "service.name": serviceName }));
  const provider = new NodeTracerProvider({
    resource,
    spanProcessors: [new SimpleSpanProcessor(new OTLPTraceExporter())],
  });
  provider.register();
  return provider;
}

export function mockUrl(pathSuffix = ""): string {
  const base = process.env.LLMOBS_CAPTURE_MOCK_URL;
  if (base === undefined) {
    throw new Error("LLMOBS_CAPTURE_MOCK_URL is unset");
  }
  return base + pathSuffix;
}

export async function finish(
  provider: NodeTracerProvider,
  aliases: Record<string, string> = {},
): Promise<void> {
  await provider.forceFlush();
  await provider.shutdown();
  const listed = process.env.LLMOBS_CAPTURE_LIBRARIES;
  const metaPath = process.env.LLMOBS_CAPTURE_META;
  if (listed === undefined) {
    throw new Error("LLMOBS_CAPTURE_LIBRARIES is unset");
  }
  if (metaPath === undefined) {
    throw new Error("LLMOBS_CAPTURE_META is unset");
  }
  const libraries: Record<string, string> = {};
  for (const name of listed.split(",")) {
    if (name.length === 0) {
      continue;
    }
    const directory = aliases[name] ?? name;
    const packageJson = path.join(projectRoot, "node_modules", directory, "package.json");
    const parsed = JSON.parse(readFileSync(packageJson, "utf8")) as {
      name?: string;
      version?: string;
    };
    if (parsed.name !== name || parsed.version === undefined) {
      throw new Error(`library ${name} was not installed under ${directory}`);
    }
    libraries[name] = parsed.version;
  }
  writeFileSync(metaPath, JSON.stringify({ libraries }));
}
