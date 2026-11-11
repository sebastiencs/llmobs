import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CopyButton } from "@/components/copy-button";

describe("CopyButton", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("writes the value and shows a check for 1.5s", async () => {
    vi.useFakeTimers();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });

    render(<CopyButton value="abc" label="trace ID" />);
    const button = screen.getByRole("button", { name: "Copy trace ID" });
    fireEvent.click(button);
    await act(async () => {
      await Promise.resolve();
    });

    expect(writeText).toHaveBeenCalledWith("abc");
    expect(button.querySelector(".lucide-check")).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1_500);
    });
    expect(button.querySelector(".lucide-copy")).toBeInTheDocument();
  });
});
