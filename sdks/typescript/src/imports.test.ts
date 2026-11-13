/**
 * Guard for the runtime set (D77): the shipped source must stay loadable on
 * Node, Bun, Deno and edge runtimes, so it imports nothing that only Node
 * provides — no `node:` specifiers, no bare `async_hooks`/`events`/`fs`/`http`
 * imports, no `require(`. Test files are exempt (they may read fixtures).
 */

import { readdirSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const SRC = new URL("./", import.meta.url);
const EXEMPT = /\.test(?:-helpers)?\.ts$/;
const NODE_PREFIX = /node:/;
const BARE_NODE_IMPORT =
  /(?:^import\s*["']|from\s*["']|import\s*\(\s*["'])(?:async_hooks|events|fs|http)["']/m;
const REQUIRE_CALL = /require\(/;

describe("imports", () => {
  it("ships no Node-only import", () => {
    const files = readdirSync(SRC).filter(
      (name) => name.endsWith(".ts") && !EXEMPT.test(name) && !name.startsWith("."),
    );
    expect(files.length).toBeGreaterThan(0);
    const offenders: string[] = [];
    for (const name of files) {
      const source = readFileSync(new URL(name, SRC), "utf8");
      if (NODE_PREFIX.test(source)) {
        offenders.push(`${name}: node: specifier`);
      }
      if (BARE_NODE_IMPORT.test(source)) {
        offenders.push(`${name}: bare async_hooks/events/fs/http import`);
      }
      if (REQUIRE_CALL.test(source)) {
        offenders.push(`${name}: require() call`);
      }
    }
    expect(offenders).toEqual([]);
  });
});
