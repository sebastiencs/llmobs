import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { WEBHOOK_CHANNEL_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import channelsFixture from "../../../../../../fixtures/api/static/channels.json";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }));

const PAGE = "/acme/support-bot/settings/channels";
const CHANNELS = "/api/v1/projects/:project/channels";

function projectAs(role: string) {
  return http.get("/api/v1/projects/:project", () =>
    HttpResponse.json({ ...projectFixture, role }),
  );
}

async function openAction(user: UserEvent, channel: string, item: string) {
  await user.click(screen.getByRole("button", { name: `Actions for ${channel}` }));
  await user.click(await screen.findByRole("menuitem", { name: item }));
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
}

async function openCreate(user: UserEvent) {
  await user.click(screen.getByRole("button", { name: "New channel" }));
  return await screen.findByRole("dialog", { name: "New channel" });
}

it("lists channels with their last delivery", async () => {
  await renderApp(PAGE);

  const table = await screen.findByRole("table", { name: "Notification channels" });
  const rows = within(table).getAllByRole("row").slice(1);
  expect(rows[0]).toHaveTextContent("On-call email");
  expect(rows[1]).toHaveTextContent("Pager webhook");
  expect(rows[2]).toHaveTextContent("Team Slack");
  expect(within(table).getByText("hooks.slack.com/…/abcd")).toBeInTheDocument();
  expect(within(table).getByText("Sent")).toBeInTheDocument();
  expect(within(table).getByText("Failed")).toBeInTheDocument();
  expect(within(table).getByText("the endpoint answered 500")).toBeInTheDocument();
  expect(rows[0]).toHaveTextContent("—");
});

it("shows the Channels entry in the settings nav", async () => {
  await renderApp(PAGE);

  await screen.findByRole("table", { name: "Notification channels" });
  expect(projectNav().getByRole("link", { name: "Channels" })).toHaveAttribute(
    "aria-current",
    "page",
  );
});

it("creates a webhook channel", async () => {
  let body: unknown;
  let lists = 0;
  const onStart = ({ request }: { request: Request }) => {
    if (request.method === "GET" && new URL(request.url).pathname.endsWith("/channels")) {
      lists += 1;
    }
  };
  server.events.on("request:start", onStart);
  server.use(
    http.post(CHANNELS, async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(channelsFixture.data[1], { status: 201 });
    }),
  );

  try {
    const { user } = await renderApp(PAGE);
    await screen.findByRole("table", { name: "Notification channels" });
    expect(lists).toBe(1);
    const dialog = await openCreate(user);
    await user.type(within(dialog).getByLabelText("Name"), "Pager");
    await user.click(within(dialog).getByRole("radio", { name: "Webhook" }));
    await user.type(within(dialog).getByLabelText("URL"), "https://hooks.example.com/x");
    await user.type(within(dialog).getByLabelText("Signing secret"), "0123456789abcdef");
    await user.click(within(dialog).getByRole("button", { name: "Add header" }));
    await user.type(within(dialog).getByLabelText("Header name 1"), "X-Team");
    await user.type(within(dialog).getByLabelText("Header value 1"), "core");
    await user.click(within(dialog).getByRole("button", { name: "Create channel" }));

    await waitFor(() =>
      expect(body).toEqual({
        name: "Pager",
        kind: "webhook",
        url: "https://hooks.example.com/x",
        secret: "0123456789abcdef",
        headers: { "X-Team": "core" },
      }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New channel" })).toBeNull());
    await waitFor(() => expect(lists).toBe(2));
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("creates an email channel", async () => {
  let body: unknown;
  server.use(
    http.post(CHANNELS, async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(channelsFixture.data[0], { status: 201 });
    }),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  const dialog = await openCreate(user);
  await user.type(within(dialog).getByLabelText("Name"), "Ops");
  await user.click(within(dialog).getByRole("radio", { name: "Email" }));
  await user.type(within(dialog).getByLabelText("Recipients"), "a@x.io\n\n b@x.io ");
  await user.click(within(dialog).getByRole("button", { name: "Create channel" }));

  await waitFor(() =>
    expect(body).toEqual({
      name: "Ops",
      kind: "email",
      recipients: ["a@x.io", "b@x.io"],
    }),
  );
});

it("shows field errors from the server", async () => {
  server.use(
    http.post(CHANNELS, () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "url", message: "must use https" }],
      }),
    ),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  const dialog = await openCreate(user);
  await user.type(within(dialog).getByLabelText("Name"), "Team");
  await user.type(within(dialog).getByLabelText("Webhook URL"), "https://x.test/hook");
  await user.click(within(dialog).getByRole("button", { name: "Create channel" }));

  expect(await within(dialog).findByText("must use https")).toBeInTheDocument();
  expect(within(dialog).getByLabelText("Webhook URL")).toHaveAttribute("aria-invalid", "true");
  expect(within(dialog).queryByRole("alert")).toBeNull();
});

it("edits without resending secrets", async () => {
  let body: unknown;
  server.use(
    http.patch("/api/v1/projects/:project/channels/:id", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(channelsFixture.data[1]);
    }),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  await openAction(user, "Pager webhook", "Edit");
  const dialog = await screen.findByRole("dialog", { name: "Edit channel" });
  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "Pager 2");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));

  await waitFor(() => expect(body).toEqual({ name: "Pager 2" }));
});

