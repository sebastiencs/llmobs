import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { DEFAULT_QUERY } from "@/lib/sql/draft";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";
import resultDocument from "../../../../../../fixtures/api/static/sql-result.json";

const PROBLEM_JSON = { "Content-Type": "application/problem+json" };

function single<T>(items: readonly T[]): T {
  expect(items).toHaveLength(1);
  const [item] = items;
  if (item === undefined) {
    throw new Error("expected one item");
  }
  return item;
}

function editorContent(): HTMLElement {
  const content = document.querySelector(".cm-content");
  if (!(content instanceof HTMLElement)) {
    throw new Error("SQL editor did not mount");
  }
  return content;
}

function storeDraft(query: string, ref = "acme/support-bot") {
  localStorage.setItem(`llmobs.sql.draft.${ref}`, JSON.stringify({ query }));
}

function captureSql(
  answer: () => Response | Promise<Response> = () => HttpResponse.json(resultDocument),
) {
  const bodies: Record<string, unknown>[] = [];
  const accepts: (string | null)[] = [];
  server.use(
    http.post("*/api/v1/projects/:project/sql", async ({ request }) => {
      accepts.push(request.headers.get("Accept"));
      bodies.push((await request.json()) as Record<string, unknown>);
      return answer();
    }),
  );
  return { bodies, accepts };
}

function problemResponse(problem: Record<string, unknown>, status: number) {
  return HttpResponse.json(
    { type: "about:blank", title: "Error", status, ...problem },
    { status, headers: PROBLEM_JSON },
  );
}

it("lists SQL in the sidebar and opens the console", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");
  await user.click(screen.getByRole("link", { name: "SQL" }));
  await waitFor(() => {
    expect(router.state.location.pathname).toBe("/acme/support-bot/sql");
  });
  expect(await screen.findByRole("heading", { name: "SQL" })).toBeInTheDocument();
  expect(screen.getByText("Run a query to see results.")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "SQL" })).toHaveAttribute("aria-current", "page");
  expect(screen.getByRole("button", { name: "Run" })).toHaveAttribute("title", "Run (Ctrl↵)");
});

it("opens the console from the palette", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");
  await screen.findByRole("grid", { name: "Traces" });
  await user.keyboard("{Meta>}k{/Meta}");
  const dialog = await screen.findByRole("dialog", { name: /Command palette/ });
  await user.click(within(dialog).getByRole("option", { name: "SQL" }));
  await waitFor(() => {
    expect(router.state.location.pathname).toBe("/acme/support-bot/sql");
  });
});

it("runs the draft over the picked range", async () => {
  const sql = captureSql();
  const { user } = await renderApp("/acme/support-bot/sql?range=1h");
  await user.click(screen.getByRole("button", { name: "Run" }));
  await waitFor(() => {
    const body = single(sql.bodies);
    expect(body.query).toBe(DEFAULT_QUERY);
    expect(Date.parse(String(body.to)) - Date.parse(String(body.from))).toBe(3_600_000);
    expect(Object.keys(body).sort()).toEqual(["from", "query", "to"]);
    expect(single(sql.accepts)).toBe("application/json");
  });
  const grid = await screen.findByRole("grid", { name: "Query results" });
  expect(
    within(grid)
      .getAllByRole("columnheader")
      .map((header) => header.textContent?.trim()),
  ).toEqual(resultDocument.columns.map((column) => column.name));
  await screen.findByText(new RegExp(`^${resultDocument.rows.length} rows? · \\d+ ms$`));
});

it("runs the stored draft with Ctrl+Enter", async () => {
  storeDraft("SELECT 1");
  const sql = captureSql();
  const { user } = await renderApp("/acme/support-bot/sql");
  const content = await waitFor(editorContent);
  content.focus();
  await user.keyboard("{Control>}{Enter}{/Control}");
  await waitFor(() => {
    expect(single(sql.bodies).query).toBe("SELECT 1");
  });
});

it("saves typing to the draft", async () => {
  storeDraft("");
  const { user } = await renderApp("/acme/support-bot/sql");
  const content = await waitFor(editorContent);
  await user.type(content, "SELECT 7");
  await waitFor(() => {
    expect(JSON.parse(localStorage.getItem("llmobs.sql.draft.acme/support-bot") ?? "null")).toEqual(
      { query: "SELECT 7" },
    );
  });
});

it("restores the draft from localStorage", async () => {
  storeDraft("SELECT 42");
  await renderApp("/acme/support-bot/sql");
  await waitFor(() => {
    expect(editorContent().textContent).toBe("SELECT 42");
  });
});

it("keeps one draft per project", async () => {
  storeDraft("SELECT 'bot'");
  storeDraft("SELECT 'play'", "acme/playground");
  const { router } = await renderApp("/acme/support-bot/sql");
  await waitFor(() => {
    expect(editorContent().textContent).toBe("SELECT 'bot'");
  });
  await router.navigate({
    to: "/$org/$project/sql",
    params: { org: "acme", project: "playground" },
  });
  await waitFor(() => {
    expect(editorContent().textContent).toBe("SELECT 'play'");
  });
});

