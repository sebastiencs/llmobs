import { render, screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { LockedFeature } from "./locked-feature";

it("says what unlocks the feature", () => {
  render(<LockedFeature feature="project_roles" upgradeUrl={undefined} />);

  expect(screen.getByRole("note", { name: "Locked feature" })).toHaveTextContent(
    "Available with the Teams add-on or an enterprise licence.",
  );
});

it("links to the upgrade page in a new tab", () => {
  render(<LockedFeature feature="sso_enforcement" upgradeUrl="https://up.example/billing" />);

  const link = screen.getByRole("link", { name: "Upgrade" });
  expect(link).toHaveAttribute("href", "https://up.example/billing");
  expect(link).toHaveAttribute("target", "_blank");
});

it("has no upgrade link without a URL", () => {
  render(<LockedFeature feature="sso_enforcement" upgradeUrl={undefined} />);

  expect(screen.queryByRole("link", { name: "Upgrade" })).toBeNull();
});
