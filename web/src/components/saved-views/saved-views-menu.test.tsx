import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

const ERRORS = "01900000-0000-7000-8000-000000000210";

it("lists trace views and marks the shared default", async () => {
  const { user } = await renderApp("/acme/support-bot/traces?range=1h");
  await user.click(await screen.findByRole("button", { name: "Default view" }));
  expect(screen.getByRole("menuitem", { name: /Errors/ })).toBeInTheDocument();
  expect(screen.getByRole("img", { name: "Shared" })).toBeInTheDocument();
  expect(screen.getByRole("img", { name: "Default" })).toBeInTheDocument();
  expect(
    screen.queryByRole("menuitem", { name: /Cost per model per day/ }),
  ).not.toBeInTheDocument();
});

it("loads a view into the URL", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces?range=1h");
  await user.click(await screen.findByRole("button", { name: "Default view" }));
  await user.click(screen.getByRole("menuitem", { name: /Errors/ }));
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({
      filter: "status = 'error'",
      range: "24h",
      view: ERRORS,
    });
  });
});

it("marks a changed filter and saves it", async () => {
  const bodies: unknown[] = [];
  const onStart = ({ request }: { request: Request }) => {
    if (request.method === "PATCH" && request.url.includes("/saved-views/")) {
      void request
        .clone()
        .json()
        .then((body) => {
          bodies.push(body);
        });
    }
  };
  server.events.on("request:start", onStart);
  try {
    const { user } = await renderApp("/acme/support-bot/traces");
    await user.click(await screen.findByRole("button", { name: "Remove status = 'error'" }));
    const trigger = await screen.findByRole("button", { name: /Unsaved changes/ });
    expect(trigger).toHaveAccessibleName(/Errors/);
    await user.click(trigger);
    await user.click(screen.getByRole("menuitem", { name: "Save" }));
    await waitFor(() => {
      expect(bodies).toEqual([{ state: { range: "24h" } }]);
    });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("saves the current search as a new view and selects it", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces?range=1h");
  await user.click(await screen.findByRole("button", { name: "Default view" }));
  await user.click(screen.getByRole("menuitem", { name: "Save as…" }));
  await user.type(screen.getByRole("textbox", { name: "Name" }), "Nightly");
  await user.click(screen.getByRole("button", { name: "Save view" }));
  await waitFor(() => {
    expect(router.state.location.search.view).toEqual(expect.any(String));
  });
  const view = router.state.location.search.view;
  expect(view).not.toBe(ERRORS);
  expect(await screen.findByRole("button", { name: "Nightly" })).toBeInTheDocument();
});

it("deletes the loaded view", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces?range=1h");
  await user.click(await screen.findByRole("button", { name: "Default view" }));
  await user.click(screen.getByRole("menuitem", { name: /Errors/ }));
  await screen.findByRole("button", { name: "Errors" });
  await user.click(screen.getByRole("button", { name: "Errors" }));
  await user.click(screen.getByRole("menuitem", { name: "Delete" }));
  await user.click(screen.getByRole("button", { name: "Delete view" }));
  await waitFor(() => {
    expect(router.state.location.search.view).toBeUndefined();
  });
  expect(router.state.location.search.range).toBe("24h");
  expect(screen.getByRole("button", { name: "Default view" })).toBeInTheDocument();
});

it("applies the default view when the page opens with no search", async () => {
  const { router } = await renderApp("/acme/support-bot/traces");
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({
      filter: "status = 'error'",
      range: "24h",
      view: ERRORS,
    });
  });
  expect(router.history.length).toBe(1);
  expect(await screen.findByRole("button", { name: "Errors" })).toBeInTheDocument();
});

it("shows a failed save in the menu", async () => {
  server.use(
    http.patch("*/api/v1/projects/:project/saved-views/:id", () =>
      HttpResponse.json(
        { type: "about:blank", title: "Not Found", status: 404, code: "not_found" },
        { status: 404, headers: { "Content-Type": "application/problem+json" } },
      ),
    ),
  );
  const { user } = await renderApp("/acme/support-bot/traces");
  await user.click(await screen.findByRole("button", { name: "Errors" }));
  await user.click(screen.getByRole("menuitem", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Not Found");
});
