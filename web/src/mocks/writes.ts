import { type HttpHandler, HttpResponse, http } from "msw";
import adminUserCreatedDocument from "../../../fixtures/api/static/admin-user-created.json";
import adminUsersDocument from "../../../fixtures/api/static/admin-users.json";
import apiKeyCreatedDocument from "../../../fixtures/api/static/api-key-created.json";
import inviteCreatedDocument from "../../../fixtures/api/static/invite-created.json";
import llmConnectionDocument from "../../../fixtures/api/static/llm-connection.json";
import llmConnectionTestDocument from "../../../fixtures/api/static/llm-connection-test.json";
import meDocument from "../../../fixtures/api/static/me.json";
import orgDocument from "../../../fixtures/api/static/org.json";
import orgMembersDocument from "../../../fixtures/api/static/org-members.json";
import personalTokenCreatedDocument from "../../../fixtures/api/static/personal-token-created.json";
import projectDocument from "../../../fixtures/api/static/project.json";
import projectMembersDocument from "../../../fixtures/api/static/project-members.json";
import scoreDefinitionDocument from "../../../fixtures/api/static/score-definition.json";
import setupResponseDocument from "../../../fixtures/api/static/setup-response.json";
import signupResponseDocument from "../../../fixtures/api/static/signup-response.json";

/**
 * Stateless write answers for the dev server's browser worker (D-L-10). They
 * are never installed in the test server: page tests mock writes per test.
 */
export type WriteMethod = "post" | "patch" | "put" | "delete";

export interface WriteOperation {
  operationId: string;
  method: WriteMethod;
  /** OpenAPI path, with `{param}` segments. */
  path: string;
}

export const WRITE_OPERATIONS: readonly WriteOperation[] = [
  { operationId: "post_login", method: "post", path: "/api/auth/login" },
  { operationId: "post_logout", method: "post", path: "/api/auth/logout" },
  { operationId: "post_signup", method: "post", path: "/api/auth/signup" },
  { operationId: "post_setup", method: "post", path: "/api/auth/setup" },
  { operationId: "post_password_forgot", method: "post", path: "/api/auth/password/forgot" },
  { operationId: "post_password_reset", method: "post", path: "/api/auth/password/reset" },
  { operationId: "post_email_verify", method: "post", path: "/api/auth/email/verify" },
  {
    operationId: "post_email_verify_resend",
    method: "post",
    path: "/api/auth/email/verify/resend",
  },
  { operationId: "post_invite_accept", method: "post", path: "/api/auth/invites/{token}/accept" },
  { operationId: "patch_me", method: "patch", path: "/api/v1/me" },
  { operationId: "post_me_password", method: "post", path: "/api/v1/me/password" },
  { operationId: "post_personal_token", method: "post", path: "/api/v1/me/tokens" },
  { operationId: "delete_current_token", method: "delete", path: "/api/v1/me/tokens/current" },
  { operationId: "delete_personal_token", method: "delete", path: "/api/v1/me/tokens/{id}" },
  { operationId: "post_org", method: "post", path: "/api/v1/orgs" },
  { operationId: "patch_org", method: "patch", path: "/api/v1/orgs/{org}" },
  { operationId: "patch_org_member", method: "patch", path: "/api/v1/orgs/{org}/members/{user}" },
  { operationId: "delete_org_member", method: "delete", path: "/api/v1/orgs/{org}/members/{user}" },
  { operationId: "post_org_invite", method: "post", path: "/api/v1/orgs/{org}/invites" },
  { operationId: "delete_org_invite", method: "delete", path: "/api/v1/orgs/{org}/invites/{id}" },
  {
    operationId: "post_llm_connection",
    method: "post",
    path: "/api/v1/orgs/{org}/llm-connections",
  },
  {
    operationId: "patch_llm_connection",
    method: "patch",
    path: "/api/v1/orgs/{org}/llm-connections/{id}",
  },
  {
    operationId: "delete_llm_connection",
    method: "delete",
    path: "/api/v1/orgs/{org}/llm-connections/{id}",
  },
  {
    operationId: "post_llm_connection_test",
    method: "post",
    path: "/api/v1/orgs/{org}/llm-connections/{id}/test",
  },
  { operationId: "post_project", method: "post", path: "/api/v1/orgs/{org}/projects" },
  { operationId: "patch_project", method: "patch", path: "/api/v1/projects/{project}" },
  { operationId: "post_api_key", method: "post", path: "/api/v1/projects/{project}/api-keys" },
  {
    operationId: "delete_api_key",
    method: "delete",
    path: "/api/v1/projects/{project}/api-keys/{id}",
  },
  {
    operationId: "put_project_member",
    method: "put",
    path: "/api/v1/projects/{project}/members/{user}",
  },
  {
    operationId: "delete_project_member",
    method: "delete",
    path: "/api/v1/projects/{project}/members/{user}",
  },
  {
    operationId: "post_score_definition",
    method: "post",
    path: "/api/v1/projects/{project}/score-definitions",
  },
  {
    operationId: "patch_score_definition",
    method: "patch",
    path: "/api/v1/projects/{project}/score-definitions/{id}",
  },
  {
    operationId: "delete_score_definition",
    method: "delete",
    path: "/api/v1/projects/{project}/score-definitions/{id}",
  },
  { operationId: "post_admin_user", method: "post", path: "/api/v1/admin/users" },
  { operationId: "patch_admin_user", method: "patch", path: "/api/v1/admin/users/{id}" },
  {
    operationId: "post_admin_user_reset_link",
    method: "post",
    path: "/api/v1/admin/users/{id}/reset-link",
  },
  {
    operationId: "post_admin_settings_test_email",
    method: "post",
    path: "/api/v1/admin/settings/test-email",
  },
];

