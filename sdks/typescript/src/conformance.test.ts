/**
 * The cross-language conformance scenarios (README § Scenarios): each case
 * `fixtures/otlp/llmobs/sdk-<case>/input.json` is the canonical OTLP this SDK
 * must emit for the TypeScript call of the scenario row, run through the real
 * pipeline with the OTLP exporter replaced by an in-memory one.
 */

import { readdirSync, readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { canonical, SCENARIOS } from "./conformance.test-helpers.js";
import { resetForTests } from "./runtime.js";
import { finished, startMemory } from "./testing.test-helpers.js";

const FIXTURES = new URL("../../../fixtures/otlp/llmobs/", import.meta.url);

describe("conformance", () => {
  beforeEach(async () => {
    await resetForTests();
  });

  afterEach(async () => {
    await resetForTests();
  });

  for (const scenario of SCENARIOS) {
    it(scenario.name, async () => {
      const { exporter } = startMemory(scenario.options);
      await scenario.run();
      const spans = await finished(exporter);
      expect(canonical(spans)).toEqual(readFixture(scenario.name));
    });
  }

  it("lists every sdk-* fixture directory", () => {
    const directories = readdirSync(FIXTURES)
      .filter((name) => name.startsWith("sdk-"))
      .sort();
    expect(SCENARIOS.map((scenario) => scenario.name).sort()).toEqual(directories);
  });
});

function readFixture(name: string): unknown {
  return JSON.parse(readFileSync(new URL(`${name}/input.json`, FIXTURES), "utf8") as string);
}
