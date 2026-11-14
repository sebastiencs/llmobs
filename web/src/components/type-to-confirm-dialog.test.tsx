import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { TypeToConfirmDialog } from "./type-to-confirm-dialog";

function dialog(overrides: Partial<Parameters<typeof TypeToConfirmDialog>[0]> = {}) {
  return (
    <TypeToConfirmDialog
      open
      onOpenChange={vi.fn()}
      title="Delete the project?"
      description="This cannot be undone."
      expected="support-bot"
      inputLabel='Type the project slug "support-bot" to confirm'
      confirmLabel="Delete project"
      pending={false}
      onConfirm={vi.fn()}
      {...overrides}
    />
  );
}

function confirmButton() {
  return screen.getByRole("button", { name: "Delete project" });
}

it("enables the confirm button only for the exact text", async () => {
  const user = userEvent.setup();
  render(dialog());

  expect(confirmButton()).toBeDisabled();
  await user.type(screen.getByRole("textbox"), "Support-Bot");
  expect(confirmButton()).toBeDisabled();

  await user.clear(screen.getByRole("textbox"));
  await user.type(screen.getByRole("textbox"), "support-bot");
  expect(confirmButton()).toBeEnabled();
});

it("confirms on click and on Enter in the input", async () => {
  const user = userEvent.setup();
  const onConfirm = vi.fn();
  render(dialog({ onConfirm }));

  await user.type(screen.getByRole("textbox"), "support-bot");
  await user.click(confirmButton());
  expect(onConfirm).toHaveBeenCalledTimes(1);
});

it("confirms when Enter is pressed in the input", async () => {
  const user = userEvent.setup();
  const onConfirm = vi.fn();
  render(dialog({ onConfirm }));

  await user.type(screen.getByRole("textbox"), "support-bot{Enter}");
  expect(onConfirm).toHaveBeenCalledTimes(1);
});

it("clears the typed text when reopened", async () => {
  const user = userEvent.setup();
  const { rerender } = render(dialog());

  await user.type(screen.getByRole("textbox"), "support-bot");
  expect(confirmButton()).toBeEnabled();

  rerender(dialog({ open: false }));
  rerender(dialog({ open: true }));
  expect(screen.getByRole("textbox")).toHaveValue("");
  expect(confirmButton()).toBeDisabled();
});

it("keeps Cancel and Esc inert while pending", async () => {
  const user = userEvent.setup();
  const onOpenChange = vi.fn();
  render(dialog({ pending: true, onOpenChange }));

  expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
  await user.keyboard("{Escape}");
  expect(onOpenChange).not.toHaveBeenCalled();
});