it("deletes a channel", async () => {
  const ids: string[] = [];
  server.use(
    http.delete("/api/v1/projects/:project/channels/:id", ({ params }) => {
      ids.push(String(params.id));
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  await openAction(user, "Pager webhook", "Delete");
  const dialog = await screen.findByRole("alertdialog", { name: "Delete channel?" });
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));

  await waitFor(() => expect(ids).toEqual([WEBHOOK_CHANNEL_ID]));
  await waitFor(() =>
    expect(screen.queryByRole("alertdialog", { name: "Delete channel?" })).toBeNull(),
  );
});

it("sends a test", async () => {
  server.use(
    http.post("/api/v1/projects/:project/channels/:id/test", () =>
      HttpResponse.json({ ok: false, error: "the endpoint answered 500" }),
    ),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  await openAction(user, "Pager webhook", "Send test");
  const dialog = await screen.findByRole("dialog", { name: "Test notification" });
  expect(
    await within(dialog).findByText("The channel did not accept the test message."),
  ).toBeInTheDocument();
  expect(within(dialog).getByText("the endpoint answered 500")).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Close" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog", { name: "Test notification" })).toBeNull(),
  );

  server.use(
    http.post("/api/v1/projects/:project/channels/:id/test", () => HttpResponse.json({ ok: true })),
  );
  await openAction(user, "Pager webhook", "Send test");
  const again = await screen.findByRole("dialog", { name: "Test notification" });
  expect(
    await within(again).findByText("The channel accepted the test message."),
  ).toBeInTheDocument();
});

it("viewers can read but not write", async () => {
  let writes = 0;
  const count = () => {
    writes += 1;
    return HttpResponse.json({}, { status: 500 });
  };
  server.use(
    projectAs("viewer"),
    http.post(CHANNELS, count),
    http.patch("/api/v1/projects/:project/channels/:id", count),
    http.delete("/api/v1/projects/:project/channels/:id", count),
  );

  const { user } = await renderApp(PAGE);
  const table = await screen.findByRole("table", { name: "Notification channels" });
  expect(within(table).getByText("Team Slack")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "New channel" })).toBeDisabled();
  const actions = screen.getByRole("button", { name: "Actions for Team Slack" });
  expect(actions).toBeDisabled();

  await user.click(screen.getByRole("button", { name: "New channel" }));
  await user.click(actions);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByRole("menu")).toBeNull();
  expect(writes).toBe(0);
});

it("email without SMTP is flagged", async () => {
  server.use(
    http.get(CHANNELS, () =>
      HttpResponse.json({
        ...channelsFixture,
        data: [{ ...channelsFixture.data[0], available: false }],
      }),
    ),
  );

  await renderApp(PAGE);

  expect(await screen.findByText("SMTP not configured")).toBeInTheDocument();
});

it("shows Forbidden for a 403 and the empty state", async () => {
  server.use(http.get(CHANNELS, () => problemResponse(403, "forbidden")));
  const first = await renderApp(PAGE);
  expect(
    await screen.findByText("You don't have access to this project's channels."),
  ).toBeInTheDocument();
  first.unmount();

  server.use(http.get(CHANNELS, () => HttpResponse.json({ data: [], next_cursor: null })));
  await renderApp(PAGE);
  expect(await screen.findByText("No channels yet")).toBeInTheDocument();
});

