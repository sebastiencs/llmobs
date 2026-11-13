import { expect, it } from "vitest";
import { accessLabel, expiresAt, scopesFor } from "@/components/account/token-format";

it("labels the access level", () => {
  expect(accessLabel(["read"])).toBe("Read only");
  expect(accessLabel(["read", "write"])).toBe("Read and write");
  expect(accessLabel(["write", "read"])).toBe("Read and write");
});

it("maps the access level to scopes", () => {
  expect(scopesFor("read")).toEqual(["read"]);
  expect(scopesFor("read-write")).toEqual(["read", "write"]);
});

it("computes the expiry instant", () => {
  expect(expiresAt("never", 0)).toBeUndefined();
  expect(expiresAt("30", Date.UTC(2026, 0, 1))).toBe("2026-01-31T00:00:00.000Z");
  expect(expiresAt("365", Date.UTC(2026, 0, 1))).toBe("2027-01-01T00:00:00.000Z");
});