type Respond = (input: {
  body: Record<string, unknown>;
  params: Record<string, string>;
}) => Response;

const ORG_ROLES = ["owner", "admin", "member", "viewer"] as const;
const PROJECT_OVERRIDES = ["admin", "member", "viewer", "none"] as const;

const RESPONSES: Record<string, Respond> = {
  post_login: () => HttpResponse.json(meDocument),
  post_logout: () => noContent(),
  post_signup: () => HttpResponse.json(signupResponseDocument, { status: 201 }),
  post_setup: () => HttpResponse.json(setupResponseDocument, { status: 201 }),
  post_password_forgot: () => noContent(),
  post_password_reset: () => noContent(),
  post_email_verify: () => noContent(),
  post_email_verify_resend: () => noContent(),
  post_invite_accept: () => HttpResponse.json(orgDocument),

  patch_me: ({ body }) => {
    const { name: _name, ...unnamed } = meDocument.user;
    const name = typeof body.name === "string" ? body.name.trim() : "";
    return HttpResponse.json(name ? { ...unnamed, name } : unnamed);
  },
  post_me_password: () => noContent(),
  post_personal_token: ({ body }) => {
    const created = personalTokenCreatedDocument;
    const expiry = typeof body.expires_at === "string" ? new Date(body.expires_at) : undefined;
    return HttpResponse.json(
      {
        ...created,
        token: {
          ...created.token,
          name: typeof body.name === "string" ? body.name : created.token.name,
          scopes: Array.isArray(body.scopes) ? body.scopes : created.token.scopes,
          ...(expiry !== undefined && !Number.isNaN(expiry.getTime())
            ? { expires_at: expiry.toISOString() }
            : {}),
        },
      },
      { status: 201 },
    );
  },
  delete_current_token: () => noContent(),
  delete_personal_token: () => noContent(),

  post_org: ({ body }) =>
    HttpResponse.json(
      { ...orgDocument, name: trimmed(body.name) ?? orgDocument.name },
      { status: 201 },
    ),
  patch_org: ({ body }) =>
    HttpResponse.json({ ...orgDocument, name: trimmed(body.name) ?? orgDocument.name }),
  patch_org_member: ({ body, params }) => {
    const member = orgMembersDocument.data.find((row) => row.user.id === params.user);
    if (!member) {
      return problem(404, "not_found");
    }
    const role = ORG_ROLES.find((candidate) => candidate === body.role);
    if (!role) {
      return problem(400, "bad_request");
    }
    return HttpResponse.json({ ...member, role });
  },
  delete_org_member: () => noContent(),
  delete_org_invite: () => noContent(),
  post_org_invite: ({ body }) =>
    HttpResponse.json(
      {
        ...inviteCreatedDocument,
        invite: {
          ...inviteCreatedDocument.invite,
          email: typeof body.email === "string" ? body.email : inviteCreatedDocument.invite.email,
          role: typeof body.role === "string" ? body.role : inviteCreatedDocument.invite.role,
        },
      },
      { status: 201 },
    ),
  post_llm_connection: ({ body }) =>
    HttpResponse.json({ ...llmConnectionDocument, ...named(body) }, { status: 201 }),
  patch_llm_connection: ({ body }) =>
    HttpResponse.json({ ...llmConnectionDocument, ...named(body) }),
  delete_llm_connection: () => noContent(),
  post_llm_connection_test: () => HttpResponse.json(llmConnectionTestDocument),
  post_project: ({ body }) =>
    HttpResponse.json(
      {
        ...projectDocument,
        name: trimmed(body.name) ?? projectDocument.name,
        ...(typeof body.retention_days === "number" ? { retention_days: body.retention_days } : {}),
      },
      { status: 201 },
    ),

  patch_project: ({ body }) => {
    const { retention_days: _retention, ...withoutRetention } = projectDocument;
    const result: Record<string, unknown> =
      "retention_days" in body && body.retention_days === null
        ? { ...withoutRetention }
        : { ...projectDocument };
    if (typeof body.retention_days === "number") {
      result.retention_days = body.retention_days;
    }
    const name = trimmed(body.name);
    if (name !== undefined) {
      result.name = name;
    }
    return HttpResponse.json(result);
  },
  post_api_key: ({ body }) => {
    const kind = body.kind === "public" ? "public" : "secret";
    const scopes = Array.isArray(body.scopes)
      ? body.scopes
      : kind === "public"
        ? ["feedback"]
        : ["ingest", "read", "write"];
    const { name: _name, ...unnamedKey } = apiKeyCreatedDocument.key;
    const name = trimmed(body.name);
    return HttpResponse.json(
      {
        key: {
          ...unnamedKey,
          kind,
          scopes,
          prefix: kind === "public" ? "pk-llmobs-EXAMPL" : "sk-llmobs-EXAMPL",
          ...(name !== undefined ? { name } : {}),
        },
        secret:
          kind === "public"
            ? "pk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0"
            : apiKeyCreatedDocument.secret,
      },
      { status: 201 },
    );
  },
  delete_api_key: () => noContent(),
  put_project_member: ({ body, params }) => {
    const member = projectMembersDocument.data.find((row) => row.user.id === params.user);
    if (!member) {
      return problem(404, "not_found");
    }
    if (member.org_role === "owner" || member.org_role === "admin") {
      return problem(400, "bad_request", "owners and admins cannot be overridden");
    }
    const role = PROJECT_OVERRIDES.find((candidate) => candidate === body.role);
    if (!role) {
      return problem(400, "bad_request");
    }
    return HttpResponse.json({
      user: member.user,
      org_role: member.org_role,
      override: role,
      ...(role !== "none" ? { effective_role: role } : {}),
    });
  },
  delete_project_member: () => noContent(),
  post_score_definition: ({ body }) =>
    HttpResponse.json({ ...scoreDefinitionDocument, ...named(body) }, { status: 201 }),
  patch_score_definition: ({ body }) =>
    HttpResponse.json({ ...scoreDefinitionDocument, ...named(body) }),
  delete_score_definition: () => noContent(),

  post_admin_user: ({ body }) => {
    const { name: _name, ...unnamedUser } = adminUserCreatedDocument.user;
    const name = trimmed(body.name);
    return HttpResponse.json(
      {
        reset_url: adminUserCreatedDocument.reset_url,
        user: {
          ...unnamedUser,
          email: typeof body.email === "string" ? body.email : adminUserCreatedDocument.user.email,
          is_instance_admin: body.is_instance_admin === true,
          ...(name !== undefined ? { name } : {}),
        },
      },
      { status: 201 },
    );
  },
  patch_admin_user: ({ body, params }) => {
    if (params.id === meDocument.user.id) {
      return problem(400, "bad_request", "cannot change your own account");
    }
    const user = adminUsersDocument.data.find((row) => row.id === params.id);
    if (!user) {
      return problem(404, "not_found");
    }
    return HttpResponse.json({
      ...user,
      ...(typeof body.disabled === "boolean" ? { disabled: body.disabled } : {}),
      ...(typeof body.is_instance_admin === "boolean"
        ? { is_instance_admin: body.is_instance_admin }
        : {}),
    });
  },
  post_admin_user_reset_link: ({ params }) => {
    const user = adminUsersDocument.data.find((row) => row.id === params.id);
    if (!user) {
      return problem(404, "not_found");
    }
    return HttpResponse.json({ url: adminUserCreatedDocument.reset_url });
  },
  post_admin_settings_test_email: () => problem(400, "bad_request", "email is not configured"),
};

