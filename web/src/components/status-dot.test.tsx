import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { StatusDot } from "@/components/status-dot";

describe("StatusDot", () => {
  it("names the status and gives an error a ring", () => {
    const { rerender } = render(<StatusDot status="ok" />);
    expect(screen.getByText("ok")).toBeInTheDocument();

    rerender(<StatusDot status="error" />);
    expect(screen.getByText("error")).toBeInTheDocument();
    expect(document.querySelector(".ring-status-error")).toBeInTheDocument();

    rerender(<StatusDot status="unset" />);
    expect(screen.getByText("unset")).toBeInTheDocument();
  });
});
