import { beforeEach, expect, it } from "vitest";
import { ADA_ID, BOB_ID, NEW_USER_ID, TOKEN_ID } from "@/test/fixtures";
import openapiDocument from "../../../crates/server/openapi.json";
import adminUserCreatedDocument from "../../../fixtures/api/static/admin-user-created.json";
import inviteCreatedDocument from "../../../fixtures/api/static/invite-created.json";
import { server } from "./node";
import { WRITE_OPERATIONS, writeHandlers } from "./writes";

const PROJECT = "/api/v1/projects/acme%2Fsupport-bot";

/** Writes that later tracks add under the covered prefixes (none today). */
const LATER_TRACKS: string[] = [];

beforeEach(() => {
  server.use(...writeHandlers);
});

function part(json: Record<string, unknown>, key: string): Record<string, unknown> {
  const value = json[key];
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${key} is not an object`);
  }
  return value as Record<string, unknown>;
}

async function send(
  method: string,
  path: string,
  requestBody?: string,
): Promise<{ status: number; json: Record<string, unknown> | undefined }> {
  const response = await fetch(new URL(path, window.location.origin), {
    method,
    ...(requestBody !== undefined
      ? { body: requestBody, headers: { "Content-Type": "application/json" } }
      : {}),
  });
  const text = await response.text();
  return {
    status: response.status,
    json: text ? (JSON.parse(text) as Record<string, unknown>) : undefined,
  };
}

it("lists every account and admin write of the contract", () => {
  const paths = openapiDocument.paths as unknown as Record<
    string,
    Record<string, { operationId?: string }>
  >;
  const expected: { operationId: string; method: string; path: string }[] = [];
  for (const [path, operations] of Object.entries(paths)) {
    for (const method of ["post", "patch", "put", "delete"] as const) {
      const operationId = operations[method]?.operationId;
      if (!operationId) {
        continue;
      }
      const covered =
        path.startsWith("/api/auth/") ||
        path.startsWith("/api/v1/me") ||
        path.startsWith("/api/v1/admin/") ||
        path.startsWith("/api/v1/orgs") ||
        path === "/api/v1/projects/{project}" ||
        path.startsWith("/api/v1/projects/{project}/api-keys") ||
        path.startsWith("/api/v1/projects/{project}/members");
      if (covered && !LATER_TRACKS.includes(operationId)) {
        expected.push({ operationId, method, path });
      }
    }
  }
  expect(expected.map((operation) => operation.operationId).sort()).toEqual(
    WRITE_OPERATIONS.map((operation) => operation.operationId).sort(),
  );
  const listed = new Map(WRITE_OPERATIONS.map((operation) => [operation.operationId, operation]));
  for (const operation of expected) {
    const match = listed.get(operation.operationId);
    expect(match, operation.operationId).toBeDefined();
    expect(match?.method, operation.operationId).toBe(operation.method);
    expect(match?.path, operation.operationId).toBe(operation.path);
  }
});

it("has one handler per operation", () => {
  expect(writeHandlers).toHaveLength(WRITE_OPERATIONS.length);
});

it("signs in as the fixture user", async () => {
  const response = await send(
    "POST",
    "/api/auth/login",
    JSON.stringify({ email: "ada@example.com", password: "x" }),
  );
  expect(response.status).toBe(200);
  expect(part(response.json ?? {}, "user").email).toBe("ada@example.com");
});

it("refuses to change the signed-in admin", async () => {
  const response = await send(
    "PATCH",
    `/api/v1/admin/users/${ADA_ID}`,
    JSON.stringify({ disabled: true }),
  );
  expect(response.status).toBe(400);
  expect(response.json?.detail).toBe("cannot change your own account");
});

it("creates a public key", async () => {
  const response = await send(
    "POST",
    `${PROJECT}/api-keys`,
    JSON.stringify({ kind: "public", name: "Web" }),
  );
  expect(response.status).toBe(201);
  const key = part(response.json ?? {}, "key");
  expect(key.kind).toBe("public");
  expect(key.scopes).toEqual(["feedback"]);
  expect(key.name).toBe("Web");
  expect(response.json?.secret).toMatch(/^pk-llmobs-/);
});

it("omits the name of an unnamed key", async () => {
  const response = await send("POST", `${PROJECT}/api-keys`, JSON.stringify({ kind: "secret" }));
  expect(response.status).toBe(201);
  expect("name" in part(response.json ?? {}, "key")).toBe(false);
});

it("refuses an override for an owner", async () => {
  const response = await send(
    "PUT",
    `${PROJECT}/members/${ADA_ID}`,
    JSON.stringify({ role: "viewer" }),
  );
  expect(response.status).toBe(400);
});

it("answers 204 for deletes", async () => {
  expect((await send("DELETE", `/api/v1/me/tokens/${TOKEN_ID}`)).status).toBe(204);
  expect((await send("DELETE", "/api/v1/me/tokens/current")).status).toBe(204);
});

it("keeps retention when the patch leaves it out", async () => {
  const renamed = await send("PATCH", PROJECT, JSON.stringify({ name: "Renamed" }));
  expect(renamed.status).toBe(200);
  expect(renamed.json?.name).toBe("Renamed");
  expect(renamed.json?.retention_days).toBe(30);

  const forever = await send("PATCH", PROJECT, JSON.stringify({ retention_days: null }));
  expect(forever.status).toBe(200);
  expect("retention_days" in (forever.json ?? {})).toBe(false);

  const week = await send("PATCH", PROJECT, JSON.stringify({ retention_days: 7 }));
  expect(week.status).toBe(200);
  expect(week.json?.retention_days).toBe(7);
});

type Check = (json: Record<string, unknown>) => void;

const rows: [string, string, string, string | undefined, number, Check?][] = [
  [
    "patch_me trims the name",
    "PATCH",
    "/api/v1/me",
    JSON.stringify({ name: "  Ada  " }),
    200,
    (json) => expect(json.name).toBe("Ada"),
  ],
  [
    "patch_me clears the name",
    "PATCH",
    "/api/v1/me",
    JSON.stringify({}),
    200,
    (json) => expect("name" in json).toBe(false),
  ],
  [
    "post_personal_token keeps the expiry",
    "POST",
    "/api/v1/me/tokens",
    JSON.stringify({ name: "CI", scopes: ["read"], expires_at: "2027-01-01T00:00:00Z" }),
    201,
    (json) => {
      const token = part(json, "token");
      expect(token.name).toBe("CI");
      expect(token.scopes).toEqual(["read"]);
      expect(token.expires_at).toBe("2027-01-01T00:00:00.000Z");
    },
  ],
  [
    "post_personal_token drops a bad expiry",
    "POST",
    "/api/v1/me/tokens",
    JSON.stringify({ name: "CI", scopes: ["read"], expires_at: "soon" }),
    201,
    (json) => expect("expires_at" in part(json, "token")).toBe(false),
  ],
  [
    "post_org names the org",
    "POST",
    "/api/v1/orgs",
    JSON.stringify({ name: "Beta" }),
    201,
    (json) => {
      expect(json.name).toBe("Beta");
      expect(json.slug).toBe("acme");
    },
  ],
  [
    "patch_org renames",
    "PATCH",
    "/api/v1/orgs/acme",
    JSON.stringify({ name: "Acme 2" }),
    200,
    (json) => expect(json.name).toBe("Acme 2"),
  ],
  [
    "patch_org_member sets a role",
    "PATCH",
    `/api/v1/orgs/acme/members/${BOB_ID}`,
    JSON.stringify({ role: "viewer" }),
    200,
    (json) => expect(json.role).toBe("viewer"),
  ],
  [
    "patch_org_member refuses a bad role",
    "PATCH",
    `/api/v1/orgs/acme/members/${BOB_ID}`,
    JSON.stringify({ role: "boss" }),
    400,
  ],
  [
    "patch_org_member answers 404",
    "PATCH",
    `/api/v1/orgs/acme/members/${NEW_USER_ID}`,
    JSON.stringify({ role: "viewer" }),
    404,
  ],
  [
    "post_org_invite echoes",
    "POST",
    "/api/v1/orgs/acme/invites",
    JSON.stringify({ email: "x@example.com", role: "viewer" }),
    201,
    (json) => {
      const invite = part(json, "invite");
      expect(invite.email).toBe("x@example.com");
      expect(invite.role).toBe("viewer");
      expect(json.accept_url).toBe(inviteCreatedDocument.accept_url);
    },
  ],
  [
    "post_project keeps retention",
    "POST",
    "/api/v1/orgs/acme/projects",
    JSON.stringify({ name: "New", retention_days: 7 }),
    201,
    (json) => {
      expect(json.name).toBe("New");
      expect(json.retention_days).toBe(7);
    },
  ],
  [
    "put_project_member hides with none",
    "PUT",
    `${PROJECT}/members/${BOB_ID}`,
    JSON.stringify({ role: "none" }),
    200,
    (json) => {
      expect(json.override).toBe("none");
      expect("effective_role" in json).toBe(false);
    },
  ],
  [
    "put_project_member answers 404",
    "PUT",
    `${PROJECT}/members/${NEW_USER_ID}`,
    JSON.stringify({ role: "viewer" }),
    404,
  ],
  [
    "post_admin_user names and promotes",
    "POST",
    "/api/v1/admin/users",
    JSON.stringify({ email: "n@example.com", name: " N ", is_instance_admin: true }),
    201,
    (json) => {
      const user = part(json, "user");
      expect(user.email).toBe("n@example.com");
      expect(user.name).toBe("N");
      expect(user.is_instance_admin).toBe(true);
    },
  ],
  [
    "post_admin_user without a name",
    "POST",
    "/api/v1/admin/users",
    JSON.stringify({ email: "n@example.com" }),
    201,
    (json) => {
      const user = part(json, "user");
      expect("name" in user).toBe(false);
      expect(user.is_instance_admin).toBe(false);
    },
  ],
  [
    "patch_admin_user disables Bob",
    "PATCH",
    `/api/v1/admin/users/${BOB_ID}`,
    JSON.stringify({ disabled: true }),
    200,
    (json) => expect(json.disabled).toBe(true),
  ],
  [
    "patch_admin_user answers 404",
    "PATCH",
    `/api/v1/admin/users/${NEW_USER_ID}`,
    JSON.stringify({ disabled: true }),
    404,
  ],
  [
    "reset link for Bob",
    "POST",
    `/api/v1/admin/users/${BOB_ID}/reset-link`,
    undefined,
    200,
    (json) => expect(json.url).toBe(adminUserCreatedDocument.reset_url),
  ],
  [
    "test email without SMTP",
    "POST",
    "/api/v1/admin/settings/test-email",
    undefined,
    400,
    (json) => expect(json.detail).toBe("email is not configured"),
  ],
];

it.each(rows)(
  "%s",
  async (_label: string, method: string, path: string, requestBody:
    | string
    | undefined, status: number, check?: Check) => {
    const response = await send(method, path, requestBody);
    expect(response.status).toBe(status);
    check?.(response.json ?? {});
  },
);
