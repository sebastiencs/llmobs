import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { JsonView } from "@/components/payload/json-view";

const NESTED = {
  shown: { inner: { leaf: "needle" } },
  hidden: { mid: { secret: "no" } },
};

it("collapses past the default depth and expands one node", () => {
  render(<JsonView value={NESTED} />);
  expect(screen.queryByText("needle")).not.toBeInTheDocument();
  expect(screen.getAllByText("{…} 1 key")).toHaveLength(2);
  fireEvent.click(screen.getByRole("button", { name: "Expand inner" }));
  expect(screen.getByText("needle")).toBeInTheDocument();
  expect(screen.queryByText("no")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Collapse inner" }));
  expect(screen.queryByText("needle")).not.toBeInTheDocument();
});

it("auto-expands ancestors of a search match", () => {
  render(<JsonView value={NESTED} />);
  fireEvent.change(screen.getByRole("textbox", { name: "Search JSON" }), {
    target: { value: "needle" },
  });
  expect(screen.getByText("needle")).toBeInTheDocument();
  expect(screen.queryByText("no")).not.toBeInTheDocument();
  expect(document.querySelector("mark")).toHaveTextContent("needle");
});

it("truncates a long string until it is expanded", () => {
  const text = "a".repeat(250);
  render(<JsonView value={text} />);
  const button = screen.getByRole("button", { name: /250 chars/ });
  expect(button).toHaveTextContent(`${"a".repeat(200)}… (250 chars)`);
  expect(screen.queryByText(text)).not.toBeInTheDocument();
  fireEvent.click(button);
  expect(screen.getByText(text)).toBeInTheDocument();
});

it("renders a media reference as a chip", () => {
  render(<JsonView value="media://abc123" />);
  expect(screen.getByText("media://abc123")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /chars/ })).not.toBeInTheDocument();
});
