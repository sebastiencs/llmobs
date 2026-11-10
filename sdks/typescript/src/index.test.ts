import { readFileSync } from "node:fs";
import { expect, it } from "vitest";

import { VERSION } from "./index.js";

it("matches package.json", () => {
  const file = new URL("../package.json", import.meta.url);
  const pkg = JSON.parse(readFileSync(file, "utf8")) as { version: string };
  expect(VERSION).toBe(pkg.version);
});
