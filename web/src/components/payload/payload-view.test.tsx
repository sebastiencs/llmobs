import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, expect, it } from "vitest";
import { PAYLOAD_FORMAT_KEY } from "@/components/payload/format-switch";
import { PayloadView } from "@/components/payload/payload-view";

beforeEach(() => {
  localStorage.removeItem(PAYLOAD_FORMAT_KEY);
});

it("remembers the format switch", () => {
  const payload = { format: "json" as const, value: { order_id: "1234" } };
  const first = render(<PayloadView label="Input" payload={payload} />);
  expect(screen.getByText("order_id")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Raw" }));
  expect(screen.getByText('{"order_id":"1234"}')).toBeInTheDocument();
  first.unmount();
  render(<PayloadView label="Input" payload={payload} />);
  expect(screen.getByText('{"order_id":"1234"}')).toBeInTheDocument();
  expect(screen.queryByText("order_id")).not.toBeInTheDocument();
});

it("keeps two payload views on the same format", () => {
  render(
    <>
      <PayloadView label="Input" payload={{ format: "json", value: { a: 1 } }} />
      <PayloadView label="Output" payload={{ format: "json", value: { b: 2 } }} />
    </>,
  );
  const raw = screen.getAllByRole("button", { name: "Raw" });
  const first = raw[0];
  if (!first) {
    throw new Error("missing raw switch");
  }
  fireEvent.click(first);
  expect(screen.getByText('{"a":1}')).toBeInTheDocument();
  expect(screen.getByText('{"b":2}')).toBeInTheDocument();
});

it("shows a notice above 1 MB instead of rendering pretty", () => {
  const value = "x".repeat(1_000_001);
  render(<PayloadView label="Input" payload={{ format: "text", value }} />);
  expect(screen.getByText("Large payload (1.0 MB)")).toBeInTheDocument();
  expect(screen.queryByText(value)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Render anyway" }));
  expect(screen.getByText(value)).toBeInTheDocument();
});

it("lists message roles and text in the pretty view", () => {
  render(
    <PayloadView
      label="Input"
      payload={{
        format: "messages",
        value: [
          {
            role: "user",
            parts: [{ type: "text", content: "Where is order 1234?" }],
          },
        ],
      }}
    />,
  );
  expect(screen.getByText("user")).toBeInTheDocument();
  expect(screen.getByText("Where is order 1234?")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "JSON" }));
  expect(screen.getByRole("button", { name: "Expand parts" })).toBeInTheDocument();
  expect(screen.queryByText("Where is order 1234?")).not.toBeInTheDocument();
});
