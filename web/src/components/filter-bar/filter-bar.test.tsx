import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

const FILTER = "status = 'error' AND model LIKE 'gpt%'";

function chip(label: string) {
  return (name: string) => name === label;
}

it("renders filter chips and removes one", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    const { router, user } = await renderApp(
      `/acme/support-bot/traces?filter=${encodeURIComponent(FILTER)}`,
    );
    expect(
      await screen.findByRole("button", { name: chip("status = 'error'") }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: chip("model LIKE 'gpt%'") })).toBeInTheDocument();
    expect(urls.some((url) => url.searchParams.get("filter") === FILTER)).toBe(true);
    await user.click(screen.getByRole("button", { name: "Remove model LIKE 'gpt%'" }));
    await waitFor(() => {
      expect(router.state.location.search.filter).toBe("status = 'error'");
    });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("edits a compare chip from the popover", async () => {
  const { router, user } = await renderApp(
    `/acme/support-bot/traces?filter=${encodeURIComponent(FILTER)}`,
  );
  await user.click(await screen.findByRole("button", { name: chip("status = 'error'") }));
  const value = await screen.findByRole("textbox", { name: "Value" });
  await user.clear(value);
  await user.type(value, "ok");
  await user.click(screen.getByRole("button", { name: "Apply" }));
  await waitFor(() => {
    expect(router.state.location.search.filter).toBe("status = 'ok' AND model LIKE 'gpt%'");
  });
});

it("applies the text editor with Enter", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces?range=15m");
  await user.click(await screen.findByRole("button", { name: "Edit as text" }));
  const content = document.querySelector(".cm-content");
  if (!(content instanceof HTMLElement)) {
    throw new Error("filter editor did not mount");
  }
  content.focus();
  await user.type(content, "status = 'ok'{Enter}");
  await waitFor(() => {
    expect(router.state.location.search.filter).toBe("status = 'ok'");
  });
});

it("shows an invalid filter on the bar and no rows", async () => {
  server.use(
    http.get("*/api/v1/projects/:project/traces", () =>
      HttpResponse.json(
        {
          type: "about:blank",
          title: "Bad Request",
          status: 400,
          code: "invalid_filter",
          errors: [
            {
              field: "filter",
              message: "unknown column nme",
              position: { line: 1, column: 1 },
            },
          ],
        },
        { status: 400, headers: { "Content-Type": "application/problem+json" } },
      ),
    ),
  );
  await renderApp(`/acme/support-bot/traces?filter=${encodeURIComponent("nme = 1")}`);
  expect(await screen.findAllByText("unknown column nme")).not.toHaveLength(0);
  await waitFor(() => {
    expect(document.querySelector(".cm-filter-error")?.textContent).toBe("nme");
  });
  expect(screen.queryByRole("grid", { name: "Traces" })).not.toBeInTheDocument();
});

it("appends a status comparison from the row", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces?range=15m");
  await screen.findByRole("grid", { name: "Traces" });
  await user.click(screen.getByRole("button", { name: "Filter by status error" }));
  await waitFor(() => {
    expect(router.state.location.search.filter).toBe("status = 'error'");
  });
  expect(router.state.location.search.trace).toBeUndefined();
});

it("shows a filter with a top-level OR as one chip and removes it whole", async () => {
  const filter = "a = 1 OR b = 2 AND c = 3";
  const { router, user } = await renderApp(
    `/acme/support-bot/traces?range=1h&filter=${encodeURIComponent(filter)}`,
  );
  const remove = await screen.findByRole("button", { name: `Remove ${filter}` });
  expect(screen.queryByRole("button", { name: "Remove c = 3" })).not.toBeInTheDocument();
  await user.click(remove);
  await waitFor(() => {
    expect(router.state.location.search.filter).toBeUndefined();
  });
  expect(router.state.location.search.range).toBe("1h");
});

it("parenthesizes an OR filter before appending from a row", async () => {
  const { router, user } = await renderApp(
    `/acme/support-bot/traces?range=1h&filter=${encodeURIComponent("a = 1 OR b = 2")}`,
  );
  await screen.findByRole("grid", { name: "Traces" });
  await user.click(screen.getByRole("button", { name: "Filter by status error" }));
  await waitFor(() => {
    expect(router.state.location.search.filter).toBe("(a = 1 OR b = 2) AND status = 'error'");
  });
});
