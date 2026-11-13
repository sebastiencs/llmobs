import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import apiKeyCreated from "../../../../fixtures/api/static/api-key-created.json";
import meFixture from "../../../../fixtures/api/static/me.json";
import orgFixture from "../../../../fixtures/api/static/org.json";
import projectFixture from "../../../../fixtures/api/static/project.json";

function handlers(
  options: { org?: () => Response; project?: () => Response; key?: () => Response } = {},
) {
  const orgBodies: unknown[] = [];
  const projectCalls: { org: string; body: unknown }[] = [];
  const keyCalls: { project: string; body: unknown }[] = [];
  server.use(
    http.post("/api/v1/orgs", async ({ request }) => {
      orgBodies.push(await request.json());
      return options.org?.() ?? HttpResponse.json(orgFixture, { status: 201 });
    }),
    http.post("/api/v1/orgs/:org/projects", async ({ params, request }) => {
      projectCalls.push({ org: String(params.org), body: await request.json() });
      return options.project?.() ?? HttpResponse.json(projectFixture, { status: 201 });
    }),
    http.post("/api/v1/projects/:project/api-keys", async ({ params, request }) => {
      keyCalls.push({
        project: decodeURIComponent(String(params.project)),
        body: await request.json(),
      });
      return options.key?.() ?? HttpResponse.json(apiKeyCreated, { status: 201 });
    }),
  );
  return { orgBodies, projectCalls, keyCalls };
}

async function fill(user: UserEvent) {
  await user.type(screen.getByLabelText("Organization name"), "Acme");
  await user.type(screen.getByLabelText("Project name"), "Support bot");
}

const create = (user: UserEvent, name = "Create workspace") =>
  user.click(screen.getByRole("button", { name }));

it("creates the org, the project and a secret key, then shows the key", async () => {
  const calls = handlers();
  const { user, queryClient } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByRole("heading", { name: "Send your first trace" })).toBeInTheDocument();
  expect(calls.orgBodies).toEqual([{ name: "Acme" }]);
  expect(calls.projectCalls).toEqual([{ org: "acme", body: { name: "Support bot" } }]);
  expect(calls.keyCalls).toEqual([
    { project: "acme/support-bot", body: { kind: "secret", name: "Default" } },
  ]);
  expect(screen.getByLabelText("Secret key")).toHaveValue(
    "sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0",
  );
  // gcTime 0 plus reset() drops the key mutation: its data holds the secret and its
  // variables the request that made it (`name: "Default"`). `reset()` alone would leave
  // both in the cache for the default five minutes.
  const cached = () =>
    JSON.stringify(
      queryClient
        .getMutationCache()
        .getAll()
        .map((mutation) => ({
          data: mutation.state.data ?? null,
          variables: mutation.state.variables ?? null,
        })),
    );
  await waitFor(() => {
    expect(cached()).not.toContain("sk-llmobs-EXAMPLE0");
    expect(cached()).not.toContain('"name":"Default"');
  });
});

it("does not create the org again when the project fails", async () => {
  let tries = 0;
  const calls = handlers({
    project: () =>
      ++tries === 1
        ? problemResponse(503, "busy")
        : HttpResponse.json(projectFixture, { status: 201 }),
  });
  const { user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByRole("alert")).toHaveTextContent("Service Unavailable");
  expect(screen.getByLabelText("Organization name")).toBeDisabled();
  expect(screen.getByText("Created.")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  await create(user, "Try again");
  expect(await screen.findByRole("heading", { name: "Send your first trace" })).toBeInTheDocument();
  expect(calls.orgBodies).toHaveLength(1);
  expect(calls.projectCalls).toHaveLength(2);
});

it("keeps going when me cannot be refreshed", async () => {
  const calls = handlers();
  let meCalls = 0;
  server.use(
    http.get("/api/auth/me", () => {
      meCalls += 1;
      return meCalls === 1 ? HttpResponse.json(meFixture) : problemResponse(500, "internal");
    }),
  );
  const { user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByRole("heading", { name: "Send your first trace" })).toBeInTheDocument();
  expect(calls.orgBodies).toHaveLength(1);
  expect(screen.queryByRole("alert")).toBeNull();
});

it("requires both names", async () => {
  const calls = handlers();
  const { user } = await renderApp("/setup/workspace");
  await create(user);
  expect(screen.getByText("Enter an organization name.")).toBeInTheDocument();
  expect(screen.getByText("Enter a project name.")).toBeInTheDocument();
  expect(calls.orgBodies).toHaveLength(0);
});

it("sends a signed-out visitor to login", async () => {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));
  const { router } = await renderApp("/setup/workspace");
  expect(router.state.location.pathname).toBe("/login");
  expect(await screen.findByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github?redirect=%2Fsetup%2Fworkspace",
  );
});

it("opens the new project after creating it", async () => {
  let hasOrg = false;
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json(hasOrg ? meFixture : { ...meFixture, orgs: [] }),
    ),
  );
  handlers({
    org: () => {
      hasOrg = true;
      return HttpResponse.json(orgFixture, { status: 201 });
    },
  });
  const { router, user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByRole("heading", { name: "Send your first trace" })).toBeInTheDocument();
  await user.click(screen.getByRole("link", { name: "Go to project" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme/support-bot/traces"));
  expect(screen.queryByRole("heading", { name: "Page not found" })).toBeNull();
});

it("shows an invalid organization name under its field", async () => {
  handlers({
    org: () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "name", message: "must be 1 to 100 characters" }],
      }),
  });
  const { user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByText("must be 1 to 100 characters")).toBeInTheDocument();
  expect(screen.getByLabelText("Organization name")).toHaveAttribute("aria-invalid", "true");
});

it("shows an invalid project name under its field", async () => {
  handlers({
    project: () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "name", message: "must be 1 to 100 characters" }],
      }),
  });
  const { user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByText("must be 1 to 100 characters")).toBeInTheDocument();
  expect(screen.getByLabelText("Project name")).toHaveAttribute("aria-invalid", "true");
  expect(screen.getByLabelText("Organization name")).toBeDisabled();
  expect(screen.getByText("Created.")).toBeInTheDocument();
});

it("retries only the key when the key fails", async () => {
  let tries = 0;
  const calls = handlers({
    key: () =>
      ++tries === 1
        ? problemResponse(500, "internal")
        : HttpResponse.json(apiKeyCreated, { status: 201 }),
  });
  const { user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByRole("alert")).toHaveTextContent("Internal Server Error");
  expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  await create(user, "Try again");
  expect(await screen.findByRole("heading", { name: "Send your first trace" })).toBeInTheDocument();
  expect(calls.orgBodies).toHaveLength(1);
  expect(calls.projectCalls).toHaveLength(1);
  expect(calls.keyCalls).toHaveLength(2);
});

it("explains rate limiting at any step", async () => {
  handlers({ org: () => problemResponse(429, "rate_limited") });
  const { user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Too many attempts. Wait a minute and try again.",
  );
  expect(screen.getByRole("button", { name: "Create workspace" })).toBeInTheDocument();
});

it("shows an unlisted failure", async () => {
  handlers({ project: () => problemResponse(503, "busy") });
  const { user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByRole("alert")).toHaveTextContent("Service Unavailable");
});

it("shows a fallback when the request never reached the server", async () => {
  handlers({ project: () => HttpResponse.error() });
  const { user } = await renderApp("/setup/workspace");
  await fill(user);
  await create(user);
  expect(await screen.findByRole("alert")).toHaveTextContent("Something went wrong. Try again.");
});
