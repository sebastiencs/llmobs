import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { expect, it } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import { DisabledReason } from "./disabled-reason";

function WithTooltips({ children }: { children: ReactNode }) {
  return <TooltipProvider>{children}</TooltipProvider>;
}

it("passes the child through without a reason", () => {
  const { container } = render(
    <WithTooltips>
      <DisabledReason reason={undefined}>
        <button type="button">Save</button>
      </DisabledReason>
    </WithTooltips>,
  );
  const button = screen.getByRole("button", { name: "Save" });
  expect(button.parentElement).toBe(container);
});

it("describes the disabled control", () => {
  render(
    <WithTooltips>
      <DisabledReason reason="You need the admin role">
        <button type="button" disabled>
          Save
        </button>
      </DisabledReason>
    </WithTooltips>,
  );
  const wrapper = screen.getByRole("button", { name: "Save" }).parentElement;
  expect(wrapper).toHaveAccessibleDescription("You need the admin role");
  expect(wrapper).toHaveAttribute("tabindex", "0");
});

it("shows the reason on focus", async () => {
  const user = userEvent.setup();
  render(
    <WithTooltips>
      <DisabledReason reason="You need the admin role">
        <button type="button" disabled>
          Save
        </button>
      </DisabledReason>
    </WithTooltips>,
  );
  await user.tab();
  expect(await screen.findByRole("tooltip")).toHaveTextContent("You need the admin role");
});
