import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { FIRING_RULE_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import eventsFixture from "../../../../../../../fixtures/api/static/alert-events.json";
import ruleFixture from "../../../../../../../fixtures/api/static/alert-rule.json";
import projectFixture from "../../../../../../../fixtures/api/static/project.json";

const PAGE = `/acme/support-bot/alerts/${FIRING_RULE_ID}`;
const RULE = "/api/v1/projects/:project/alert-rules/:id";
const EVENTS = "/api/v1/projects/:project/alert-rules/:id/events";
const CANNOT_MANAGE = "Only project members and above can manage alerts";

/** Row `index` of a table; index 0 is the header. */
function rowAt(table: HTMLElement, index: number): HTMLElement {
  const row = within(table).getAllByRole("row")[index];
  if (!row) {
    throw new Error(`no row ${index}`);
  }
  return row;
}

it("shows the rule and its history", async () => {
  await renderApp(PAGE);

  expect(await screen.findByText("Team Slack, Pager webhook")).toBeInTheDocument();
  expect(screen.getByText("environment = 'production'")).toBeInTheDocument();
  expect(screen.getByText("Every 60 minutes while firing")).toBeInTheDocument();

  const table = await screen.findByRole("table", { name: "Alert events" });
  expect(within(table).getAllByRole("row").slice(1)).toHaveLength(3);

  const fired = rowAt(table, 1);
  expect(within(fired).getByText("Fired")).toBeInTheDocument();
  expect(fired).toHaveTextContent("12.5% · 48 samples");
  expect(within(fired).getByText("Pager webhook")).toBeInTheDocument();
  expect(within(fired).getByText("Failed")).toBeInTheDocument();
  expect(within(fired).getByText("the endpoint answered 500")).toBeInTheDocument();

  expect(rowAt(table, 3)).toHaveTextContent("2 attempts");
});

it("shows the last error", async () => {
  server.use(
    http.get(RULE, () => HttpResponse.json({ ...ruleFixture, last_error: "query timed out" })),
  );

  await renderApp(PAGE);

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The last evaluation failed: query timed out",
  );
});

it("pages the history", async () => {
  server.use(
    http.get(EVENTS, ({ request }) => {
      if (new URL(request.url).searchParams.get("cursor") === "c1") {
        return HttpResponse.json({
          data: [
            {
              ...eventsFixture.data[0],
              id: "01900000-0000-7000-8000-000000000529",
              kind: "reminder",
            },
          ],
          next_cursor: null,
        });
      }
      return HttpResponse.json({ ...eventsFixture, next_cursor: "c1" });
    }),
  );

  const { user } = await renderApp(PAGE);
  const table = await screen.findByRole("table", { name: "Alert events" });
  await user.click(screen.getByRole("button", { name: "Load more" }));

  expect(await within(table).findByText("Reminder")).toBeInTheDocument();
});

it("disables the rule", async () => {
  let body: unknown;
  server.use(
    http.patch(RULE, async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ...ruleFixture, enabled: false, state: "ok" });
    }),
  );

  const { user } = await renderApp(PAGE);
  await user.click(await screen.findByRole("switch", { name: "Enabled" }));

  await waitFor(() => expect(body).toEqual({ enabled: false }));
  expect(await screen.findByText("Disabled")).toBeInTheDocument();
});

it("shows the plan limit when enabling", async () => {
  server.use(
    http.get(RULE, () => HttpResponse.json({ ...ruleFixture, enabled: false })),
    http.patch(RULE, () =>
      problemResponse(403, "plan_limit", {
        detail: "Your plan allows 1 enabled alert rule.",
      }),
    ),
  );

  const { user } = await renderApp(PAGE);
  await user.click(await screen.findByRole("switch", { name: "Enabled" }));

  expect(await screen.findByText("Your plan allows 1 enabled alert rule.")).toBeInTheDocument();
  expect(screen.getByText("Upgrade your plan or disable another rule.")).toBeInTheDocument();
});

it("shows other toggle errors next to the switch", async () => {
  server.use(http.patch(RULE, () => problemResponse(500, "internal")));

  const { user } = await renderApp(PAGE);
  await user.click(await screen.findByRole("switch", { name: "Enabled" }));

  expect(await screen.findByText("Internal Server Error")).toBeInTheDocument();
});

it("deletes the rule", async () => {
  server.use(http.delete(RULE, () => new HttpResponse(null, { status: 204 })));

  const { router, user } = await renderApp(PAGE);
  await user.click(await screen.findByRole("button", { name: "Delete rule" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete alert rule?" });
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));

  await waitFor(() => expect(router.state.location.pathname).toBe("/acme/support-bot/alerts"));
});

it("keeps the delete dialog open when the delete fails", async () => {
  server.use(http.delete(RULE, () => problemResponse(500, "internal")));

  const { user } = await renderApp(PAGE);
  await user.click(await screen.findByRole("button", { name: "Delete rule" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete alert rule?" });
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));

  expect(await within(dialog).findByText("Internal Server Error")).toBeInTheDocument();
  expect(screen.getByRole("alertdialog", { name: "Delete alert rule?" })).toBeInTheDocument();
});

it("shows not found for an unknown rule", async () => {
  server.use(http.get(RULE, () => problemResponse(404, "not_found")));

  await renderApp(PAGE);

  expect(await screen.findByRole("heading", { name: "Page not found" })).toBeInTheDocument();
});

it("does not let viewers change the rule", async () => {
  server.use(
    http.get("/api/v1/projects/:project", () =>
      HttpResponse.json({ ...projectFixture, role: "viewer" }),
    ),
  );

  await renderApp(PAGE);
  const toggle = await screen.findByRole("switch", { name: "Enabled" });
  const del = screen.getByRole("button", { name: "Delete rule" });

  expect(toggle).toBeDisabled();
  expect(del).toBeDisabled();
  expect(toggle.parentElement).toHaveAccessibleDescription(CANNOT_MANAGE);
  expect(del.parentElement).toHaveAccessibleDescription(CANNOT_MANAGE);
});

it("shows the edge values of events and channels", async () => {
  server.use(
    http.get(EVENTS, () =>
      HttpResponse.json({
        next_cursor: null,
        data: [{ ...eventsFixture.data[0], metric: "cost", value: 1.23456, deliveries: [] }],
      }),
    ),
    http.get(RULE, () =>
      HttpResponse.json({ ...ruleFixture, channel_ids: [...ruleFixture.channel_ids, "unknown"] }),
    ),
  );

  await renderApp(PAGE);
  const table = await screen.findByRole("table", { name: "Alert events" });

  expect(rowAt(table, 1)).toHaveTextContent("1.235");
  expect(within(rowAt(table, 1)).getByText("No channels")).toBeInTheDocument();
  expect(await screen.findByText("Team Slack, Pager webhook, Deleted channel")).toBeInTheDocument();
});

it("shows the empty history", async () => {
  server.use(http.get(EVENTS, () => HttpResponse.json({ data: [], next_cursor: null })));

  await renderApp(PAGE);

  expect(
    await screen.findByText("No events yet. Events appear when the rule fires or resolves."),
  ).toBeInTheDocument();
});
