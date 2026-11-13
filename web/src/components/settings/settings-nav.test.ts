import { expect, it } from "vitest";
import { isProjectSettingsPath } from "@/components/settings/settings-nav";

it("matches the settings area and every path below it", () => {
  expect(isProjectSettingsPath("/acme/bot/settings", "acme", "bot")).toBe(true);
  expect(isProjectSettingsPath("/acme/bot/settings/general", "acme", "bot")).toBe(true);
});

it("does not match other paths", () => {
  expect(isProjectSettingsPath("/acme/bot/settingsx", "acme", "bot")).toBe(false);
  expect(isProjectSettingsPath("/acme/bot/traces", "acme", "bot")).toBe(false);
  expect(isProjectSettingsPath("/acme/other/settings", "acme", "bot")).toBe(false);
});
