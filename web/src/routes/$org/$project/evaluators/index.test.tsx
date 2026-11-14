import { screen, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";
import evaluatorsFixture from "../../../../../../fixtures/api/static/evaluators.json";

/** The table row whose name cell reads `name`. */
function rowOf(table: HTMLElement, name: string): HTMLElement {
  const row = within(table).getByText(name).closest("tr");
  if (!row) {
    throw new Error(`no row named ${name}`);
  }
  return row;
}

it("lists evaluators with their kind, score and details", async () => {
  await renderApp("/acme/support-bot/evaluators");

  const table = await screen.findByRole("table", { name: "Evaluators" });
  const row = rowOf(table, "Relevance");
  expect(within(row).getByText("LLM judge")).toBeInTheDocument();
  expect(within(row).getByText("relevance")).toBeInTheDocument();
  expect(within(row).getByText("numeric")).toBeInTheDocument();
  expect(within(row).getByText("gpt-5-mini")).toBeInTheDocument();
  expect(within(row).getByText("1")).toBeInTheDocument();
});

it("shows the empty state when the project has no evaluators", async () => {
  server.use(
    http.get("/api/v1/projects/:project/evaluators", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );

  await renderApp("/acme/support-bot/evaluators");

  expect(await screen.findByText("No evaluators yet")).toBeInTheDocument();
  expect(screen.queryByRole("table", { name: "Evaluators" })).toBeNull();
});

it("has no create button until the editor exists", async () => {
  await renderApp("/acme/support-bot/evaluators");

  await screen.findByRole("table", { name: "Evaluators" });
  expect(screen.queryByRole("button", { name: "New evaluator" })).toBeNull();
});

it("shows a check's function and a webhook's host by kind", async () => {
  server.use(
    http.get("/api/v1/projects/:project/evaluators", () =>
      HttpResponse.json({
        data: [
          {
            ...evaluatorsFixture.data[0],
            id: "01900000-0000-7000-8000-000000000501",
            name: "Exact match",
            kind: "check",
            config: { check: "exact_match" },
          },
          {
            ...evaluatorsFixture.data[0],
            id: "01900000-0000-7000-8000-000000000502",
            name: "Hook",
            kind: "webhook",
            config: { url_host: "hooks.example.com", timeout_ms: 5000, header_names: [] },
            has_signing_secret: true,
            version: 3,
          },
        ],
        next_cursor: null,
      }),
    ),
  );

  await renderApp("/acme/support-bot/evaluators");

  const table = await screen.findByRole("table", { name: "Evaluators" });
  const check = rowOf(table, "Exact match");
  expect(within(check).getByText("Check")).toBeInTheDocument();
  expect(within(check).getByText("exact_match")).toBeInTheDocument();
  const hook = rowOf(table, "Hook");
  expect(within(hook).getByText("Webhook")).toBeInTheDocument();
  expect(within(hook).getByText("hooks.example.com")).toBeInTheDocument();
  expect(within(hook).getByText("3")).toBeInTheDocument();
});
