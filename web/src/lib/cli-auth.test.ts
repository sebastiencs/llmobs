import { expect, it } from "vitest";
import { cliAuthSearchFrom, cliCallbackUrl, cliTokenName, parseCliRequest } from "@/lib/cli-auth";

it("reads the raw query string", () => {
  expect(cliAuthSearchFrom("?port=54321&state=s_12345678901234567890&host=laptop&other=x")).toEqual(
    {
      port: "54321",
      state: "s_12345678901234567890",
      host: "laptop",
    },
  );
  expect(cliAuthSearchFrom("")).toEqual({});
});

it("accepts valid requests", () => {
  expect(parseCliRequest({ port: "1024", state: "a" })).toEqual({ port: 1024, state: "a" });
  expect(parseCliRequest({ port: "65535", state: "A_b-9", host: "my-host.local" })).toEqual({
    port: 65535,
    state: "A_b-9",
    host: "my-host.local",
  });
});

it("refuses an invalid port", () => {
  for (const port of ["1023", "65536", "80", "12a45", "01024", undefined]) {
    expect(parseCliRequest({ port, state: "s1" })).toBeUndefined();
  }
});

it("refuses an invalid state", () => {
  for (const state of [undefined, "", "has space", "a".repeat(257), "12345678901234567890", "7"]) {
    expect(parseCliRequest({ port: "54321", state })).toBeUndefined();
  }
});

it("refuses an invalid host", () => {
  for (const host of ["bad/host", "", "a".repeat(65)]) {
    expect(parseCliRequest({ port: "54321", state: "s1", host })).toBeUndefined();
  }
});

it("names the token after the host", () => {
  expect(cliTokenName({ port: 5000, state: "s" })).toBe("CLI");
  expect(cliTokenName({ port: 5000, state: "s", host: "laptop" })).toBe("CLI on laptop");
});

it("builds the callback with the token", () => {
  expect(cliCallbackUrl({ port: 54321, state: "xyz" }, { token: "pat-llmobs-A+B" })).toBe(
    "http://127.0.0.1:54321/callback?token=pat-llmobs-A%2BB&state=xyz",
  );
});

it("builds the callback with the error", () => {
  expect(cliCallbackUrl({ port: 54321, state: "xyz" }, { error: "access_denied" })).toBe(
    "http://127.0.0.1:54321/callback?error=access_denied&state=xyz",
  );
});
