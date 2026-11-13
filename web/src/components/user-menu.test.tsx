import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { TIME_ZONE_STORAGE_KEY } from "@/lib/settings";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import meFixture from "../../../fixtures/api/static/me.json";

it("toggles showing times in UTC", async () => {
  const { user } = await renderApp("/");

  await user.click(screen.getByRole("button", { name: "Ada Example" }));
  const item = screen.getByRole("menuitemcheckbox", { name: "Show times in UTC" });
  expect(item).toHaveAttribute("aria-checked", "false");

  await user.click(item);
  expect(localStorage.getItem(TIME_ZONE_STORAGE_KEY)).toBe("utc");

  await user.click(screen.getByRole("button", { name: "Ada Example" }));
  expect(screen.getByRole("menuitemcheckbox", { name: "Show times in UTC" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
});

it("signs out and opens the login page", async () => {
  let signedOut = false;
  server.use(
    http.post("/api/auth/logout", () => {
      signedOut = true;
      return new HttpResponse(null, { status: 204 });
    }),
    http.get("/api/auth/me", () =>
      signedOut ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
    ),
  );

  const { router, user } = await renderApp("/acme");
  await user.click(screen.getByRole("button", { name: "Ada Example" }));
  await user.click(screen.getByRole("menuitem", { name: "Sign out" }));

  await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
  expect(signedOut).toBe(true);
  expect(await screen.findByRole("heading", { name: "Sign in" })).toBeInTheDocument();
});

it("leaves even when signing out fails", async () => {
  let signedOut = false;
  server.use(
    http.post("/api/auth/logout", () => {
      signedOut = true;
      return problemResponse(500, "internal");
    }),
    http.get("/api/auth/me", () =>
      signedOut ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
    ),
  );

  const { router, user } = await renderApp("/acme");
  await user.click(screen.getByRole("button", { name: "Ada Example" }));
  await user.click(screen.getByRole("menuitem", { name: "Sign out" }));

  await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
});

it("opens the account page from the user menu", async () => {
  const { user, router } = await renderApp("/acme");
  await user.click(screen.getByRole("button", { name: "Ada Example" }));
  await user.click(screen.getByRole("menuitem", { name: "Account" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/account"));
  expect(await screen.findByRole("heading", { name: "Account", level: 1 })).toBeInTheDocument();
});
