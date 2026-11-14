import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { afterEach, expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { ProjectProvider } from "@/components/shell/project-context";
import { DELETION_POLL_MS, setDeletionPollMs } from "@/lib/deletions";
import { server } from "@/mocks/node";
import deletionsFixture from "../../../../fixtures/api/static/deletions.json";
import projectFixture from "../../../../fixtures/api/static/project.json";
import { DeletionHistory } from "./deletion-history";

afterEach(() => {
  setDeletionPollMs(DELETION_POLL_MS);
});

it("keeps the rows visible when a refetch fails", async () => {
  // The first answer has an open deletion, so the history polls.
  let gets = 0;
  server.use(
    http.get("/api/v1/projects/:project/deletions", () => {
      gets += 1;
      return gets === 1
        ? HttpResponse.json(deletionsFixture)
        : HttpResponse.json(
            { type: "about:blank", title: "Internal Server Error", status: 500, code: "internal" },
            { status: 500 },
          );
    }),
  );
  setDeletionPollMs(20);
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
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
        <DeletionHistory />
      </ProjectProvider>
    </QueryClientProvider>,
  );

  const table = await screen.findByRole("table", { name: "Deletions" });
  expect(within(table).getByText("user user-3")).toBeInTheDocument();
  await waitFor(() => expect(gets).toBeGreaterThanOrEqual(2));
  expect(await screen.findByRole("alert")).toHaveTextContent("Internal Server Error");
  expect(screen.getByRole("table", { name: "Deletions" })).toBeInTheDocument();
  expect(within(table).getByText("user user-3")).toBeInTheDocument();
});
