import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import templatesFixture from "../../../../../../fixtures/api/static/evaluator-templates.json";
import connectionsFixture from "../../../../../../fixtures/api/static/llm-connections.json";

const RELEVANCE = templatesFixture.data.find((template) => template.id === "relevance");
const CONNECTION = connectionsFixture.data[0];
if (!CONNECTION) {
  throw new Error("the connection fixture is empty");
}
const EVALUATOR_ID = "01900000-0000-7000-8000-000000000500";

/** Picks an option of the Radix select named `name`. */
async function choose(user: UserEvent, name: string, option: string) {
  await user.click(screen.getByRole("combobox", { name }));
  await user.click(await screen.findByRole("option", { name: option }));
}

/** Records the body of the next create request and answers it with the fixture evaluator. */
function captureCreate(): { body: () => unknown; calls: () => number } {
  let body: unknown;
  let calls = 0;
  server.use(
    http.post("/api/v1/projects/:project/evaluators", async ({ request }) => {
      calls += 1;
      body = await request.json();
      return HttpResponse.json(
        { ...(body as Record<string, unknown>), id: EVALUATOR_ID, version: 1 },
        { status: 201 },
      );
    }),
  );
  return { body: () => body, calls: () => calls };
}

it("creates a judge from a template and opens it", async () => {
  if (!RELEVANCE) {
    throw new Error("the relevance template is missing from the fixtures");
  }
  const created = captureCreate();
  const { user, router } = await renderApp("/acme/support-bot/evaluators/new");

  await choose(user, "Start from a template", "Relevance");
  expect(screen.getByLabelText("Name")).toHaveValue("Relevance");
  await choose(user, "Connection", CONNECTION.name);
  expect(screen.getByLabelText("Model")).toHaveValue(CONNECTION.default_model);
  await user.click(screen.getByRole("button", { name: "Create evaluator" }));

  await waitFor(() =>
    expect(created.body()).toEqual({
      name: "Relevance",
      kind: "llm_judge",
      config: {
        connection_id: CONNECTION.id,
        model: CONNECTION.default_model,
        prompt: RELEVANCE.prompt,
        output: { type: "numeric", min: 0, max: 1 },
        include_reason: true,
      },
      output_score: { name: "relevance", data_type: "numeric" },
    }),
  );
  await waitFor(() =>
    expect(router.state.location.pathname).toBe(`/acme/support-bot/evaluators/${EVALUATOR_ID}`),
  );
});

it("creates a contains check that needs all of its texts", async () => {
  const created = captureCreate();
  const { user } = await renderApp("/acme/support-bot/evaluators/new");

  await user.click(screen.getByRole("radio", { name: "Check" }));
  await user.type(screen.getByLabelText("Name"), "Refund");
  await choose(user, "Check", "Contains");
  await user.type(screen.getByLabelText("Text to find 1"), "refund");
  await user.click(screen.getByRole("radio", { name: "All" }));
  await user.type(screen.getByLabelText("Score name"), "refund_mentioned");
  await user.click(screen.getByRole("button", { name: "Create evaluator" }));

  await waitFor(() =>
    expect(created.body()).toEqual({
      name: "Refund",
      kind: "check",
      config: { check: "contains", values: ["refund"], mode: "all", case_sensitive: false },
      output_score: { name: "refund_mentioned", data_type: "boolean" },
    }),
  );
});

it("creates a regex check", async () => {
  const created = captureCreate();
  const { user } = await renderApp("/acme/support-bot/evaluators/new");

  await user.click(screen.getByRole("radio", { name: "Check" }));
  await user.type(screen.getByLabelText("Name"), "Starts ok");
  await choose(user, "Check", "Regex");
  await user.type(screen.getByLabelText("Pattern"), "^ok");
  await user.type(screen.getByLabelText("Score name"), "starts_ok");
  await user.click(screen.getByRole("button", { name: "Create evaluator" }));

  await waitFor(() =>
    expect(created.body()).toEqual({
      name: "Starts ok",
      kind: "check",
      config: { check: "regex", pattern: "^ok" },
      output_score: { name: "starts_ok", data_type: "boolean" },
    }),
  );
});

it("creates a webhook with its signing secret, and hides the header values", async () => {
  const created = captureCreate();
  const { user } = await renderApp("/acme/support-bot/evaluators/new");

  await user.click(screen.getByRole("radio", { name: "Webhook" }));
  await user.type(screen.getByLabelText("Name"), "Hook");
  await user.type(screen.getByLabelText("URL"), "https://hooks.example.com/eval");
  await user.click(screen.getByRole("button", { name: "Add header" }));
  await user.type(screen.getByLabelText("Header name 1"), "Authorization");
  const value = screen.getByLabelText("Header value 1");
  expect(value).toHaveAttribute("type", "password");
  await user.type(value, "Bearer x");
  await user.type(screen.getByLabelText("Signing secret"), "0123456789abcdef");
  await user.type(screen.getByLabelText("Score name"), "hook_ok");
  await user.click(screen.getByRole("button", { name: "Create evaluator" }));

  await waitFor(() =>
    expect(created.body()).toEqual({
      name: "Hook",
      kind: "webhook",
      config: { url: "https://hooks.example.com/eval", headers: { Authorization: "Bearer x" } },
      output_score: { name: "hook_ok", data_type: "numeric" },
      webhook_secret: "0123456789abcdef",
    }),
  );
});

it("refuses a bad schema and a short secret without calling the server", async () => {
  const created = captureCreate();
  const { user } = await renderApp("/acme/support-bot/evaluators/new");

  await user.click(screen.getByRole("radio", { name: "Check" }));
  await choose(user, "Check", "JSON Schema");
  // `[[` types one `[` in user-event's key syntax.
  await user.type(screen.getByLabelText("JSON Schema"), "[[1]");
  await user.click(screen.getByRole("button", { name: "Create evaluator" }));
  expect(await screen.findByText("The schema must be a JSON object.")).toBeInTheDocument();
  expect(screen.getByText("Enter a name.")).toBeInTheDocument();

  await user.click(screen.getByRole("radio", { name: "Webhook" }));
  await user.type(screen.getByLabelText("Signing secret"), "short");
  await user.click(screen.getByRole("button", { name: "Create evaluator" }));
  expect(
    await screen.findByText("Enter a signing secret of 16 to 256 characters."),
  ).toBeInTheDocument();
  expect(created.calls()).toBe(0);
});

it("puts server field errors under their controls", async () => {
  server.use(
    http.post("/api/v1/projects/:project/evaluators", () =>
      problemResponse(400, "bad_request", {
        errors: [
          { field: "config.prompt", message: "prompt is too long" },
          {
            field: "output_score.data_type",
            message: "data_type must be numeric for this evaluator",
          },
        ],
      }),
    ),
  );
  const { user } = await renderApp("/acme/support-bot/evaluators/new");

  await user.type(screen.getByLabelText("Name"), "Judge");
  await choose(user, "Connection", CONNECTION.name);
  await user.type(screen.getByLabelText("Prompt"), "Rate the output");
  await user.type(screen.getByLabelText("Score name"), "rate");
  await user.click(screen.getByRole("button", { name: "Create evaluator" }));

  const prompt = await screen.findByText("prompt is too long");
  expect(prompt.closest("div")).toContainElement(screen.getByLabelText("Prompt"));
  expect(
    await screen.findByText("data_type must be numeric for this evaluator"),
  ).toBeInTheDocument();
});
