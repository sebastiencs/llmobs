import { CompletionContext, type CompletionSource } from "@codemirror/autocomplete";
import { ensureSyntaxTree } from "@codemirror/language";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { SqlEditor, sqlLanguage, sqlNamespace } from "@/components/sql/sql-editor";
import schemaFixture from "../../../../fixtures/api/static/schema.json";

const schemaDocument = schemaFixture as Schema<"SchemaResponse">;

async function labelsAt(
  state: EditorState,
  pos: number,
): Promise<{ label: string; detail?: string }[]> {
  ensureSyntaxTree(state, state.doc.length, 1000);
  const sources = state.languageDataAt<CompletionSource>("autocomplete", pos);
  const results = await Promise.all(
    sources.map((source) => source(new CompletionContext(state, pos, true))),
  );
  return results.flatMap((result) => result?.options ?? []);
}

function Harness({
  initial = "SELECT 1",
  schema,
  errorRange = null,
}: {
  initial?: string;
  schema?: Schema<"SchemaResponse">;
  errorRange?: { from: number; to: number } | null;
}) {
  const [value, setValue] = useState(initial);
  return (
    <>
      <button type="button" onClick={() => setValue("SELECT 2")}>
        Load
      </button>
      <SqlEditor
        value={value}
        onChange={setValue}
        onRun={() => {}}
        errorRange={errorRange}
        schema={schema}
        invalid={false}
      />
      <button type="button">After</button>
      <output aria-label="Value">{value}</output>
    </>
  );
}

it("builds the completion namespace from the schema views", () => {
  const namespace = sqlNamespace(schemaDocument);
  expect(Object.keys(namespace)).toEqual(["traces", "spans", "sessions", "scores"]);
  expect(namespace).toHaveProperty("traces.0", {
    label: "trace_id",
    type: "property",
    detail: "string",
    info: schemaDocument.views[0]?.columns[0]?.description,
  });
  expect(sqlNamespace(undefined)).toEqual({});
});

it("completes tables only when a schema is loaded", async () => {
  const doc = "SELECT * FROM tr";
  const bare = EditorState.create({ doc, extensions: [sqlLanguage(undefined)] });
  expect((await labelsAt(bare, doc.length)).map((option) => option.label)).not.toContain("traces");
  const withSchema = EditorState.create({ doc, extensions: [sqlLanguage(schemaDocument)] });
  expect((await labelsAt(withSchema, doc.length)).map((option) => option.label)).toContain(
    "traces",
  );
});

it("completes columns after a table name", async () => {
  const doc = "SELECT traces.tr";
  const state = EditorState.create({ doc, extensions: [sqlLanguage(schemaDocument)] });
  expect(await labelsAt(state, doc.length)).toEqual(
    expect.arrayContaining([expect.objectContaining({ label: "trace_id", detail: "string" })]),
  );
});

it("reconfigures completions when the schema arrives", async () => {
  const { rerender } = render(<Harness />);
  const host = document.querySelector(".cm-editor");
  const view = host instanceof HTMLElement ? EditorView.findFromDOM(host) : null;
  expect(view).not.toBeNull();
  if (!view) {
    throw new Error("editor did not mount");
  }
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: "SELECT * FROM tr" } });
  expect((await labelsAt(view.state, view.state.doc.length)).map((o) => o.label)).not.toContain(
    "traces",
  );
  rerender(<Harness schema={schemaDocument} />);
  await waitFor(async () => {
    expect((await labelsAt(view.state, view.state.doc.length)).map((o) => o.label)).toContain(
      "traces",
    );
  });
});

it("syncs an external value and keeps typed keystrokes", async () => {
  render(<Harness />);
  await waitFor(() => {
    expect(document.querySelector(".cm-content")?.textContent).toBe("SELECT 1");
  });
  await userEvent.setup().click(screen.getByRole("button", { name: "Load" }));
  await waitFor(() => {
    expect(document.querySelector(".cm-content")?.textContent).toBe("SELECT 2");
  });
  const user = userEvent.setup();
  const content = document.querySelector(".cm-content");
  if (!(content instanceof HTMLElement)) {
    throw new Error("SQL editor did not mount");
  }
  content.focus();
  await user.keyboard("{Control>}{End}{/Control}345");
  expect(document.querySelector(".cm-content")?.textContent).toBe("SELECT 2345");
  expect(screen.getByLabelText("Value").textContent).toBe("SELECT 2345");
});

it("draws nothing for an error range past the end", () => {
  render(<Harness errorRange={{ from: 50, to: 60 }} />);
  expect(document.querySelector(".cm-sql-error")).toBeNull();
});

it("clamps an error range to the end of the text", async () => {
  render(<Harness errorRange={{ from: 2, to: 50 }} />);
  await waitFor(() => {
    expect(document.querySelector(".cm-sql-error")?.textContent).toBe("LECT 1");
  });
});

it("lets Tab leave the editor", async () => {
  render(<Harness />);
  const user = userEvent.setup();
  const content = document.querySelector(".cm-content");
  if (!(content instanceof HTMLElement)) {
    throw new Error("SQL editor did not mount");
  }
  content.focus();
  await user.tab();
  expect(screen.getByRole("button", { name: "After" })).toHaveFocus();
});
