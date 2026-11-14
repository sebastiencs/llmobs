import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import evaluatorFixture from "../../../../fixtures/api/static/evaluator.json";
import resultFixture from "../../../../fixtures/api/static/evaluator-test-result.json";
import projectFixture from "../../../../fixtures/api/static/project.json";

const EVALUATOR_ID = "01900000-0000-7000-8000-000000000500";
const EVALUATOR_URL = `/acme/support-bot/evaluators/${EVALUATOR_ID}`;
const TRACE_ID = "0123456789abcdef0123456789abcdef";

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

/** Records the body of the next test run and answers it with `answer`. */
function captureRun(answer: () => Response = () => HttpResponse.json(resultFixture)) {
  let body: Record<string, unknown> | undefined;
  let calls = 0;
  server.use(
    http.post("/api/v1/projects/:project/evaluators/test", async ({ request }) => {
      calls += 1;
      body = (await request.json()) as Record<string, unknown>;
      return answer();
    }),
  );
  return { body: () => body, calls: () => calls };
}

function projectAs(role: string) {
  return http.get("/api/v1/projects/:project", () =>
    HttpResponse.json({ ...projectFixture, role }),
  );
}

it("runs a saved evaluator on a trace", async () => {
  const run = captureRun();
  const { user } = await renderApp(EVALUATOR_URL);

  await user.type(await screen.findByLabelText("Trace ID"), TRACE_ID);
  await user.click(screen.getByRole("button", { name: "Run test" }));

  await waitFor(() =>
    expect(run.body()).toEqual({
      evaluator_id: EVALUATOR_ID,
      trace_id: TRACE_ID,
      variable_mapping: { input: "trace.input", output: "trace.output" },
    }),
  );
  expect(await screen.findByText("0.900")).toBeInTheDocument();
  expect(screen.getByText("The answer addresses the question.")).toBeInTheDocument();
  expect(screen.getByText(/120 in · 18 out/)).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "View judge trace" })).toHaveAttribute(
    "href",
    "/acme/support-bot/traces/e1000000000000000000000000000001",
  );
});

it("runs the unsaved form with its changes", async () => {
  const run = captureRun();
  const { user } = await renderApp(EVALUATOR_URL);

  const prompt = await screen.findByLabelText("Prompt");
  await user.clear(prompt);
  // user-event reads `{` as a key, so each brace of the prompt is doubled.
  await user.type(prompt, "Rate {{{{ input }} against {{{{ output }}.");
  await user.type(await screen.findByLabelText("Trace ID"), TRACE_ID);
  await user.click(screen.getByRole("button", { name: "Run test" }));

  await waitFor(() => expect(run.calls()).toBe(1));
  expect(run.body()).toMatchObject({
    evaluator_id: EVALUATOR_ID,
    evaluator: {
      kind: "llm_judge",
      name: "Relevance",
      config: { prompt: "Rate {{ input }} against {{ output }}." },
      output_score: { name: "relevance", data_type: "numeric" },
    },
    trace_id: TRACE_ID,
  });
});

it("runs on literal variables", async () => {
  const run = captureRun();
  const { user } = await renderApp(EVALUATOR_URL);

  await user.click(await screen.findByRole("tab", { name: "Variables" }));
  await user.type(screen.getByLabelText("Variable name 1"), "output");
  await user.type(screen.getByLabelText("Variable value 1"), "30 days.");
  await user.click(screen.getByRole("button", { name: "Add variable" }));
  await user.type(screen.getByLabelText("Variable name 2"), "expected");
  await user.type(screen.getByLabelText("Variable value 2"), '{{"a": 1}');
  await user.click(screen.getByRole("button", { name: "Run test" }));

  await waitFor(() =>
    expect(run.body()).toEqual({
      evaluator_id: EVALUATOR_ID,
      variables: { output: "30 days.", expected: { a: 1 } },
    }),
  );
});

it("shows a failure and the hint of its class", async () => {
  captureRun(() =>
    HttpResponse.json({
      ok: false,
      latency_ms: 4,
      trace_truncated: false,
      variables: {},
      error: { class: "pause", message: "provider status 401" },
    }),
  );
  const { user } = await renderApp(EVALUATOR_URL);

  await user.type(await screen.findByLabelText("Trace ID"), TRACE_ID);
  await user.click(screen.getByRole("button", { name: "Run test" }));

  expect(await screen.findByText("Test failed")).toBeInTheDocument();
  expect(screen.getByText("provider status 401")).toBeInTheDocument();
  expect(
    screen.getByText(
      "The key, quota or model access was refused. Check the connection in Settings › LLM connections.",
    ),
  ).toBeInTheDocument();
});

it("tests a saved webhook without resending its settings", async () => {
  server.use(
    http.get("/api/v1/projects/:project/evaluators/:id", () => HttpResponse.json(WEBHOOK)),
  );
  const run = captureRun();
  const { user } = await renderApp(`/acme/support-bot/evaluators/${WEBHOOK.id}`);

  const score = await screen.findByLabelText("Score name");
  await user.clear(score);
  await user.type(score, "hook_ok");
  await user.click(screen.getByRole("tab", { name: "Variables" }));
  await user.click(screen.getByRole("button", { name: "Run test" }));

  await waitFor(() => expect(run.calls()).toBe(1));
  const body = run.body() ?? {};
  expect(body.evaluator_id).toBe(WEBHOOK.id);
  expect(body.evaluator).toEqual({
    kind: "webhook",
    name: "Hook",
    output_score: { name: "hook_ok", data_type: "boolean" },
  });
});

it("blocks a run with a bad trace id", async () => {
  const run = captureRun();
  const { user } = await renderApp(EVALUATOR_URL);

  await user.type(await screen.findByLabelText("Trace ID"), "not-a-trace");
  await user.click(screen.getByRole("button", { name: "Run test" }));

  expect(await screen.findByText("Enter a 32-digit hex trace ID.")).toBeInTheDocument();
  expect(run.calls()).toBe(0);
});

it("shows the wait message when the rate limit answers", async () => {
  captureRun(() => problemResponse(429, "rate_limited"));
  const { user } = await renderApp(EVALUATOR_URL);

  await user.type(await screen.findByLabelText("Trace ID"), TRACE_ID);
  await user.click(screen.getByRole("button", { name: "Run test" }));

  expect(await screen.findByText("Too many tests. Wait a minute.")).toBeInTheDocument();
});

it("shows no test panel to a viewer", async () => {
  server.use(projectAs("viewer"));
  await renderApp(EVALUATOR_URL);

  expect(await screen.findByLabelText("Name")).toBeDisabled();
  const panel = screen.queryByRole("region", { name: "Test" });
  expect(panel).toBeNull();
  expect(screen.queryByRole("button", { name: "Run test" })).toBeNull();
  expect(within(document.body).queryByLabelText("Trace ID")).toBeNull();
});
