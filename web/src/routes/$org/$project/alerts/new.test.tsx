import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { FIRING_RULE_ID, SLACK_CHANNEL_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import ruleFixture from "../../../../../../fixtures/api/static/alert-rule.json";
import channelsFixture from "../../../../../../fixtures/api/static/channels.json";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

const PAGE = "/acme/support-bot/alerts/new";
const RULES = "/api/v1/projects/:project/alert-rules";
const METRICS = "/api/v1/projects/:project/metrics";
const RULE_PAGE = `/acme/support-bot/alerts/${FIRING_RULE_ID}`;

/** Chooses an option of a select by its label, through the trigger and the listbox. */
async function choose(user: UserEvent, label: string, option: string) {
  await user.click(screen.getByRole("combobox", { name: label }));
  await user.click(await screen.findByRole("option", { name: option }));
}

function metricsResponse(points: { time: string; value?: number }[]) {
  return {
    bucket_ns: 300_000_000_000,
    from: "2026-01-01T00:00:00.000Z",
    to: "2026-01-02T00:00:00.000Z",
    kpis: [],
    series: [{ metric: "error_rate", points }],
    top_models: [],
  };
}

it("creates an error rate rule", async () => {
  let body: unknown;
  server.use(
    http.post(RULES, async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(ruleFixture, { status: 201 });
    }),
  );

  const { router, user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.click(await screen.findByRole("checkbox", { name: "Team Slack (Slack)" }));
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  await waitFor(() =>
    expect(body).toEqual({
      name: "Errors",
      condition: {
        metric: "error_rate",
        window: "5m",
        comparator: "gt",
        threshold: 0.05,
        min_count: 20,
      },
      channel_ids: [SLACK_CHANNEL_ID],
      enabled: true,
      cooldown_s: 3600,
      notify_while_firing: false,
    }),
  );
  await waitFor(() => expect(router.state.location.pathname).toBe(RULE_PAGE));
});

it("sends the score fields", async () => {
  let body: Record<string, unknown> | undefined;
  server.use(
    http.post(RULES, async ({ request }) => {
      body = (await request.json()) as Record<string, unknown>;
      return HttpResponse.json(ruleFixture, { status: 201 });
    }),
  );

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Pass");
  await choose(user, "Metric", "Score");
  await user.type(screen.getByRole("textbox", { name: "Score name" }), "ok");
  await choose(user, "Aggregation", "Pass rate");
  await choose(user, "Comparator", "Below");
  const threshold = screen.getByRole("textbox", { name: "Threshold" });
  await user.clear(threshold);
  await user.type(threshold, "90");
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  await waitFor(() =>
    expect(body?.condition).toEqual({
      metric: "score",
      window: "5m",
      comparator: "lt",
      threshold: 0.9,
      min_count: 10,
      score_name: "ok",
      aggregation: "pass_rate",
    }),
  );
});

it("shows client errors and sends nothing", async () => {
  let posts = 0;
  server.use(
    http.post(RULES, () => {
      posts += 1;
      return HttpResponse.json(ruleFixture, { status: 201 });
    }),
  );

  const { user } = await renderApp(PAGE);
  await user.click(await screen.findByRole("button", { name: "Create rule" }));

  expect(await screen.findByText("Enter a name.")).toBeInTheDocument();
  const threshold = screen.getByRole("textbox", { name: "Threshold" });
  await user.clear(threshold);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  expect(await screen.findByText("Enter a number.")).toBeInTheDocument();
  expect(posts).toBe(0);
});

it("shows the plan limit with the upgrade line", async () => {
  server.use(
    http.post(RULES, () =>
      problemResponse(403, "plan_limit", { detail: "Your plan allows 1 enabled alert rule." }),
    ),
  );

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  expect(await screen.findByText("Your plan allows 1 enabled alert rule.")).toBeInTheDocument();
  expect(
    screen.getByText("Upgrade your plan, disable another rule, or save this one disabled."),
  ).toBeInTheDocument();
});

it("clears the plan limit hint once a later submit fails client validation", async () => {
  server.use(
    http.post(RULES, () =>
      problemResponse(403, "plan_limit", { detail: "Your plan allows 1 enabled alert rule." }),
    ),
  );

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.click(screen.getByRole("button", { name: "Create rule" }));
  expect(
    await screen.findByText("Upgrade your plan, disable another rule, or save this one disabled."),
  ).toBeInTheDocument();

  const threshold = screen.getByRole("textbox", { name: "Threshold" });
  await user.clear(threshold);
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  expect(await screen.findByText("Enter a number.")).toBeInTheDocument();
  expect(
    screen.queryByText("Upgrade your plan, disable another rule, or save this one disabled."),
  ).toBeNull();
});

it("creates a rule disabled when Enabled is switched off", async () => {
  let body: Record<string, unknown> | undefined;
  server.use(
    http.post(RULES, async ({ request }) => {
      body = (await request.json()) as Record<string, unknown>;
      return HttpResponse.json(ruleFixture, { status: 201 });
    }),
  );

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.click(screen.getByRole("switch", { name: "Enabled" }));
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  await waitFor(() => expect(body).toMatchObject({ enabled: false }));
});

it("shows the preview error when the metrics request fails", async () => {
  server.use(http.get(METRICS, () => problemResponse(500, "internal")));

  await renderApp(PAGE);

  expect(await screen.findByText("The preview could not be loaded.")).toBeInTheDocument();
});

it("sends the reminder interval and checks its range", async () => {
  let body: Record<string, unknown> | undefined;
  let posts = 0;
  server.use(
    http.post(RULES, async ({ request }) => {
      posts += 1;
      body = (await request.json()) as Record<string, unknown>;
      return HttpResponse.json(ruleFixture, { status: 201 });
    }),
  );

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.click(screen.getByRole("switch", { name: "Remind while firing" }));
  const minutes = await screen.findByRole("textbox", { name: "Remind every (minutes)" });
  await user.clear(minutes);
  await user.type(minutes, "3");
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  expect(await screen.findByText("Enter 5 to 10,080 minutes.")).toBeInTheDocument();
  expect(posts).toBe(0);

  await user.clear(minutes);
  await user.type(minutes, "120");
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  await waitFor(() => expect(body).toMatchObject({ notify_while_firing: true, cooldown_s: 7200 }));
});

it("shows the other server errors", async () => {
  server.use(
    http.post(RULES, () =>
      problemResponse(409, "conflict", { detail: "an alert rule with this name already exists" }),
    ),
  );

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.click(screen.getByRole("button", { name: "Create rule" }));
  expect(
    await screen.findByText("an alert rule with this name already exists"),
  ).toBeInTheDocument();
});

it("shows the busy server message", async () => {
  server.use(http.post(RULES, () => problemResponse(503, "busy")));

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.click(screen.getByRole("button", { name: "Create rule" }));
  expect(await screen.findByText("The server is busy. Try again.")).toBeInTheDocument();
});

it("caps the channels at ten", async () => {
  const channels = Array.from({ length: 11 }, (_, index) => ({
    ...channelsFixture.data[0],
    kind: "slack",
    id: `01900000-0000-7000-8000-0000000006${10 + index}`,
    name: `Channel ${index}`,
  }));
  server.use(
    http.get("/api/v1/projects/:project/channels", () =>
      HttpResponse.json({ data: channels, next_cursor: null }),
    ),
  );

  const { user } = await renderApp(PAGE);
  for (let index = 0; index < 10; index += 1) {
    await user.click(await screen.findByRole("checkbox", { name: `Channel ${index} (Slack)` }));
  }
  expect(await screen.findByRole("checkbox", { name: "Channel 10 (Slack)" })).toBeDisabled();
});

it("says so when there are no channels", async () => {
  server.use(
    http.get("/api/v1/projects/:project/channels", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );

  await renderApp(PAGE);

  expect(await screen.findByText("No channels yet.")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Add a channel" })).toHaveAttribute(
    "href",
    "/acme/support-bot/settings/channels",
  );
});

it("shows a server filter error under the filter", async () => {
  server.use(
    http.post(RULES, () =>
      problemResponse(400, "invalid_filter", {
        errors: [{ field: "filter", message: "unexpected end of input" }],
      }),
    ),
  );

  const { user } = await renderApp(PAGE);
  await user.type(await screen.findByRole("textbox", { name: "Name" }), "Errors");
  await user.type(screen.getByRole("textbox", { name: "Filter" }), "environment =");
  await user.click(screen.getByRole("button", { name: "Create rule" }));

  expect(await screen.findByText("unexpected end of input")).toBeInTheDocument();
});

it("previews over buckets as wide as the window", async () => {
  const requests: string[] = [];
  server.use(
    http.get(METRICS, ({ request }) => {
      requests.push(request.url);
      return HttpResponse.json(metricsResponse([{ time: "2026-01-01T00:00:00Z", value: 0.1 }]));
    }),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("figure", { name: "Preview of the metric over the last 24 hours" });
  expect(requests.at(-1)).toContain("bucket=5m");

  await choose(user, "Window", "24 hours");
  await waitFor(() => expect(requests.at(-1)).toContain("bucket=1d"));
  const url = new URL(requests.at(-1) ?? "");
  const span =
    Date.parse(url.searchParams.get("to") ?? "") - Date.parse(url.searchParams.get("from") ?? "");
  expect(span).toBe(14 * 24 * 60 * 60 * 1000);
});

it("does not preview LLM span latency", async () => {
  let requests = 0;
  server.use(
    http.get(METRICS, () => {
      requests += 1;
      return HttpResponse.json(metricsResponse([{ time: "2026-01-01T00:00:00Z", value: 1 }]));
    }),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("figure");
  const before = requests;
  await choose(user, "Metric", "p95 latency");
  await choose(user, "Latency of", "LLM spans");

  expect(await screen.findByText(/No preview for LLM span latency/)).toBeInTheDocument();
  expect(requests).toBe(before);
});

it("debounces the filter in the preview", async () => {
  const filters: (string | null)[] = [];
  server.use(
    http.get(METRICS, ({ request }) => {
      filters.push(new URL(request.url).searchParams.get("filter"));
      return HttpResponse.json(metricsResponse([{ time: "2026-01-01T00:00:00Z", value: 0.1 }]));
    }),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("figure");
  await user.type(screen.getByRole("textbox", { name: "Filter" }), "error_count > 0");

  await waitFor(() => expect(filters).toContain("error_count > 0"));
  expect(filters.filter((filter) => filter !== null)).toEqual(["error_count > 0"]);
});

it("shows the preview states", async () => {
  server.use(
    http.get(METRICS, () =>
      problemResponse(400, "invalid_filter", { errors: [{ field: "filter", message: "bad" }] }),
    ),
  );

  await renderApp(PAGE);
  expect(await screen.findByText("The filter is invalid: bad")).toBeInTheDocument();
});

it("shows no data when the preview has no points", async () => {
  server.use(http.get(METRICS, () => HttpResponse.json(metricsResponse([]))));

  await renderApp(PAGE);

  expect(await screen.findByText("No data in this range.")).toBeInTheDocument();
});

it("does not let viewers create rules", async () => {
  server.use(
    http.get("/api/v1/projects/:project", () =>
      HttpResponse.json({ ...projectFixture, role: "viewer" }),
    ),
  );

  await renderApp(PAGE);

  expect(
    await screen.findByText("Only project members and above can create alert rules."),
  ).toBeInTheDocument();
  expect(screen.queryByRole("textbox", { name: "Name" })).toBeNull();
});
