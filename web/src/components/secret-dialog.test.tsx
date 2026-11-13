import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, it, vi } from "vitest";
import { SecretDialog } from "@/components/secret-dialog";

const PAT_SECRET = "pat-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0";
const SK_SECRET = "sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0";

// The dialog stays mounted; only its `open` prop toggles, so a reopen shows the same
// component instance and the tests really exercise `finish()` resetting the state.
function Harness({
  secret,
  showSnippets,
  description,
  onDone,
}: {
  secret: string;
  showSnippets?: boolean;
  description?: string;
  onDone: () => void;
}) {
  const [open, setOpen] = useState(true);
  return (
    <>
      <SecretDialog
        open={open}
        title="Token created"
        secret={secret}
        showSnippets={showSnippets}
        description={description}
        onDone={() => {
          onDone();
          setOpen(false);
        }}
      />
      <button
        type="button"
        onClick={() => {
          setOpen(true);
        }}
      >
        Reopen
      </button>
    </>
  );
}

// userEvent.setup() installs its own clipboard stub, so it runs before the override.
function setup() {
  const user = userEvent.setup();
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  return { user, writeText };
}

it("shows the secret once with a copy button", () => {
  setup();
  render(<Harness secret={PAT_SECRET} onDone={vi.fn()} />);
  expect(screen.getByRole("dialog", { name: "Token created" })).toBeInTheDocument();
  expect(screen.getByLabelText("Secret")).toHaveValue(PAT_SECRET);
  expect(screen.getByText("Copy it now. You won't see it again.")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Copy secret" })).toBeInTheDocument();
  expect(screen.queryByRole("tab")).toBeNull();
});

it("shows snippets for a secret key", () => {
  setup();
  render(<Harness secret={SK_SECRET} showSnippets onDone={vi.fn()} />);
  expect(screen.getByRole("tab", { name: "OTel env vars" })).toBeInTheDocument();
  expect(screen.getByRole("dialog")).toHaveTextContent(`Authorization=Bearer ${SK_SECRET}`);
});

it("shows no snippets unless asked for a secret key", () => {
  setup();
  const key = render(<Harness secret={SK_SECRET} onDone={vi.fn()} />);
  expect(screen.queryByRole("tab")).toBeNull();
  key.unmount();
  render(<Harness secret={PAT_SECRET} showSnippets onDone={vi.fn()} />);
  expect(screen.queryByRole("tab")).toBeNull();
});

it("closes with Done", async () => {
  const { user } = setup();
  const onDone = vi.fn();
  render(<Harness secret={PAT_SECRET} onDone={onDone} />);
  await user.click(screen.getByRole("button", { name: "Done" }));
  expect(onDone).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("asks before Escape closes an uncopied secret", async () => {
  const { user } = setup();
  const onDone = vi.fn();
  render(<Harness secret={PAT_SECRET} onDone={onDone} />);
  await user.keyboard("{Escape}");
  expect(screen.getByRole("dialog")).toBeInTheDocument();
  expect(screen.getByRole("alert")).toHaveTextContent(
    "You haven't copied the secret. Close without copying?",
  );
  expect(onDone).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Go back" }));
  expect(screen.queryByRole("alert")).toBeNull();
  await user.keyboard("{Escape}");
  await user.click(screen.getByRole("button", { name: "Close anyway" }));
  expect(onDone).toHaveBeenCalledTimes(1);
});

it("closes on Escape after a copy", async () => {
  const { user } = setup();
  const onDone = vi.fn();
  render(<Harness secret={PAT_SECRET} onDone={onDone} />);
  await user.click(screen.getByRole("button", { name: "Copy secret" }));
  await act(async () => {
    await Promise.resolve();
  });
  await user.keyboard("{Escape}");
  expect(onDone).toHaveBeenCalledTimes(1);
});

it("asks before an outside click closes an uncopied secret", async () => {
  const { user } = setup();
  const onDone = vi.fn();
  render(<Harness secret={PAT_SECRET} onDone={onDone} />);
  // Radix registers its outside-pointer listener in a setTimeout(0).
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  fireEvent.pointerDown(document.body);
  fireEvent.click(document.body);
  expect(screen.getByRole("alert")).toBeInTheDocument();
  expect(onDone).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Copy secret" }));
  await act(async () => {
    await Promise.resolve();
  });
  fireEvent.pointerDown(document.body);
  fireEvent.click(document.body);
  expect(onDone).toHaveBeenCalledTimes(1);
});

it("puts the description before the warning line", () => {
  setup();
  render(<Harness secret={PAT_SECRET} description="Shown once." onDone={vi.fn()} />);
  expect(screen.getByRole("dialog")).toHaveTextContent(
    "Shown once. Copy it now. You won't see it again.",
  );
});

it("starts clean when reopened", async () => {
  const { user } = setup();
  const onDone = vi.fn();
  render(<Harness secret={PAT_SECRET} onDone={onDone} />);
  await user.keyboard("{Escape}");
  await user.click(screen.getByRole("button", { name: "Close anyway" }));
  expect(onDone).toHaveBeenCalledTimes(1);
  await user.click(screen.getByRole("button", { name: "Reopen" }));
  expect(screen.getByRole("dialog")).toBeInTheDocument();
  expect(screen.queryByRole("alert")).toBeNull();
});

it("resets the warning when the secret changes", async () => {
  const { user } = setup();
  const onDone = vi.fn();
  const view = render(<Harness secret={PAT_SECRET} onDone={onDone} />);
  await user.keyboard("{Escape}");
  expect(screen.getByRole("alert")).toBeInTheDocument();
  view.rerender(
    <Harness secret="pat-llmobs-OTHER0OTHER0OTHER0OTHER0OTHER0OTHER" onDone={onDone} />,
  );
  expect(screen.queryByRole("alert")).toBeNull();
});
