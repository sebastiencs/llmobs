import { act, renderHook } from "@testing-library/react";
import { expect, it } from "vitest";
import { DEFAULT_QUERY, readDraft, useSqlDraft, writeDraft } from "@/lib/sql/draft";

const V = "01900000-0000-7000-8000-000000000211";

it("reads the default when nothing is stored", () => {
  expect(readDraft("acme/support-bot")).toEqual({ query: DEFAULT_QUERY, params: {} });
});

it("stores one draft per project", () => {
  writeDraft("acme/support-bot", { query: "SELECT 1", params: {} });
  expect(localStorage.getItem("llmobs.sql.draft.acme/support-bot")).toBe(
    '{"query":"SELECT 1","params":{}}',
  );
  expect(readDraft("acme/support-bot")).toEqual({ query: "SELECT 1", params: {} });
  expect(readDraft("acme/playground")).toEqual({ query: DEFAULT_QUERY, params: {} });
});

it("reads a stored draft without parameters as having none", () => {
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", '{"query":"SELECT 1"}');
  expect(readDraft("acme/support-bot")).toEqual({ query: "SELECT 1", params: {} });
});

it("reads a malformed stored draft as the default", () => {
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", "not json");
  expect(readDraft("acme/support-bot")).toEqual({ query: DEFAULT_QUERY, params: {} });
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", '{"query":3}');
  expect(readDraft("acme/support-bot")).toEqual({ query: DEFAULT_QUERY, params: {} });
});

it("keeps string parameter values and the saved query id", () => {
  localStorage.setItem(
    "llmobs.sql.draft.acme/support-bot",
    '{"query":"Q","params":{"a":"1","b":2},"view":"V"}',
  );
  expect(readDraft("acme/support-bot")).toEqual({ query: "Q", params: { a: "1" }, view: "V" });
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", `{"query":"Q","view":${1}}`);
  expect(readDraft("acme/support-bot")).toEqual({ query: "Q", params: {} });
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", '{"query":"Q","params":[]}');
  expect(readDraft("acme/support-bot")).toEqual({ query: "Q", params: {} });
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", `{"query":"Q","view":"${V}"}`);
  expect(readDraft("acme/support-bot")).toEqual({ query: "Q", params: {}, view: V });
});

it("applies two updates in one act and writes the result through", () => {
  localStorage.setItem("llmobs.sql.draft.acme/support-bot", '{"query":"","params":{}}');
  const { result } = renderHook(() => useSqlDraft("acme/support-bot"));
  act(() => {
    result.current[1]((draft) => ({ ...draft, query: `${draft.query}A` }));
    result.current[1]((draft) => ({ ...draft, query: `${draft.query}A` }));
  });
  expect(result.current[0]).toEqual({ query: "AA", params: {} });
  expect(localStorage.getItem("llmobs.sql.draft.acme/support-bot")).toBe(
    '{"query":"AA","params":{}}',
  );
});
