import { type HttpHandler, HttpResponse, http } from "msw";
import meDocument from "../../../fixtures/api/static/me.json";
import seedDocument from "../../../fixtures/api/static/saved-views.json";

export const FIXTURE_USER_ID = meDocument.user.id;

type StoredView = {
  id: string;
  kind: string;
  name: string;
  state: Record<string, unknown>;
  owner_user_id?: string;
  shared: boolean;
  is_default: boolean;
  created_at: string;
  updated_at: string;
};

const LIST = "/api/v1/projects/:project/saved-views";
const ONE = "/api/v1/projects/:project/saved-views/:id";

let views: StoredView[] = [];

export function seedSavedViews(): StoredView[] {
  return seedDocument.data.map((view) => ({
    id: view.id,
    kind: view.kind,
    name: view.name,
    state: { ...view.state },
    owner_user_id: view.owner_user_id,
    shared: view.shared,
    is_default: view.is_default,
    created_at: view.created_at,
    updated_at: view.updated_at,
  }));
}

/** Handlers for one in-memory store. Calling this replaces the store with `initial`. */
export function savedViewHandlers(initial: readonly StoredView[]): HttpHandler[] {
  views = initial.map((view) => structuredClone(view));
  return handlers;
}

const handlers: HttpHandler[] = [
  http.get(LIST, ({ request }) => {
    const url = new URL(request.url);
    if (url.searchParams.get("cursor")) {
      return HttpResponse.json({ data: [], next_cursor: null });
    }
    const kind = url.searchParams.get("kind");
    const data = views.filter((view) => visible(view) && (kind === null || view.kind === kind));
    return HttpResponse.json({ data, next_cursor: null });
  }),
  http.post(LIST, async ({ request }) => {
    const body = await readJson(request);
    const name = typeof body?.name === "string" ? body.name.trim() : "";
    const kind = typeof body?.kind === "string" ? body.kind : "";
    if (!body || name.length === 0 || kind.length === 0 || !isObject(body.state)) {
      return problem(400, "bad_request", "Bad Request");
    }
    const now = new Date().toISOString();
    const view: StoredView = {
      id: crypto.randomUUID(),
      kind,
      name,
      state: body.state,
      owner_user_id: FIXTURE_USER_ID,
      shared: body.shared === true,
      is_default: body.is_default === true,
      created_at: now,
      updated_at: now,
    };
    if (view.is_default) {
      clearDefault(view.kind, FIXTURE_USER_ID, view.id);
    }
    views.push(view);
    return HttpResponse.json(view, { status: 201 });
  }),
  http.patch(ONE, async ({ params, request }) => {
    const view = views.find((item) => item.id === params.id);
    if (!view) {
      return notFound();
    }
    const body = await readJson(request);
    if (!body) {
      return problem(400, "bad_request", "Bad Request");
    }
    if (typeof body.name === "string" && body.name.trim().length > 0) {
      view.name = body.name.trim();
    }
    if (typeof body.shared === "boolean") {
      view.shared = body.shared;
    }
    if (isObject(body.state)) {
      view.state = body.state;
    }
    if (typeof body.is_default === "boolean") {
      view.is_default = body.is_default;
      if (body.is_default && view.owner_user_id) {
        clearDefault(view.kind, view.owner_user_id, view.id);
      }
    }
    view.updated_at = new Date().toISOString();
    return HttpResponse.json(view);
  }),
  http.delete(ONE, ({ params }) => {
    const index = views.findIndex((item) => item.id === params.id);
    if (index < 0) {
      return notFound();
    }
    views.splice(index, 1);
    return new HttpResponse(null, { status: 204 });
  }),
];

function visible(view: StoredView): boolean {
  return view.shared || view.owner_user_id === undefined || view.owner_user_id === FIXTURE_USER_ID;
}

/** One default per owner per kind. A shared default stays for users who have none of their own. */
function clearDefault(kind: string, ownerId: string, exceptId: string): void {
  for (const view of views) {
    if (view.id !== exceptId && view.kind === kind && view.owner_user_id === ownerId) {
      view.is_default = false;
    }
  }
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

function notFound(): Response {
  return problem(404, "not_found", "Not Found");
}

function problem(status: number, code: string, title: string): Response {
  return HttpResponse.json(
    { type: "about:blank", title, status, code },
    { status, headers: { "Content-Type": "application/problem+json" } },
  );
}
