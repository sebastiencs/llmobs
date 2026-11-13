import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, it, vi } from "vitest";
import { ConfirmDialog } from "./confirm-dialog";

function Harness({
  pending = false,
  error,
  onConfirm,
}: {
  pending?: boolean;
  error?: string;
  onConfirm: () => void;
}) {
  const [open, setOpen] = useState(true);
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={setOpen}
      title="Revoke key?"
      description="CI stops working."
      confirmLabel="Revoke"
      destructive
      pending={pending}
      error={error}
      onConfirm={onConfirm}
    />
  );
}

it("confirms without closing", async () => {
  const user = userEvent.setup();
  const onConfirm = vi.fn();
  render(<Harness onConfirm={onConfirm} />);

  expect(screen.getByRole("button", { name: "Revoke" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Cancel" })).toBeInTheDocument();
  expect(screen.getByRole("alertdialog", { name: "Revoke key?" })).toHaveTextContent(
    "CI stops working.",
  );
  expect(screen.getByRole("button", { name: "Revoke" })).toHaveAttribute(
    "data-variant",
    "destructive",
  );

  await user.click(screen.getByRole("button", { name: "Revoke" }));
  expect(onConfirm).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("alertdialog", { name: "Revoke key?" })).toBeInTheDocument();
});

it("cancels", async () => {
  const user = userEvent.setup();
  const onConfirm = vi.fn();
  render(<Harness onConfirm={onConfirm} />);

  await user.click(screen.getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

it("shows the error and disables buttons while pending", async () => {
  const user = userEvent.setup();
  const onConfirm = vi.fn();
  render(<Harness pending error="Nope" onConfirm={onConfirm} />);

  expect(screen.getByRole("alert")).toHaveTextContent("Nope");
  expect(screen.getByRole("button", { name: "Revoke" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();

  await user.keyboard("{Escape}");
  expect(screen.getByRole("alertdialog", { name: "Revoke key?" })).toBeInTheDocument();
});
