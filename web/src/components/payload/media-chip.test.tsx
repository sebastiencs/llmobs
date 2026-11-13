import { render, screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { MediaChip } from "@/components/payload/media-chip";

it("links a stored file through the API", () => {
  render(
    <MediaChip uri="media://497790947d466676" mimeType="application/pdf" projectRef="acme/bot" />,
  );
  expect(screen.getByRole("link")).toHaveAttribute(
    "href",
    "/api/v1/projects/acme%2Fbot/media/497790947d466676",
  );
});

it("links an external file, opened only by the click", () => {
  render(
    <MediaChip uri="HTTPS://example.com/a.pdf" mimeType="application/pdf" projectRef="acme/bot" />,
  );
  expect(screen.getByRole("link")).toHaveAttribute("href", "HTTPS://example.com/a.pdf");
});

it.each([
  "javascript:alert(1)",
  "data:text/html,<b>x</b>",
  "data:image/png;base64,AAAA",
  "ftp://example.com/a",
])("does not link %s", (uri) => {
  render(<MediaChip uri={uri} mimeType="application/octet-stream" projectRef="acme/bot" />);
  expect(screen.queryByRole("link")).not.toBeInTheDocument();
});
