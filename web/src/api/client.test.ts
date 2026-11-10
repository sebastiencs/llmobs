import { describe, expect, it } from "vitest";
import { createAppQueryClient, retryQuery } from "./client";

describe("retryQuery", () => {
  it("retries twice, and never retries a 4xx problem", () => {
    const network = new Error("offline");
    expect(retryQuery(0, network)).toBe(true);
    expect(retryQuery(1, network)).toBe(true);
    expect(retryQuery(2, network)).toBe(false);
    expect(
      retryQuery(0, {
        type: "about:blank",
        title: "Unauthorized",
        status: 401,
        code: "unauthorized",
      }),
    ).toBe(false);
    expect(
      retryQuery(0, { type: "about:blank", title: "Server error", status: 500, code: "internal" }),
    ).toBe(true);

    const retry = createAppQueryClient().getDefaultOptions().queries?.retry;
    expect(retry).toBe(retryQuery);
  });
});
