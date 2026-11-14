import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import { projectListOptions, traceDetailQuery } from "@/api/queries";
import { traceListOptions } from "@/components/data-table/use-infinite-list";
import {
  count,
  deletionErrorLabel,
  deletionStatusLabel,
  deletionSubject,
  invalidateProjectReads,
  isOpenDeletion,
} from "./deletions";

const base = {
  id: "01900000-0000-7000-8000-000000000900",
  org_id: "01900000-0000-7000-8000-000000000010",
  created_at: "2026-01-01T11:00:00.000000000Z",
  requested_by: { kind: "user" as const, id: "01900000-0000-7000-8000-000000000100" },
  status: "applied" as const,
};

describe("count", () => {
  it("pluralises everything but one", () => {
    expect(count(1, "trace")).toBe("1 trace");
    expect(count(3, "trace")).toBe("3 traces");
    expect(count(0, "trace")).toBe("0 traces");
  });
});

describe("deletionSubject", () => {
  it("names the user", () => {
    expect(deletionSubject({ ...base, kind: "user", user_id: "user-3" })).toBe("user user-3");
  });

  it("counts the requested traces", () => {
    expect(deletionSubject({ ...base, kind: "trace_ids", requested_traces: 1 })).toBe("1 trace");
    expect(deletionSubject({ ...base, kind: "trace_ids", requested_traces: 4 })).toBe("4 traces");
  });

  const range = {
    from: "2026-01-01T00:00:00.000000000Z",
    to: "2026-01-02T00:00:00.000000000Z",
    include_eval: false,
    include_experiments: false,
  };

  it("shows a filter's text, with its trace count when known", () => {
    const filter = { ...range, filter: "status = 'error'" };
    expect(deletionSubject({ ...base, kind: "filter", filter, trace_count: 1 })).toBe(
      "status = 'error' (1 trace)",
    );
    expect(deletionSubject({ ...base, kind: "filter", filter })).toBe("status = 'error'");
  });

  it("shows a search when there is no filter text", () => {
    const filter = { ...range, search: "timeout" };
    expect(deletionSubject({ ...base, kind: "filter", filter, trace_count: 2 })).toBe(
      'search "timeout" (2 traces)',
    );
    expect(deletionSubject({ ...base, kind: "filter", filter })).toBe('search "timeout"');
  });

  it("says all traces when there is neither text nor search", () => {
    expect(deletionSubject({ ...base, kind: "filter", filter: range, trace_count: 0 })).toBe(
      "all traces (0 traces)",
    );
    expect(deletionSubject({ ...base, kind: "filter", filter: range })).toBe("all traces");
  });

  it("names a dropped project or organization by its name and slug", () => {
    expect(
      deletionSubject({ ...base, kind: "project", name: "Support bot", slug: "support-bot" }),
    ).toBe("Support bot (support-bot)");
    expect(deletionSubject({ ...base, kind: "org", slug: "acme" })).toBe("acme (acme)");
    expect(deletionSubject({ ...base, kind: "org" })).toBe("unknown (?)");
  });
});

describe("labels", () => {
  it("labels every status", () => {
    expect(deletionStatusLabel("resolving")).toBe("Finding data");
    expect(deletionStatusLabel("pending")).toBe("Hidden, erasing");
    expect(deletionStatusLabel("applied")).toBe("Erased");
    expect(deletionStatusLabel("failed")).toBe("Failed");
  });

  it("labels every failure", () => {
    expect(deletionErrorLabel("too_many_traces")).toBe("Too many traces matched (over 100,000)");
    expect(deletionErrorLabel("resolution_failed")).toBe("Could not find the data to delete");
  });
});

describe("isOpenDeletion", () => {
  it("is true only while resolving or pending", () => {
    expect(isOpenDeletion({ ...base, kind: "user", status: "resolving" })).toBe(true);
    expect(isOpenDeletion({ ...base, kind: "user", status: "pending" })).toBe(true);
    expect(isOpenDeletion({ ...base, kind: "user", status: "applied" })).toBe(false);
    expect(isOpenDeletion({ ...base, kind: "user", status: "failed" })).toBe(false);
  });
});

it("invalidates the project's reads and leaves other queries alone", async () => {
  const queryClient = new QueryClient();
  const range = { from: "2026-01-01T00:00:00Z", to: "2026-01-02T00:00:00Z" };
  const traceList = traceListOptions("acme", "support-bot", range).queryKey;
  const traceDetail = traceDetailQuery("acme", "support-bot", "a".repeat(32)).queryKey;
  const orgProjects = projectListOptions("acme").queryKey;
  queryClient.setQueryData(traceList, { pages: [], pageParams: [] });
  queryClient.setQueryData(traceDetail, { trace: {} });
  queryClient.setQueryData(orgProjects, { data: [] });

  await invalidateProjectReads(queryClient);

  expect(queryClient.getQueryState(traceList)?.isInvalidated).toBe(true);
  expect(queryClient.getQueryState(traceDetail)?.isInvalidated).toBe(true);
  expect(queryClient.getQueryState(orgProjects)?.isInvalidated).toBe(false);
});
