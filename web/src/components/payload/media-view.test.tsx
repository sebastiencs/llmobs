import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { MediaView } from "@/components/payload/media-view";

it("shows a dropped chip with the size in megabytes", () => {
  render(
    <MediaView
      part={{ type: "uri", uri: "llmobs:dropped", reason: "too_large", size: 32_000_000 }}
      projectRef="acme/bot"
    />,
  );
  expect(screen.getByText("Media dropped at ingest (too large, 32 MB)")).toBeInTheDocument();
});

it("does not load an external url until Load is clicked", () => {
  render(
    <MediaView
      part={{
        type: "uri",
        uri: "https://example.com/a.png",
        modality: "image",
        mime_type: "image/png",
      }}
      projectRef="acme/bot"
    />,
  );
  expect(document.querySelector("img")).toBeNull();
  expect(screen.getByText("https://example.com/a.png")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Load" }));
  expect(document.querySelector("img")).toHaveAttribute("src", "https://example.com/a.png");
});

it("renders a blob image from a data url without showing the base64", () => {
  render(
    <MediaView
      part={{ type: "blob", content: "AAAA", mime_type: "image/png", modality: "image" }}
      projectRef="acme/bot"
    />,
  );
  expect(document.querySelector("img")).toHaveAttribute("src", "data:image/png;base64,AAAA");
  expect(screen.queryByText("AAAA")).not.toBeInTheDocument();
});
