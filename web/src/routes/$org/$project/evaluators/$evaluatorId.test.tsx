import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import evaluatorFixture from "../../../../../../fixtures/api/static/evaluator.json";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

const EVALUATOR_URL = "/acme/support-bot/evaluators/01900000-0000-7000-8000-000000000500";

const WEBHOOK = {
  ...evaluatorFixture,
  id: "01900000-0000-7000-8000-000000000502",
  name: "Hook",
  kind: "webhook",
  config: { url_host: "hooks.example.com", timeout_ms: 5000, header_names: ["x-token"] },
  output_score: { name: "hook", data_type: "boolean" },
  has_signing_secret: true,
  version: 3,
};

/** Records the body of the next PATCH of an evaluator. */
function capturePatch(answer: (body: Record<string, unknown>) => Record<string, unknown>) {
  let body: Record<string, unknown> | undefined;
  server.use(
    http.patch("/api/v1/projects/:project/evaluators/:id", async ({ request }) => {
      body = (await request.json()) as Record<string, unknown>;
      return HttpResponse.json(answer(body));
    }),
  );
  return () => body;
}

function projectAs(role: string) {
  return http.get("/api/v1/projects/:project", () =>
    HttpResponse.json({ ...projectFixture, role }),
  );
}

it("sends the whole config when the prompt changes", async () => {
  const patched = capturePatch((body) => ({ ...evaluatorFixture, ...body }));
  const { user } = await renderApp(EVALUATOR_URL);

  const prompt = await screen.findByLabelText("Prompt");
  await user.clear(prompt);
  await user.type(prompt, "Rate the answer.");
  await user.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() =>
    expect(patched()).toEqual({
      config: {
        connection_id: evaluatorFixture.config.connection_id,
        model: "gpt-5-mini",
        prompt: "Rate the answer.",
        output: { type: "numeric", min: 0, max: 1 },
        include_reason: true,
        max_output_tokens: 1024,
        timeout_ms: 60000,
      },
    }),
  );
});

it("renames only when just the name changes", async () => {
  const patched = capturePatch((body) => ({ ...evaluatorFixture, ...body }));
  const { user } = await renderApp(EVALUATOR_URL);

  const name = await screen.findByLabelText("Name");
  await user.clear(name);
  await user.type(name, "Relevance v2");
  await user.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => expect(patched()).toEqual({ name: "Relevance v2" }));
});

it("sends no webhook settings or secret unless they are replaced", async () => {
  server.use(
    http.get("/api/v1/projects/:project/evaluators/:id", () => HttpResponse.json(WEBHOOK)),
  );
  const patched = capturePatch((body) => ({ ...WEBHOOK, ...body }));
  const { user } = await renderApp(`/acme/support-bot/evaluators/${WEBHOOK.id}`);

  expect(await screen.findByText("Endpoint host: hooks.example.com")).toBeInTheDocument();
  const score = screen.getByLabelText("Score name");
  await user.clear(score);
  await user.type(score, "hook_ok");
  await user.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() =>
    expect(patched()).toEqual({
      output_score: { name: "hook_ok", data_type: "boolean" },
    }),
  );
});

it("replaces the webhook settings and the secret together", async () => {
  server.use(
    http.get("/api/v1/projects/:project/evaluators/:id", () => HttpResponse.json(WEBHOOK)),
  );
  const patched = capturePatch((body) => ({ ...WEBHOOK, ...body }));
  const { user } = await renderApp(`/acme/support-bot/evaluators/${WEBHOOK.id}`);

  await user.click(await screen.findByRole("checkbox", { name: "Replace webhook settings" }));
  await user.type(screen.getByLabelText("URL"), "https://hooks.example.com/v2");
  await user.type(screen.getByLabelText("Header value 1"), "abc");
  await user.type(screen.getByLabelText("Signing secret"), "fedcba9876543210");
  await user.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() =>
    expect(patched()).toEqual({
      config: {
        url: "https://hooks.example.com/v2",
        timeout_ms: 5000,
        headers: { "x-token": "abc" },
      },
      webhook_secret: "fedcba9876543210",
    }),
  );
});

it("deletes after confirming and returns to the list", async () => {
  let deleted = false;
  server.use(
    http.delete("/api/v1/projects/:project/evaluators/:id", () => {
      deleted = true;
      return new Response(null, { status: 204 });
    }),
  );
  const { user, router } = await renderApp(EVALUATOR_URL);

  await user.click(await screen.findByRole("button", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete Relevance?" });
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));

  await waitFor(() => expect(deleted).toBe(true));
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme/support-bot/evaluators"));
});

it("shows a viewer a read-only form", async () => {
  server.use(projectAs("viewer"));
  await renderApp(EVALUATOR_URL);

  expect(await screen.findByLabelText("Name")).toBeDisabled();
  expect(screen.getByLabelText("Prompt")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Delete" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Save" }).parentElement).toHaveAccessibleDescription(
    "Only members can edit evaluators",
  );
});

it("shows the not-found page for an unknown evaluator", async () => {
  server.use(
    http.get("/api/v1/projects/:project/evaluators/:id", () => problemResponse(404, "not_found")),
  );
  await renderApp(EVALUATOR_URL);

  expect(await screen.findByRole("heading", { name: "Page not found" })).toBeInTheDocument();
});
