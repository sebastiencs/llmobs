import { http } from "msw";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/api/client";
import { isSessionApiPath, setUnauthorizedHandler } from "@/api/session";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";

afterEach(() => {
  setUnauthorizedHandler(undefined);
});

it("treats /api/v1/ and /api/auth/me as session paths", () => {
  expect(isSessionApiPath("/api/v1/orgs")).toBe(true);
  expect(isSessionApiPath("/api/v1/projects/acme%2Fsupport-bot/traces")).toBe(true);
  expect(isSessionApiPath("/api/auth/me")).toBe(true);
  expect(isSessionApiPath("/api/auth/login")).toBe(false);
  expect(isSessionApiPath("/api/auth/invites/abc/accept")).toBe(false);
});

it("calls the handler for a 401 from /api/v1/", async () => {
  const spy = vi.fn();
  setUnauthorizedHandler(spy);
  server.use(http.get("/api/v1/orgs", () => problemResponse(401, "unauthorized")));

  const { error } = await api.GET("/api/v1/orgs");

  expect(spy).toHaveBeenCalledTimes(1);
  expect(spy).toHaveBeenCalledWith("/api/v1/orgs");
  expect(error?.status).toBe(401);
});

it("passes the me path to the handler", async () => {
  const spy = vi.fn();
  setUnauthorizedHandler(spy);
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));

  await api.GET("/api/auth/me");

  expect(spy).toHaveBeenCalledTimes(1);
  expect(spy).toHaveBeenCalledWith("/api/auth/me");
});

it("ignores a 401 from the login form", async () => {
  const spy = vi.fn();
  setUnauthorizedHandler(spy);
  server.use(http.post("/api/auth/login", () => problemResponse(401, "unauthorized")));

  await api.POST("/api/auth/login", { body: { email: "a@example.com", password: "x" } });

  expect(spy).not.toHaveBeenCalled();
});

it("ignores other statuses", async () => {
  const spy = vi.fn();
  setUnauthorizedHandler(spy);
  server.use(http.get("/api/v1/orgs", () => problemResponse(403, "forbidden")));

  await api.GET("/api/v1/orgs");

  expect(spy).not.toHaveBeenCalled();
});