it("delete failure keeps the dialog open", async () => {
  server.use(
    http.delete("/api/v1/projects/:project/channels/:id", () => problemResponse(500, "internal")),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  await openAction(user, "Pager webhook", "Delete");
  const dialog = await screen.findByRole("alertdialog", { name: "Delete channel?" });
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));

  const open = await screen.findByRole("alertdialog", { name: "Delete channel?" });
  expect(await within(open).findByRole("alert")).toHaveTextContent("Internal Server Error");
});

it("a duplicate name is a conflict", async () => {
  server.use(
    http.post(CHANNELS, () =>
      problemResponse(409, "conflict", { detail: "a channel with this name already exists" }),
    ),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  const dialog = await openCreate(user);
  await user.type(within(dialog).getByLabelText("Name"), "Team Slack");
  await user.type(
    within(dialog).getByLabelText("Webhook URL"),
    "https://hooks.slack.com/services/T/B/abcd",
  );
  await user.click(within(dialog).getByRole("button", { name: "Create channel" }));

  expect(
    await within(dialog).findByText("a channel with this name already exists"),
  ).toBeInTheDocument();
});

it("edit shows header names, not values", async () => {
  const bodies: unknown[] = [];
  server.use(
    http.patch("/api/v1/projects/:project/channels/:id", async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(channelsFixture.data[1]);
    }),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  await openAction(user, "Pager webhook", "Edit");
  const dialog = await screen.findByRole("dialog", { name: "Edit channel" });
  expect(within(dialog).getByLabelText("Header name 1")).toHaveValue("X-Team");
  expect(within(dialog).getByLabelText("Header value 1")).toHaveValue("");

  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "Pager 2");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toEqual([{ name: "Pager 2" }]));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Edit channel" })).toBeNull());

  await openAction(user, "Pager webhook", "Edit");
  const again = await screen.findByRole("dialog", { name: "Edit channel" });
  await user.type(within(again).getByLabelText("Header value 1"), "abc");
  await user.click(within(again).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies[1]).toEqual({ headers: { "X-Team": "abc" } }));
});

it("checks the required fields before sending", async () => {
  let posts = 0;
  server.use(
    http.post(CHANNELS, () => {
      posts += 1;
      return HttpResponse.json(channelsFixture.data[1], { status: 201 });
    }),
  );

  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  const dialog = await openCreate(user);
  await user.click(within(dialog).getByRole("button", { name: "Create channel" }));

  expect(await within(dialog).findByText("Enter a name.")).toBeInTheDocument();
  expect(within(dialog).getByText("Enter a URL.")).toBeInTheDocument();
  expect(posts).toBe(0);
});

it("creates a Slack channel and forgets the URL", async () => {
  let body: unknown;
  server.use(
    http.post(CHANNELS, async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(channelsFixture.data[2], { status: 201 });
    }),
  );

  const { user, queryClient } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  const dialog = await openCreate(user);
  await user.type(within(dialog).getByLabelText("Name"), "Team");
  await user.type(
    within(dialog).getByLabelText("Webhook URL"),
    "https://hooks.slack.com/services/T/B/abcd",
  );
  await user.click(within(dialog).getByRole("button", { name: "Create channel" }));

  await waitFor(() =>
    expect(body).toEqual({
      name: "Team",
      kind: "slack",
      url: "https://hooks.slack.com/services/T/B/abcd",
    }),
  );
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "New channel" })).toBeNull());
  await waitFor(() =>
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((mutation) => mutation.state.variables ?? null),
      ),
    ).not.toContain("hooks.slack.com/services"),
  );
});

it("edit shows the target hint", async () => {
  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  await openAction(user, "Team Slack", "Edit");
  const dialog = await screen.findByRole("dialog", { name: "Edit channel" });

  expect(within(dialog).getByText(/hooks\.slack\.com\/…\/abcd/)).toBeInTheDocument();
});

it("caps the headers at ten", async () => {
  const { user } = await renderApp(PAGE);
  await screen.findByRole("table", { name: "Notification channels" });
  const dialog = await openCreate(user);
  await user.click(within(dialog).getByRole("radio", { name: "Webhook" }));
  for (let index = 0; index < 10; index += 1) {
    await user.click(within(dialog).getByRole("button", { name: "Add header" }));
  }

  expect(within(dialog).getByRole("button", { name: "Add header" })).toBeDisabled();
});
