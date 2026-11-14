import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it, vi } from "vitest";
import { traceDetailQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { ProjectProvider } from "@/components/shell/project-context";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import deletionFixture from "../../../../fixtures/api/static/deletion.json";
import projectFixture from "../../../../fixtures/api/static/project.json";
import { DeleteMatchingDialog, type TraceSelection } from "./delete-matching-dialog";

const SELECTION: TraceSelection = {
  filter: "name = 'x'",
  from: "2026-01-01T00:00:00Z",
  to: "2026-01-02T00:00:00Z",
  includeEval: false,
  includeExperiments: false,
};

const PREVIEW = "/api/v1/projects/:project/deletions/preview";
const DELETE = "/api/v1/projects/:project/deletions";
const CAPPED = "More than 100,000 traces match. Narrow the filter or the time range.";

function renderDialog(selection: Partial<TraceSelection> = {}) {
  const onOpenChange = vi.fn();
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  render(
    <QueryClientProvider client={queryClient}>
      <ProjectProvider
        value={{
          org: "acme",
          project: "support-bot",
          ref: "acme/support-bot",
          data: projectFixture as Schema<"Project">,
        }}
      >
        <DeleteMatchingDialog
          open
          onOpenChange={onOpenChange}
          selection={{ ...SELECTION, ...selection }}
        />
      </ProjectProvider>
    </QueryClientProvider>,
  );
  return { onOpenChange, queryClient };
}

it("counts the matches, then deletes them with the selection's filter and range", async () => {
  let body: unknown;
  server.use(
    http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 3, capped: false })),
    http.post(DELETE, async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(deletionFixture, { status: 202 });
    }),
  );
  const user = userEvent.setup();
  const { onOpenChange } = renderDialog();

  expect(await screen.findByText(/^3 traces match/)).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Delete 3 traces" }));

  await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
  expect(body).toEqual({
    kind: "filter",
    filter: "name = 'x'",
    from: SELECTION.from,
    to: SELECTION.to,
    include_eval: false,
    include_experiments: false,
  });
});

it("uses the singular for one matching trace", async () => {
  server.use(http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 1, capped: false })));
  renderDialog();

  expect(await screen.findByText(/^1 trace matches/)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Delete 1 trace" })).toBeInTheDocument();
});

it("refuses to delete more than 100,000 matches", async () => {
  server.use(
    http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 100_000, capped: true })),
  );
  renderDialog();

  expect(await screen.findByRole("alert")).toHaveTextContent(CAPPED);
  expect(screen.queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
});

it("says so when nothing matches", async () => {
  server.use(http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 0, capped: false })));
  renderDialog();

  expect(await screen.findByText("No trace matches. Nothing to delete.")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
});

it("explains an invalid filter", async () => {
  server.use(
    http.post(PREVIEW, () => problemResponse(400, "invalid_filter", { detail: "unknown column" })),
  );
  renderDialog();

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The filter is not valid. Fix it on the list first.",
  );
  expect(screen.queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
});

it("shows the narrow-it message when the deletion is refused as too large", async () => {
  server.use(
    http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 3, capped: false })),
    http.post(DELETE, () => problemResponse(422, "too_many_traces")),
  );
  const user = userEvent.setup();
  renderDialog();

  await user.click(await screen.findByRole("button", { name: "Delete 3 traces" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(CAPPED);
  expect(screen.queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
});

it("shows the counting text before the preview answers", () => {
  server.use(http.post(PREVIEW, () => new Promise<Response>(() => undefined)));
  renderDialog();

  expect(screen.getByText("Counting matching traces…")).toHaveAttribute("aria-live", "polite");
});

it("keeps the count in a polite live region", async () => {
  server.use(http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 3, capped: false })));
  renderDialog();

  expect(await screen.findByText(/^3 traces match/)).toHaveAttribute("aria-live", "polite");
});

it("explains a failed preview", async () => {
  server.use(http.post(PREVIEW, () => HttpResponse.json({}, { status: 500 })));
  renderDialog();

  expect(await screen.findByRole("alert")).toHaveTextContent("Could not count the traces.");
  expect(screen.queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
});

it("explains a failed deletion request", async () => {
  server.use(
    http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 3, capped: false })),
    http.post(DELETE, () => HttpResponse.json({}, { status: 500 })),
  );
  const user = userEvent.setup();
  renderDialog();

  await user.click(await screen.findByRole("button", { name: "Delete 3 traces" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Could not request the deletion.");
});

it("does not close on Escape while the deletion request runs", async () => {
  let release: () => void = () => undefined;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  server.use(
    http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 3, capped: false })),
    http.post(DELETE, async () => {
      await gate;
      return HttpResponse.json(deletionFixture, { status: 202 });
    }),
  );
  const user = userEvent.setup();
  const { onOpenChange } = renderDialog();

  await user.click(await screen.findByRole("button", { name: "Delete 3 traces" }));
  await user.keyboard("{Escape}");
  expect(onOpenChange).not.toHaveBeenCalledWith(false);

  release();
  await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
});

it("marks the project's reads stale after a deletion", async () => {
  server.use(
    http.post(PREVIEW, () => HttpResponse.json({ matched_traces: 3, capped: false })),
    http.post(DELETE, () => HttpResponse.json(deletionFixture, { status: 202 })),
  );
  const user = userEvent.setup();
  const { onOpenChange, queryClient } = renderDialog();
  const read = traceDetailQuery("acme", "support-bot", "a".repeat(32)).queryKey;
  queryClient.setQueryData(read, {});

  await user.click(await screen.findByRole("button", { name: "Delete 3 traces" }));

  await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
  expect(queryClient.getQueryState(read)?.isInvalidated).toBe(true);
});
