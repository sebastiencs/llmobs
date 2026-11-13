import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { api } from "@/api/client";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import authConfig from "../../../fixtures/api/static/auth-config.json";
import meFixture from "../../../fixtures/api/static/me.json";

function signedOut() {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));
}

it("redirects to login with the page to come back to", async () => {
  signedOut();

  const { router } = await renderApp("/acme?x=1");

  expect(router.state.location.pathname).toBe("/login");
  expect(router.state.location.search).toEqual({ redirect: "/acme?x=1" });
});

it("redirects to setup when the server needs it", async () => {
  signedOut();
  server.use(
    http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, setup_required: true })),
  );

  const { router } = await renderApp("/acme");

  expect(router.state.location.pathname).toBe("/setup");
});

it("leaves public pages alone", async () => {
  signedOut();

  const { router } = await renderApp("/login");

  expect(router.state.location.pathname).toBe("/login");
  expect(screen.getByRole("heading", { name: "Sign in" })).toBeInTheDocument();
});

it("sends the user to login when the session ends during use", async () => {
  let ended = false;
  server.use(
    http.get("/api/auth/me", () =>
      ended ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
    ),
    http.get("/api/v1/orgs/:org/projects", () => {
      ended = true;
      return problemResponse(401, "unauthorized");
    }),
  );

  const { router, queryClient } = await renderApp("/acme");

  await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
  expect(await screen.findByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github?redirect=%2Facme",
  );
  await waitFor(() =>
    expect(
      queryClient.getQueryCache().findAll({ queryKey: ["get", "/api/v1/orgs/{org}/projects"] }),
    ).toHaveLength(0),
  );
  expect(queryClient.getQueryData(["get", "/api/auth/config"])).toBeDefined();
});

it("does not redirect when the login form gets a 401", async () => {
  signedOut();
  server.use(http.post("/api/auth/login", () => problemResponse(401, "unauthorized")));

  const { router, user } = await renderApp("/login?redirect=%2Facme");
  await user.type(screen.getByLabelText("Email"), "ada@example.com");
  await user.type(screen.getByLabelText("Password"), "wrong password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Incorrect email or password.");
  expect(router.state.location.pathname).toBe("/login");
  expect(screen.getByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github?redirect=%2Facme",
  );
});

it("ignores a session 401 on a public page", async () => {
  signedOut();
  server.use(http.get("/api/v1/orgs", () => problemResponse(401, "unauthorized")));

  const { router } = await renderApp("/login?redirect=%2Facme");
  await api.GET("/api/v1/orgs");

  expect(router.state.location.pathname).toBe("/login");
  expect(screen.getByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github?redirect=%2Facme",
  );
});

it("sends a session that ends on a new server to setup", async () => {
  let ended = false;
  server.use(
    http.get("/api/auth/me", () =>
      ended ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
    ),
    http.get("/api/v1/orgs/:org/projects", () => {
      ended = true;
      return problemResponse(401, "unauthorized");
    }),
    http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, setup_required: true })),
  );

  const { router } = await renderApp("/acme");

  await waitFor(() => expect(router.state.location.pathname).toBe("/setup"));
});

it("redirects when me answers 401 during use", async () => {
  let ended = false;
  server.use(
    http.get("/api/auth/me", () =>
      ended ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
    ),
  );

  const { router, queryClient } = await renderApp("/acme");
  await screen.findByRole("heading", { name: "Acme" });

  ended = true;
  await queryClient.refetchQueries({ queryKey: ["get", "/api/auth/me"] }).catch(() => undefined);

  await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
  expect(await screen.findByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github?redirect=%2Facme",
  );
});
