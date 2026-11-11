import { screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { TIME_ZONE_STORAGE_KEY } from "@/lib/settings";
import { renderApp } from "@/test/render";

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
