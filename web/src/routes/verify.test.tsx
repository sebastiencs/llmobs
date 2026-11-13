import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

it("verifies the email from the link", async () => {
  let calls = 0;
  let body: unknown;
  server.use(
    http.post("/api/auth/email/verify", async ({ request }) => {
      calls += 1;
      body = await request.json();
      return new HttpResponse(null, { status: 204 });
    }),
  );
  await renderApp("/verify?token=tok123");
  expect(await screen.findByRole("heading", { name: "Email verified" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", "/login");
  expect(body).toEqual({ token: "tok123" });
  expect(calls).toBe(1);
});

it("offers a new link when the token is invalid", async () => {
  server.use(
    http.post("/api/auth/email/verify", () =>
      problemResponse(400, "bad_request", { detail: "invalid or expired link" }),
    ),
  );
  let body: unknown;
  server.use(
    http.post("/api/auth/email/verify/resend", async ({ request }) => {
      body = await request.json();
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { user } = await renderApp("/verify?token=tok123");
  expect(
    await screen.findByRole("heading", { name: "This link is invalid or has expired" }),
  ).toBeInTheDocument();
  await user.type(screen.getByLabelText("Email"), "ada@example.com");
  await user.click(screen.getByRole("button", { name: "Send a new link" }));
  expect(body).toEqual({ email: "ada@example.com" });
  expect(await screen.findByRole("status")).toHaveTextContent(
    "If the address still needs verification, we sent a new link.",
  );
});

it("retries after a server error", async () => {
  let calls = 0;
  server.use(
    http.post("/api/auth/email/verify", () => {
      calls += 1;
      return problemResponse(500, "internal", { detail: "boom" });
    }),
  );
  const { user } = await renderApp("/verify?token=tok123");
  expect(await screen.findByRole("heading", { name: "Verification failed" })).toBeInTheDocument();
  expect(await screen.findByRole("alert")).toHaveTextContent("boom");
  await user.click(screen.getByRole("button", { name: "Try again" }));
  await waitFor(() => expect(calls).toBe(2));
});

it("explains a link without a token", async () => {
  await renderApp("/verify");
  expect(await screen.findByText(/^This link is incomplete\./)).toBeInTheDocument();
  expect(screen.getByLabelText("Email")).toBeInTheDocument();
});

it("says when email is not configured", async () => {
  server.use(
    http.post("/api/auth/email/verify/resend", () =>
      problemResponse(400, "bad_request", { detail: "email is not configured" }),
    ),
  );
  const { user } = await renderApp("/verify");
  await user.type(await screen.findByLabelText("Email"), "ada@example.com");
  await user.click(screen.getByRole("button", { name: "Send a new link" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Email is not configured on this server. Ask an instance admin to verify your account.",
  );
});

it("explains rate limiting on resend", async () => {
  server.use(
    http.post("/api/auth/email/verify/resend", () => problemResponse(429, "rate_limited")),
  );
  const { user } = await renderApp("/verify");
  await user.type(await screen.findByLabelText("Email"), "ada@example.com");
  await user.click(screen.getByRole("button", { name: "Send a new link" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Too many attempts. Wait a minute and try again.",
  );
});

it("clears the success message on a new send", async () => {
  let calls = 0;
  server.use(
    http.post("/api/auth/email/verify/resend", () => {
      calls += 1;
      return calls === 1
        ? new HttpResponse(null, { status: 204 })
        : problemResponse(429, "rate_limited");
    }),
  );
  const { user } = await renderApp("/verify");
  await user.type(await screen.findByLabelText("Email"), "ada@example.com");
  await user.click(screen.getByRole("button", { name: "Send a new link" }));
  expect(await screen.findByRole("status")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Send a new link" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Too many attempts. Wait a minute and try again.",
  );
  expect(screen.queryByRole("status")).toBeNull();
});

it("asks for an email before sending", async () => {
  const { user } = await renderApp("/verify");
  await user.click(await screen.findByRole("button", { name: "Send a new link" }));
  expect(await screen.findByText("Enter your email address.")).toBeInTheDocument();
});
