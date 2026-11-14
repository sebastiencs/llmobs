import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import { DangerZone } from "./danger-zone";

function zone(overrides: Partial<Parameters<typeof DangerZone>[0]> = {}) {
  return (
    <TooltipProvider>
      <DangerZone
        title="Delete project"
        description="Erased within 24 hours."
        buttonLabel="Delete project…"
        onOpen={vi.fn()}
        {...overrides}
      />
    </TooltipProvider>
  );
}

it("disables the button without a reason while the role is loading", () => {
  render(zone({ disabled: true }));

  const button = screen.getByRole("button", { name: "Delete project…" });
  expect(button).toBeDisabled();
  expect(button).not.toHaveAccessibleDescription();
});

it("explains a role refusal next to the disabled button", () => {
  render(zone({ disabledReason: "Only owners can delete" }));

  const button = screen.getByRole("button", { name: "Delete project…" });
  expect(button).toBeDisabled();
  expect(button.parentElement).toHaveAccessibleDescription("Only owners can delete");
});

it("opens when enabled", async () => {
  const user = userEvent.setup();
  const onOpen = vi.fn();
  render(zone({ onOpen }));

  await user.click(screen.getByRole("button", { name: "Delete project…" }));
  expect(onOpen).toHaveBeenCalledTimes(1);
});
