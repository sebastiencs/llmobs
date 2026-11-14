import { expect, it, vi } from "vitest";
import { readSchemaPanelOpen, writeSchemaPanelOpen } from "@/lib/sql/schema-panel";

it("is open with nothing stored and stores the closed choice", () => {
  expect(readSchemaPanelOpen()).toBe(true);
  writeSchemaPanelOpen(false);
  expect(localStorage.getItem("llmobs.sql.schema")).toBe("closed");
  expect(readSchemaPanelOpen()).toBe(false);
});

it("survives a localStorage that throws", () => {
  const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new Error("denied");
  });
  try {
    expect(readSchemaPanelOpen()).toBe(true);
  } finally {
    getItem.mockRestore();
  }
  const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new Error("denied");
  });
  try {
    expect(() => writeSchemaPanelOpen(false)).not.toThrow();
  } finally {
    setItem.mockRestore();
  }
});
