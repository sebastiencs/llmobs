import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { afterEach, beforeEach, expect, it } from "vitest";
import { setExternalNavigator } from "@/lib/external-navigation";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

let urls: string[] = [];

beforeEach(() => {
  urls = [];
  setExternalNavigator((url) => {
    urls.push(url);
  });
});

afterEach(() => {
  setExternalNavigator(undefined);
});

/** A `201 CreatedPersonalToken`; every test that triggers the POST answers this shape. */
function tokenCreated(name: string): Response {
  return HttpResponse.json(
    {
      token: {
        id: "01900000-0000-7000-8000-000000000203",
        name,
        prefix: "pat-llmobs-CLITOK",
        scopes: ["read", "write"],
        created_at: "2026-01-05T00:00:00.000000000Z",
      },
      secret: "pat-llmobs-CLITOKEN0CLITOKEN0CLITOKEN0CLIT",
    },
    { status: 201 },
  );
}

it("asks the signed-in user to authorize", async () => {
  await renderApp("/cli-auth?port=54321&state=s1&host=laptop");

  expect(
    await screen.findByRole("heading", { name: "Authorize the llmobs CLI", level: 1 }),
  ).toBeInTheDocument();
  expect(screen.getByText("ada@example.com")).toBeInTheDocument();
  expect(screen.getByText("CLI on laptop")).toBeInTheDocument();
});

it("creates a token and returns it to the CLI", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/me/tokens", async ({ request }) => {
      body = await request.json();
      return tokenCreated("CLI on laptop");
    }),
  );

  const { user, queryClient } = await renderApp("/cli-auth?port=54321&state=s1&host=laptop");
  await user.click(await screen.findByRole("button", { name: "Authorize" }));

  await waitFor(() => expect(body).toEqual({ name: "CLI on laptop", scopes: ["read", "write"] }));
  await waitFor(() =>
    expect(urls).toEqual([
      "http://127.0.0.1:54321/callback?token=pat-llmobs-CLITOKEN0CLITOKEN0CLITOKEN0CLIT&state=s1",
    ]),
  );
  expect(
    await screen.findByRole("heading", { name: "You're signed in", level: 1 }),
  ).toBeInTheDocument();
  expect(screen.queryByText(/pat-llmobs-CLITOKEN/)).toBeNull();
  await waitFor(() =>
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((mutation) => mutation.state.data ?? null),
      ),
    ).not.toContain("pat-llmobs-CLITOKEN0"),
  );
});

it("names the token CLI without a host", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/me/tokens", async ({ request }) => {
      body = await request.json();
      return tokenCreated("CLI");
    }),
  );

  const { user } = await renderApp("/cli-auth?port=54321&state=s1");
  expect(await screen.findByText("this computer")).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Authorize" }));
  await waitFor(() => expect(body).toMatchObject({ name: "CLI" }));
});

it("cancels without creating a token", async () => {
  const calls: unknown[] = [];
  server.use(
    http.post("/api/v1/me/tokens", async ({ request }) => {
      calls.push(await request.json());
      return tokenCreated("CLI");
    }),
  );

  const { user } = await renderApp("/cli-auth?port=54321&state=s1");
  await user.click(await screen.findByRole("button", { name: "Cancel" }));

  expect(urls).toEqual(["http://127.0.0.1:54321/callback?error=access_denied&state=s1"]);
  expect(calls).toEqual([]);
  expect(
    await screen.findByRole("heading", { name: "Sign-in cancelled", level: 1 }),
  ).toBeInTheDocument();
});

it("refuses an invalid link", async () => {
  const calls: number[] = [];
  server.use(
    http.post("/api/v1/me/tokens", () => {
      calls.push(1);
      return new HttpResponse(null, { status: 500 });
    }),
  );

  for (const url of [
    "/cli-auth?port=80&state=s1",
    "/cli-auth?port=54321",
    "/cli-auth?port=01024&state=s1",
    "/cli-auth?port=54321&state=s1&host=bad%2Fhost",
  ]) {
    const view = await renderApp(url);
    expect(
      await screen.findByRole("heading", { name: "This sign-in link is invalid", level: 1 }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Authorize" })).toBeNull();
    expect(urls).toEqual([]);
    expect(calls).toEqual([]);
    view.unmount();
  }
});

it("accepts the edge ports", async () => {
  const first = await renderApp("/cli-auth?port=1024&state=s1");
  expect(
    await screen.findByRole("heading", { name: "Authorize the llmobs CLI", level: 1 }),
  ).toBeInTheDocument();
  first.unmount();

  await renderApp("/cli-auth?port=65535&state=s1");
  expect(
    await screen.findByRole("heading", { name: "Authorize the llmobs CLI", level: 1 }),
  ).toBeInTheDocument();
});

it("refuses an all-digit state", async () => {
  const calls: number[] = [];
  server.use(
    http.post("/api/v1/me/tokens", () => {
      calls.push(1);
      return new HttpResponse(null, { status: 500 });
    }),
  );

  await renderApp("/cli-auth?port=54321&state=12345678901234567890");

  expect(
    await screen.findByRole("heading", { name: "This sign-in link is invalid", level: 1 }),
  ).toBeInTheDocument();
  expect(calls).toEqual([]);
  expect(urls).toEqual([]);
});

it("returns a long state with digits byte for byte", async () => {
  server.use(http.post("/api/v1/me/tokens", () => tokenCreated("CLI")));

  const { user } = await renderApp("/cli-auth?port=54321&state=s_12345678901234567890");
  await user.click(await screen.findByRole("button", { name: "Authorize" }));

  await waitFor(() =>
    expect(urls).toEqual([
      "http://127.0.0.1:54321/callback?token=pat-llmobs-CLITOKEN0CLITOKEN0CLITOKEN0CLIT&state=s_12345678901234567890",
    ]),
  );
});

it("shows a failure and stays on the page", async () => {
  server.use(
    http.post("/api/v1/me/tokens", () =>
      problemResponse(429, "rate_limited", { detail: "slow down" }),
    ),
  );

  const { user } = await renderApp("/cli-auth?port=54321&state=s1");
  await user.click(await screen.findByRole("button", { name: "Authorize" }));

  const alert = await screen.findByRole("alert");
  expect(alert).toHaveTextContent("slow down");
  expect(urls).toEqual([]);
  expect(screen.getByRole("button", { name: "Authorize" })).toBeEnabled();
});

it("sends a signed-out user through login and keeps the link", async () => {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));

  const { router } = await renderApp("/cli-auth?port=54321&state=s1");

  await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
  expect(router.state.location.search).toMatchObject({
    redirect: "/cli-auth?port=54321&state=s1",
  });
});