export const writeHandlers: HttpHandler[] = WRITE_OPERATIONS.map((operation) => {
  const respond = RESPONSES[operation.operationId];
  if (!respond) {
    throw new Error(`writes: no response for ${operation.operationId}`);
  }
  const path = operation.path.replace(/\{([^}]+)\}/g, ":$1");
  return http[operation.method](path, async ({ request, params }) =>
    respond({ body: (await readJson(request)) ?? {}, params: stringParams(params) }),
  );
});

function noContent(): Response {
  return new HttpResponse(null, { status: 204 });
}

/** The trimmed value when it is a non-empty string, else `undefined`. */
function trimmed(value: unknown): string | undefined {
  if (typeof value !== "string" || value.trim().length === 0) {
    return undefined;
  }
  return value.trim();
}

/** The connection fixture's overrides, with `name` from the body when it is a string. */
function named(body: Record<string, unknown>): { name: string } | Record<string, never> {
  return typeof body.name === "string" ? { name: body.name } : {};
}

const TITLES: Record<400 | 404, string> = { 400: "Bad Request", 404: "Not Found" };

function problem(status: 400 | 404, code: string, detail?: string): Response {
  return HttpResponse.json(
    {
      type: "about:blank",
      title: TITLES[status],
      status,
      code,
      ...(detail !== undefined ? { detail } : {}),
    },
    { status, headers: { "Content-Type": "application/problem+json" } },
  );
}

function stringParams(params: Record<string, unknown>): Record<string, string> {
  const pathParams: Record<string, string> = {};
  for (const [key, value] of Object.entries(params)) {
    if (typeof value === "string") {
      pathParams[key] = value;
    } else if (Array.isArray(value) && typeof value[0] === "string") {
      pathParams[key] = value[0];
    }
  }
  return pathParams;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function readJson(request: Request): Promise<Record<string, unknown> | undefined> {
  try {
    const body: unknown = await request.json();
    return isObject(body) ? body : undefined;
  } catch {
    return undefined;
  }
}
