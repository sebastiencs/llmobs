import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { FIRING_RULE_ID, SLACK_CHANNEL_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import ruleFixture from "../../../../../../../fixtures/api/static/alert-rule.json";
import projectFixture from "../../../../../../../fixtures/api/static/project.json";

const PAGE = `/acme/support-bot/alerts/${FIRING_RULE_ID}/edit`;
const RULE = "/api/v1/projects/:project/alert-rules/:id";
const RULE_PAGE = `/acme/support-bot/alerts/${FIRING_RULE_ID}`;

it("edits the name only", async () => {
  let body: unknown;
  server.use(
    http.patch(RULE, async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ...ruleFixture, name: "Errors 2" });
    }),
  );

  const { router, user } = await renderApp(PAGE);
  const name = await screen.findByRole("textbox", { name: "Name" });
  expect(name).toHaveValue("High error rate");
  expect(screen.getByRole("textbox", { name: "Threshold" })).toHaveValue("5");
  expect(screen.getByRole("textbox", { name: "Filter" })).toHaveValue("environment = 'production'");
  expect(await screen.findByRole("checkbox", { name: "Team Slack (Slack)" })).toBeChecked();
  expect(screen.getByRole("checkbox", { name: "Pager webhook (Webhook)" })).toBeChecked();
  expect(screen.getByRole("textbox", { name: "Remind every (minutes)" })).toHaveValue("60");

  await user.clear(name);
  await user.type(name, "Errors 2");
  await user.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => expect(body).toEqual({ name: "Errors 2" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(RULE_PAGE));
});

it("sends the channels and reminder interval that changed", async () => {
  let body: unknown;
  server.use(
    http.patch(RULE, async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(ruleFixture);
    }),
  );

  const { user } = await renderApp(PAGE);
  await user.click(await screen.findByRole("checkbox", { name: "Pager webhook (Webhook)" }));
  const minutes = screen.getByRole("textbox", { name: "Remind every (minutes)" });
  await user.clear(minutes);
  await user.type(minutes, "90");
  await user.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => expect(body).toEqual({ channel_ids: [SLACK_CHANNEL_ID], cooldown_s: 5400 }));
});

it("shows the plan limit from a save", async () => {
  server.use(http.patch(RULE, () => problemResponse(403, "plan_limit", { detail: "x" })));

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), " 2");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("x")).toBeInTheDocument();
});

it("sends nothing when nothing changed", async () => {
  let patches = 0;
  server.use(
    http.patch(RULE, () => {
      patches += 1;
      return HttpResponse.json(ruleFixture);
    }),
  );

  const { router, user } = await renderApp(PAGE);
  await user.click(await screen.findByRole("button", { name: "Save" }));

  await waitFor(() => expect(router.state.location.pathname).toBe(RULE_PAGE));
  expect(patches).toBe(0);
});

it("warns that a new condition resets the state", async () => {
  let body: Record<string, unknown> | undefined;
  server.use(
    http.patch(RULE, async ({ request }) => {
      body = (await request.json()) as Record<string, unknown>;
      return HttpResponse.json(ruleFixture);
    }),
  );

  const { user } = await renderApp(PAGE);
  const name = await screen.findByRole("textbox", { name: "Name" });
  await user.type(name, " 2");
  expect(screen.queryByText("Saving a new condition resets the rule to OK.")).toBeNull();

  const threshold = screen.getByRole("textbox", { name: "Threshold" });
  await user.clear(threshold);
  await user.type(threshold, "10");
  expect(
    await screen.findByText("Saving a new condition resets the rule to OK."),
  ).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() =>
    expect(body).toEqual({
      name: "High error rate 2",
      condition: {
        metric: "error_rate",
        window: "5m",
        filter: "environment = 'production'",
        comparator: "gt",
        threshold: 0.1,
        min_count: 20,
      },
    }),
  );
});

it("does not let viewers edit", async () => {
  server.use(
    http.get("/api/v1/projects/:project", () =>
      HttpResponse.json({ ...projectFixture, role: "viewer" }),
    ),
  );

  await renderApp(PAGE);

  expect(
    await screen.findByText("Only project members and above can edit alert rules."),
  ).toBeInTheDocument();
  expect(screen.queryByRole("textbox", { name: "Name" })).toBeNull();
});

it("shows not found for an unknown rule", async () => {
  server.use(http.get(RULE, () => problemResponse(404, "not_found")));

  await renderApp(PAGE);

  expect(await screen.findByRole("heading", { name: "Page not found" })).toBeInTheDocument();
});
