import { act, renderHook } from "@testing-library/react";
import { expect, it } from "vitest";
import { DEFAULT_QUERY, readDraft, useSqlDraft, writeDraft } from "@/lib/sql/draft";

it("reads the default when nothing is stored", () => {
  expect(readDraft("acme/support-bot")).toEqual({ query: DEFAULT_QUERY });
});

it("stores one draft per project", () => {
  writeDraft("acme/support-bot", { query: "SELECT 1" });
  expect(localStorage.getItem("llmobs.sql.draft.acme/support-bot")).toBe('{"query":"SELECT 1"}');
  expect(readDraft("acme/support-bot")).toEqual({ query: "SELECT 1" });
  expect(readDraft("acme/playground")).toEqual({ query: DEFAULT_QUERY });
});

it("reads a malformed stored draft as the default", () => {
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", "not json");
  expect(readDraft("acme/support-bot")).toEqual({ query: DEFAULT_QUERY });
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", '{"query":3}');
  expect(readDraft("acme/support-bot")).toEqual({ query: DEFAULT_QUERY });
});

it("applies two updates in one act and writes the result through", () => {
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", '{"query":""}');
  const { result } = renderHook(() => useSqlDraft("acme/support-bot"));
  act(() => {
    result.current[1]((draft) => ({ query: `${draft.query}A` }));
    result.current[1]((draft) => ({ query: `${draft.query}A` }));
  });
  expect(result.current[0]).toEqual({ query: "AA" });
  expect(localStorage.getItem("llmobs.sql.draft.acme/support-bot")).toBe('{"query":"AA"}');
});
