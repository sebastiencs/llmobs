import { screen, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { FIRING_RULE_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import rulesFixture from "../../../../../../fixtures/api/static/alert-rules.json";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

const PAGE = "/acme/support-bot/alerts";
const RULES = "/api/v1/projects/:project/alert-rules";

it("lists the rules with their state", async () => {
  await renderApp(PAGE);

  const table = await screen.findByRole("table", { name: "Alert rules" });
  expect(within(table).getAllByRole("row").slice(1)).toHaveLength(3);

  const high = within(table).getByRole("row", { name: /^High error rate\b/ });
  expect(high).toHaveTextContent("Firing");
  expect(high).toHaveTextContent("Error rate > 5.0% over 5m");
  expect(high).toHaveTextContent("12.5%");
  expect(within(high).getAllByRole("cell").at(-1)).toHaveTextContent(/^2$/);

  const daily = within(table).getByRole("row", { name: /^Daily cost\b/ });
  expect(daily).toHaveTextContent("Disabled");

  const helpfulness = within(table).getByRole("row", { name: /^Low helpfulness\b/ });
  expect(helpfulness).toHaveTextContent("No data");
});

it("links to the rule page", async () => {
  await renderApp(PAGE);

  const table = await screen.findByRole("table", { name: "Alert rules" });
  expect(within(table).getByRole("link", { name: "High error rate" })).toHaveAttribute(
    "href",
    `/acme/support-bot/alerts/${FIRING_RULE_ID}`,
  );
});

it("shows a rule that was never evaluated as never", async () => {
  await renderApp(PAGE);

  const table = await screen.findByRole("table", { name: "Alert rules" });
  const daily = within(table).getByRole("row", { name: /^Daily cost\b/ });
  expect(within(daily).getByText("Never")).toBeInTheDocument();
});

it("flags a failed evaluation", async () => {
  server.use(
    http.get(RULES, () =>
      HttpResponse.json({
        ...rulesFixture,
        data: [{ ...rulesFixture.data[1], last_error: "query timed out" }],
      }),
    ),
  );

  await renderApp(PAGE);

  expect(await screen.findByLabelText("Evaluation failed")).toBeInTheDocument();
});

it("shows the empty state", async () => {
  server.use(http.get(RULES, () => HttpResponse.json({ data: [], next_cursor: null })));

  await renderApp(PAGE);

  expect(await screen.findByRole("heading", { name: "No alert rules yet" })).toBeInTheDocument();
});

it("shows the Alerts entry in the sidebar as current", async () => {
  await renderApp(PAGE);

  await screen.findByRole("table", { name: "Alert rules" });
  expect(screen.getByRole("link", { name: "Alerts" })).toHaveAttribute("aria-current", "page");
});

it("shows the forbidden state", async () => {
  server.use(http.get(RULES, () => problemResponse(403, "forbidden")));

  await renderApp(PAGE);

  expect(
    await screen.findByText("You don't have access to this project's alert rules."),
  ).toBeInTheDocument();
});

it("links New rule to the editor", async () => {
  await renderApp(PAGE);

  expect(await screen.findByRole("link", { name: "New rule" })).toHaveAttribute(
    "href",
    "/acme/support-bot/alerts/new",
  );
});

it("disables New rule for viewers", async () => {
  server.use(
    http.get("/api/v1/projects/:project", () =>
      HttpResponse.json({ ...projectFixture, role: "viewer" }),
    ),
  );

  await renderApp(PAGE);

  expect(await screen.findByRole("button", { name: "New rule" })).toBeDisabled();
  expect(screen.queryByRole("link", { name: "New rule" })).toBeNull();
});