it("does nothing for an empty query", async () => {
  storeDraft("   ");
  const sql = captureSql();
  const { user } = await renderApp("/acme/support-bot/sql");
  const run = screen.getByRole("button", { name: "Run" });
  expect(run).toBeDisabled();
  const content = await waitFor(editorContent);
  content.focus();
  await user.keyboard("{Control>}{Enter}{/Control}");
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(sql.bodies).toHaveLength(0);
  expect(run).toBeDisabled();
  expect(screen.queryByText("Running…")).toBeNull();
});

it("shows Running… and blocks a second run", async () => {
  let release = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  captureSql(async () => {
    await gate;
    return HttpResponse.json(resultDocument);
  });
  const { user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Run" }));
  const status = await screen.findByRole("status");
  expect(status).toHaveTextContent("Running…");
  expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
  release();
  await screen.findByRole("grid", { name: "Query results" });
  expect(screen.queryByText("Running…")).toBeNull();
});

it("underlines the reported position", async () => {
  storeDraft("SELECT 1\nFROM nme");
  captureSql(() =>
    problemResponse(
      {
        code: "invalid_filter",
        detail: "invalid query",
        status: 400,
        title: "Bad Request",
        errors: [
          {
            field: "query",
            message: "table 'nme' not found",
            position: { line: 2, column: 6 },
          },
        ],
      },
      400,
    ),
  );
  const { user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Run" }));
  const alert = await screen.findByRole("alert");
  expect(alert).toHaveTextContent("table 'nme' not found");
  expect(alert).toHaveAttribute("id", "sql-error");
  await waitFor(() => {
    expect(document.querySelector(".cm-sql-error")?.textContent).toBe("nme");
  });
  expect(editorContent()).toHaveAttribute("aria-invalid", "true");
  expect(editorContent()).toHaveAttribute("aria-describedby", "sql-error");
  expect(screen.queryByRole("grid", { name: "Query results" })).toBeNull();
  editorContent().focus();
  await user.keyboard("{Control>}{End}{/Control}x");
  await waitFor(() => {
    expect(document.querySelector(".cm-sql-error")).toBeNull();
  });
  expect(editorContent()).toHaveAttribute("aria-invalid", "false");
});

it("shows an error without a position", async () => {
  storeDraft("SELECT nope()");
  captureSql(() =>
    problemResponse(
      {
        code: "invalid_filter",
        detail: "invalid query",
        errors: [{ field: "query", message: "unknown function nope" }],
      },
      400,
    ),
  );
  const { user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Run" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("unknown function nope");
  expect(document.querySelector(".cm-sql-error")).toBeNull();
});

it("shows a timeout message", async () => {
  captureSql(() =>
    problemResponse(
      {
        code: "query_timeout",
        detail: "the query exceeded its time limit",
        title: "Unprocessable Entity",
      },
      422,
    ),
  );
  const { user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Run" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("the query exceeded its time limit");
});

it("shows a generic message for a non-problem failure", async () => {
  captureSql(
    () => new HttpResponse("oops", { status: 500, headers: { "Content-Type": "text/plain" } }),
  );
  const { user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Run" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("The query could not be run");
});

it("shows a generic message for an empty error body", async () => {
  captureSql(() => new HttpResponse(null, { status: 503 }));
  const { user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Run" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("The query could not be run");
  expect(screen.queryByRole("grid", { name: "Query results" })).toBeNull();
});

it("replaces the result and clears the error on the next run", async () => {
  let release = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  let calls = 0;
  captureSql(async () => {
    calls += 1;
    if (calls === 1) {
      return problemResponse(
        {
          code: "query_timeout",
          detail: "the query exceeded its time limit",
          title: "Unprocessable Entity",
        },
        422,
      );
    }
    await gate;
    return HttpResponse.json(resultDocument);
  });
  const { user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Run" }));
  await screen.findByRole("alert");
  await user.click(screen.getByRole("button", { name: "Run" }));
  expect(await screen.findByText("Running…")).toBeInTheDocument();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByRole("grid", { name: "Query results" })).toBeNull();
  release();
  await screen.findByRole("grid", { name: "Query results" });
  expect(screen.queryByRole("alert")).toBeNull();
});

it("renders NULL and JSON cells", async () => {
  captureSql(() =>
    HttpResponse.json({
      columns: [
        { name: "a", type: "Utf8" },
        { name: "n", type: "Int64" },
        { name: "m", type: "Map(Utf8, Utf8)" },
      ],
      rows: [{ a: null, n: 3, m: { k: "v" } }],
      truncated: true,
    }),
  );
  const { user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Run" }));
  const grid = await screen.findByRole("grid", { name: "Query results" });
  expect(
    within(grid)
      .getAllByRole("gridcell")
      .map((cell) => cell.textContent),
  ).toEqual(["NULL", "3", '{"k":"v"}']);
  expect(await screen.findByText(/^1 row \(truncated\) · \d+ ms$/)).toBeInTheDocument();
});

it("changes the range in the URL", async () => {
  const { router, user } = await renderApp("/acme/support-bot/sql");
  await user.click(screen.getByRole("button", { name: "Last 24 hours" }));
  await user.click(await screen.findByRole("button", { name: "Last 7 days" }));
  await waitFor(() => {
    expect(router.state.location.search).toEqual({ range: "7d" });
  });
});
