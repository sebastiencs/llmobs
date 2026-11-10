import mswPackage from "msw/package.json";
import { expect, it } from "vitest";
import manifest from "../../../fixtures/api/manifest.json";
import workerSource from "../../msw/mockServiceWorker.js?raw";
import { sortBySpecificity } from "./handlers";
import { type MockEntry, type MockOperation, operations } from "./manifest";

it("maps every manifest entry to a GET operation", () => {
  const ids = new Set(operations.map((operation) => operation.operationId));
  for (const entry of manifest) {
    expect(ids.has(entry.operation_id), entry.operation_id).toBe(true);
  }
  expect(operations.every((operation) => operation.method === "get")).toBe(true);
  expect(operations.reduce((sum, operation) => sum + operation.entries.length, 0)).toBe(
    manifest.length,
  );
});

it("serves every manifest entry", async () => {
  for (const operation of operations) {
    for (const entry of operation.entries) {
      const url = fixtureUrl(operation, entry);
      const response = await fetch(url);
      expect(response.status, url).toBe(entry.status);
      expect(response.headers.get("content-type"), url).toBe(entry.contentType);
      if (entry.body instanceof Uint8Array) {
        const bytes = new Uint8Array(await response.arrayBuffer());
        expect(Array.from(bytes), url).toEqual(Array.from(entry.body));
        expect(await sha256(bytes), url).toBe(fileName(entry.file));
      } else {
        expect(await response.json(), url).toEqual(entry.body);
      }
    }
  }
});

it("selects the list_traces fixture from the query", async () => {
  const operation = operations.find((item) => item.operationId === "list_traces");
  if (!operation) {
    throw new Error("list_traces is missing");
  }
  const base = fixtureUrl(operation, operation.entries[0] ?? emptyEntry(), {});
  const includeEval = bodyFor(operation, "responses/traces-include-eval.json");
  const plain = bodyFor(operation, "responses/traces.json");

  expect(await (await fetch(`${base}?include_eval=true&limit=10`)).json()).toEqual(includeEval);
  expect(await (await fetch(`${base}?limit=10`)).json()).toEqual(plain);
  expect(await (await fetch(`${base}?include_eval=false`)).json()).toEqual(plain);
});

it("returns a not-found problem for an unknown trace or org", async () => {
  const trace = await fetch(
    `${location.origin}/api/v1/projects/acme%2Fsupport-bot/traces/does-not-exist`,
  );
  expect(trace.status).toBe(404);
  expect(trace.headers.get("content-type")).toBe("application/problem+json");
  expect(await trace.json()).toEqual({
    type: "about:blank",
    title: "Not Found",
    status: 404,
    code: "not_found",
  });

  const projects = await fetch(`${location.origin}/api/v1/orgs/nope/projects`);
  expect(projects.status).toBe(404);
  expect(projects.headers.get("content-type")).toBe("application/problem+json");
  expect(await projects.json()).toMatchObject({ code: "not_found", status: 404 });
});

it("sorts a literal path ahead of a param path", () => {
  expect(
    sortBySpecificity([{ mswPath: "/a/:id" }, { mswPath: "/a/stream" }]).map(
      (item) => item.mswPath,
    ),
  ).toEqual(["/a/stream", "/a/:id"]);
});

it("commits the service worker for the installed msw version", () => {
  expect(workerSource).toContain(`const PACKAGE_VERSION = '${mswPackage.version}'`);
});

function fixtureUrl(
  operation: MockOperation,
  entry: MockEntry,
  query: Record<string, string> = entry.query,
): string {
  let path = operation.path;
  for (const name of path.matchAll(/\{([^}]+)\}/g)) {
    const param = name[1];
    if (!param) {
      continue;
    }
    const raw = param === "project" ? "acme/support-bot" : entry.pathParams[param];
    if (raw === undefined) {
      throw new Error(`${operation.operationId}: missing path param ${param}`);
    }
    path = path.replaceAll(`{${param}}`, encodeURIComponent(raw));
  }
  const search = new URLSearchParams(query).toString();
  return `${location.origin}${path}${search.length > 0 ? `?${search}` : ""}`;
}

function bodyFor(operation: MockOperation, file: string): unknown {
  const entry = operation.entries.find((item) => item.file === file);
  if (!entry) {
    throw new Error(`${file} is missing`);
  }
  return entry.body;
}

function fileName(file: string): string {
  const base = file.split("/").pop() ?? file;
  return base.replace(/\.png$/, "");
}

async function sha256(bytes: Uint8Array): Promise<string> {
  const buffer = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(buffer).set(bytes);
  const digest = await crypto.subtle.digest("SHA-256", buffer);
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function emptyEntry(): MockEntry {
  return {
    pathParams: {},
    query: {},
    status: 200,
    contentType: "application/json",
    file: "",
    body: {},
  };
}
