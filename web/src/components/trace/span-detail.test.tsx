import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { PayloadBlock } from "@/components/trace/span-detail";

it("pretty-prints json and messages", () => {
  render(<PayloadBlock label="Input" payload={{ format: "json", value: { order_id: "1234" } }} />);
  expect(screen.getByText(/"order_id": "1234"/)).toBeInTheDocument();
});

it("holds a payload over 1 MB behind a show raw button", () => {
  const value = "x".repeat(1_000_001);
  render(<PayloadBlock label="Input" payload={{ format: "text", value }} />);
  expect(screen.queryByText(value)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Show raw" }));
  expect(screen.getByText(value)).toBeInTheDocument();
});
