import openapiDocument from "../../../crates/server/openapi.json";
import manifestDocument from "../../../fixtures/api/manifest.json";

export interface MockOperation {
  operationId: string;
  method: "get";
  path: string;
  mswPath: string;
  entries: MockEntry[];
}

export interface MockEntry {
  pathParams: Record<string, string>;
  query: Record<string, string>;
  status: number;
  contentType: string;
  file: string;
  body: unknown | Uint8Array;
}

const HTTP_METHODS = ["get", "post", "put", "patch", "delete"] as const;

const jsonBodies = import.meta.glob("../../../fixtures/api/{static,responses}/**/*.json", {
  eager: true,
  import: "default",
});

const mediaBodies = import.meta.glob("../../../fixtures/api/media/*.png", {
  eager: true,
  query: "?inline",
  import: "default",
});

export const operations: MockOperation[] = loadOperations(
  openapiDocument,
  manifestDocument,
  jsonBodies,
  mediaBodies,
);

function loadOperations(
  openapi: unknown,
  manifest: unknown,
  json: unknown,
  media: unknown,
): MockOperation[] {
  const paths = operationsById(openapi);
  const rows = manifestRows(manifest);
  const grouped = new Map<string, MockEntry[]>();

  for (const row of rows) {
    const operation = paths.get(row.operationId);
    if (!operation) {
      throw new Error(`manifest: ${row.operationId} is missing from openapi.json`);
    }
    // POST examples document a response. The mock only serves GET.
    if (operation.method !== "get") {
      continue;
    }
    const entries = grouped.get(row.operationId) ?? [];
    entries.push(entryFromRow(row, json, media));
    grouped.set(row.operationId, entries);
  }

  const loaded: MockOperation[] = [];
  for (const [operationId, entries] of grouped) {
    const operation = paths.get(operationId);
    if (operation?.method !== "get") {
      throw new Error(`manifest: ${operationId} is not a GET`);
    }
    loaded.push({
      operationId,
      method: "get",
      path: operation.path,
      mswPath: operation.path.replace(/\{([^}]+)\}/g, ":$1"),
      entries,
    });
  }
  return loaded;
}

function operationsById(openapi: unknown): Map<string, { method: string; path: string }> {
  if (!isRecord(openapi) || !isRecord(openapi.paths)) {
    throw new Error("openapi.json: expected paths");
  }
  const found = new Map<string, { method: string; path: string }>();
  for (const [path, item] of Object.entries(openapi.paths)) {
    if (!isRecord(item)) {
      continue;
    }
    for (const method of HTTP_METHODS) {
      const operation = item[method];
      if (!isRecord(operation) || typeof operation.operationId !== "string") {
        continue;
      }
      if (found.has(operation.operationId)) {
        throw new Error(`openapi.json: duplicate operation ${operation.operationId}`);
      }
      found.set(operation.operationId, { method, path });
    }
  }
  return found;
}

interface ManifestRow {
  operationId: string;
  pathParams: Record<string, string>;
  query: Record<string, string>;
  status: number;
  contentType: string;
  file: string;
}

function manifestRows(manifest: unknown): ManifestRow[] {
  if (!Array.isArray(manifest)) {
    throw new Error("manifest.json: expected an array");
  }
  return manifest.map((row, index) => {
    if (!isRecord(row)) {
      throw new Error(`manifest.json[${index}]: expected an object`);
    }
    if (typeof row.operation_id !== "string") {
      throw new Error(`manifest.json[${index}]: operation_id must be a string`);
    }
    if (typeof row.status !== "number") {
      throw new Error(`manifest.json[${index}]: status must be a number`);
    }
    if (typeof row.content_type !== "string") {
      throw new Error(`manifest.json[${index}]: content_type must be a string`);
    }
    if (typeof row.file !== "string") {
      throw new Error(`manifest.json[${index}]: file must be a string`);
    }
    return {
      operationId: row.operation_id,
      pathParams: stringRecord(row.path_params, `manifest.json[${index}].path_params`),
      query: stringRecord(row.query, `manifest.json[${index}].query`),
      status: row.status,
      contentType: row.content_type,
      file: row.file,
    };
  });
}

function entryFromRow(row: ManifestRow, json: unknown, media: unknown): MockEntry {
  if (row.file.startsWith("media/")) {
    const inlined = lookup(media, row.file);
    if (typeof inlined !== "string") {
      throw new Error(`${row.file}: expected an inlined data URL, got ${typeof inlined}`);
    }
    return { ...rowFields(row), body: decodeDataUrl(inlined, row.file) };
  }
  return { ...rowFields(row), body: lookup(json, row.file) };
}

function rowFields(row: ManifestRow): Omit<MockEntry, "body"> {
  return {
    pathParams: row.pathParams,
    query: row.query,
    status: row.status,
    contentType: row.contentType,
    file: row.file,
  };
}

function lookup(files: unknown, file: string): unknown {
  if (!isRecord(files)) {
    throw new Error(`${file}: fixture bodies were not loaded`);
  }
  const suffix = `/${file}`;
  const key = Object.keys(files).find((candidate) => candidate.endsWith(suffix));
  if (key === undefined) {
    throw new Error(`${file}: fixture body was not loaded`);
  }
  return files[key];
}

function decodeDataUrl(value: string, file: string): Uint8Array {
  const marker = "base64,";
  const index = value.indexOf(marker);
  if (!value.startsWith("data:") || index === -1) {
    throw new Error(`${file}: expected an inlined data URL`);
  }
  const binary = atob(value.slice(index + marker.length));
  const bytes = new Uint8Array(binary.length);
  for (let offset = 0; offset < binary.length; offset += 1) {
    bytes[offset] = binary.charCodeAt(offset);
  }
  return bytes;
}

function stringRecord(value: unknown, label: string): Record<string, string> {
  if (!isRecord(value)) {
    throw new Error(`${label}: expected an object`);
  }
  const record: Record<string, string> = {};
  for (const [key, item] of Object.entries(value)) {
    if (typeof item !== "string") {
      throw new Error(`${label}: ${key} must be a string`);
    }
    record[key] = item;
  }
  return record;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
